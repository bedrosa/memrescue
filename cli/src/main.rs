//! memrescue-cli: acceso al motor de MemRescue sin interfaz gráfica.
//!
//! Todas las operaciones son de solo lectura y solo aceptan ficheros de imagen.

use mr_api::task::TaskState;
use mr_api::{HashOptions, ImageInfo, OpenOptions, Source, VerifyOptions};
use mr_common::HashAlgorithm;
use mr_io::resilient::{OnError, ReadPolicy};
use mr_io::SectorState;
use mr_store::Store;
use std::path::PathBuf;
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

const USAGE: &str = "\
memrescue-cli: motor de MemRescue

USO:
  memrescue-cli version
  memrescue-cli probe <fichero>
  memrescue-cli image-info <imagen> [ORIGEN] [--json]
  memrescue-cli split-list <imagen.001>
  memrescue-cli sectors <imagen> --lba N [--count M] [--retries N] [--jump N] [--read-back]
                        [--bad-fill PATRÓN] [--skip-fill PATRÓN] [ORIGEN]
  memrescue-cli verify <imagen> [--retries N] [--jump N] [--read-back]
                       [--skip-codes 23,1117] [--ignore-errors] [ORIGEN] [--json]
  memrescue-cli hash <imagen> [--algo crc32,md5,sha1,sha256] [ORIGEN] [--db trabajo.mrdb] [--json]
  memrescue-cli db-init <trabajo.mrdb>
  memrescue-cli jobs <trabajo.mrdb>

ORIGEN (opcional):
  --sector-size N    tamaño de sector lógico (por defecto, el nativo del formato; 512 en RAW)
  --force-raw        leer como volcado sector a sector aunque parezca un contenedor
  --tolerant         modo tolerante para VHD/VHDX/VMDK con tablas de asignación dañadas: los
                     bloques con entradas inválidas o ambiguas se descartan y se tratan como
                     ilegibles (patrón de relleno, nunca datos); sin esta opción se rechazan
  --split            abrir la imagen dividida que empieza en <imagen.001> (ver split-list)
  --range INI:NUM    trabajar solo con NUM sectores desde el LBA INI (partición o rango)

POLÍTICA DE ERRORES (verify):
  --skip-codes LISTA códigos de error del sistema que no se reintentan (p. ej. 23,1117)
  --ignore-errors    ignorar todos los errores: ningún sector se reintenta
  Los sectores afectados quedan ILEGIBLES (patrón de relleno), nunca como datos.

PATRONES DE RELLENO (sectors):
  --bad-fill P       relleno de los sectores ilegibles (por defecto BAD!)
  --skip-fill P      relleno de los sectores saltados con --jump (por defecto SKIP)
  P: 4 caracteres ASCII (BAD!) u 8 dígitos hexadecimales (DEADBEEF). No se admiten patrones con
  los cuatro bytes iguales (00000000 se confundiría con sectores dispersos) ni el mismo patrón
  para ambos. El relleno nunca son datos: cada sector indica su estado.

Formatos: RAW, VHD (fijo/dinámico), VHDX (fijo/dinámico), VMDK monolithicSparse e imágenes
divididas. Tamaños de sector habituales: 512, 1024, 2048, 4096.
Códigos de salida: 0 correcto, 1 error de operación, 2 uso incorrecto,
3 hay sectores no fiables (ilegibles o saltados; su contenido es un patrón de relleno).
";

struct Args {
    positional: Vec<String>,
    sector_size: u32,
    algos: Vec<HashAlgorithm>,
    db: Option<PathBuf>,
    json: bool,
    lba: Option<u64>,
    count: u64,
    retries: u32,
    force_raw: bool,
    tolerant: bool,
    split: bool,
    range: Option<(u64, u64)>,
    jump: u64,
    read_back: bool,
    /// Política de omisión de errores (`verify`).
    skip_codes: Vec<i32>,
    ignore_errors: bool,
    /// Patrones de relleno (`sectors`); `None` = por defecto.
    bad_fill: Option<[u8; 4]>,
    skip_fill: Option<[u8; 4]>,
}

/// Analiza `--skip-codes`: códigos positivos separados por comas (decimal o `0x…`), sin
/// duplicados y como máximo 256.
fn parse_skip_codes(v: &str) -> Result<Vec<i32>, String> {
    let mut codes = Vec::new();
    for part in v.split(',').map(str::trim).filter(|x| !x.is_empty()) {
        let n = match part.strip_prefix("0x").or_else(|| part.strip_prefix("0X")) {
            Some(h) => i32::from_str_radix(h, 16).ok(),
            None => part.parse::<i32>().ok(),
        };
        match n {
            Some(c) if c > 0 => codes.push(c),
            _ => {
                return Err(format!(
                    "código de error no válido en --skip-codes: {part} (debe ser positivo)"
                ))
            }
        }
    }
    codes.sort_unstable();
    codes.dedup();
    if codes.is_empty() {
        return Err("--skip-codes necesita al menos un código".into());
    }
    if codes.len() > 256 {
        return Err("--skip-codes admite como máximo 256 códigos".into());
    }
    Ok(codes)
}

fn num<T: std::str::FromStr>(
    it: &mut impl Iterator<Item = String>,
    name: &str,
) -> Result<T, String> {
    let v = it.next().ok_or(format!("falta el valor de {name}"))?;
    v.parse()
        .map_err(|_| format!("valor no válido para {name}: {v}"))
}

fn parse(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut a = Args {
        positional: vec![],
        sector_size: 0,
        algos: vec![HashAlgorithm::Sha256],
        db: None,
        json: false,
        lba: None,
        count: 1,
        retries: 1,
        force_raw: false,
        tolerant: false,
        split: false,
        range: None,
        jump: 0,
        read_back: false,
        skip_codes: Vec::new(),
        ignore_errors: false,
        bad_fill: None,
        skip_fill: None,
    };
    while let Some(s) = it.next() {
        match s.as_str() {
            "--sector-size" => a.sector_size = num(&mut it, "--sector-size")?,
            "--lba" => a.lba = Some(num(&mut it, "--lba")?),
            "--count" => a.count = num(&mut it, "--count")?,
            "--retries" => a.retries = num(&mut it, "--retries")?,
            "--jump" => a.jump = num(&mut it, "--jump")?,
            "--read-back" => a.read_back = true,
            "--skip-codes" => {
                let v = it.next().ok_or("falta el valor de --skip-codes")?;
                a.skip_codes = parse_skip_codes(&v)?;
            }
            "--ignore-errors" => a.ignore_errors = true,
            "--bad-fill" | "--skip-fill" => {
                let v = it.next().ok_or(format!("falta el valor de {s}"))?;
                let p =
                    mr_io::resilient::parse_fill_pattern(&v).map_err(|e| format!("{s}: {e}"))?;
                if s == "--bad-fill" {
                    a.bad_fill = Some(p);
                } else {
                    a.skip_fill = Some(p);
                }
            }
            "--force-raw" => a.force_raw = true,
            "--tolerant" => a.tolerant = true,
            "--split" => a.split = true,
            "--range" => {
                let v = it.next().ok_or("falta el valor de --range")?;
                let (s, c) = v
                    .split_once(':')
                    .ok_or(format!("--range debe ser INI:NUM, no {v}"))?;
                let parse = |x: &str| {
                    x.trim()
                        .parse::<u64>()
                        .map_err(|_| format!("valor no válido para --range: {v}"))
                };
                a.range = Some((parse(s)?, parse(c)?));
            }
            "--algo" => {
                let v = it.next().ok_or("falta el valor de --algo")?;
                a.algos = v
                    .split(',')
                    .map(|x| x.trim().parse::<HashAlgorithm>())
                    .collect::<Result<_, _>>()?;
            }
            "--db" => a.db = Some(it.next().ok_or("falta el valor de --db")?.into()),
            "--json" => a.json = true,
            x if x.starts_with("--") => return Err(format!("opción desconocida: {x}")),
            _ => a.positional.push(s),
        }
    }
    Ok(a)
}

fn json_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// Abre el origen indicado por los argumentos (formato automático, dividida, forzada, rango).
fn open_src(a: &Args, p: &str) -> Result<Source, mr_api::ApiError> {
    let opts = OpenOptions {
        sector_size: a.sector_size,
        force_raw: a.force_raw,
        tolerant: a.tolerant,
    };
    let src = if a.split {
        mr_api::open_split_source(p, opts)?
    } else {
        mr_api::open_with(p, opts)?
    };
    // Modo tolerante (petición explícita): aviso visible en la salida de errores.
    if a.tolerant && src.info().description.contains("modo tolerante") {
        eprintln!(
            "AVISO: contenedor abierto en modo tolerante ({}). Los bloques descartados se tratan              como ilegibles: su contenido es un patrón de relleno, nunca datos.",
            src.info().description
        );
    }
    match a.range {
        Some((start, count)) => mr_api::open_range(&src, start, count),
        None => Ok(src),
    }
}

fn print_info(i: &ImageInfo, json: bool) {
    if json {
        let segs: Vec<String> = i
            .segments
            .iter()
            .map(|s| json_str(&s.display().to_string()))
            .collect();
        println!(
            "{{\"path\":{},\"format\":{},\"description\":{},\"size_bytes\":{},\"sector_size\":{},\"sector_count\":{},\"trailing_bytes\":{},\"segments\":[{}],\"range\":{}}}",
            json_str(&i.path.display().to_string()),
            json_str(i.format.name()),
            json_str(&i.description),
            i.size_bytes,
            i.sector_size,
            i.sector_count,
            i.trailing_bytes,
            segs.join(","),
            match i.range {
                Some((s, c)) => format!("[{s},{c}]"),
                None => "null".into(),
            }
        );
    } else {
        println!("Imagen:          {}", i.path.display());
        println!("Formato:         {}", i.description);
        if !i.segments.is_empty() {
            println!("Segmentos:       {}", i.segments.len());
        }
        println!("Tamaño:          {} bytes", i.size_bytes);
        println!("Tamaño sector:   {} bytes", i.sector_size);
        println!("Sectores:        {}", i.sector_count);
        if i.trailing_bytes > 0 {
            println!(
                "Bytes sobrantes: {} (no completan un sector)",
                i.trailing_bytes
            );
        }
    }
}

/// Volcado hexadecimal: desplazamiento absoluto (hex), 16 bytes por línea y texto ASCII.
fn hexdump(base_offset: u64, data: &[u8]) {
    for (i, line) in data.chunks(16).enumerate() {
        let off = base_offset + (i * 16) as u64;
        let hex: Vec<String> = line.iter().map(|b| format!("{b:02X}")).collect();
        let (l, r) = hex.split_at(hex.len().min(8));
        let ascii: String = line
            .iter()
            .map(|&b| {
                if (0x20..0x7f).contains(&b) {
                    b as char
                } else {
                    '.'
                }
            })
            .collect();
        println!(
            "{off:012X}  {:<23}  {:<23}  {ascii}",
            l.join(" "),
            r.join(" ")
        );
    }
}

fn run(args: Vec<String>) -> Result<(), (u8, String)> {
    let usage = |m: &str| (2u8, format!("{m}\n\n{USAGE}"));
    let mut it = args.into_iter();
    let cmd = it.next().ok_or_else(|| usage("falta el comando"))?;
    let a = parse(it).map_err(|e| usage(&e))?;
    let one = |a: &Args| -> Result<String, (u8, String)> {
        match a.positional.as_slice() {
            [p] => Ok(p.clone()),
            _ => Err(usage("se esperaba exactamente un argumento")),
        }
    };
    let op = |e: &dyn std::fmt::Display| (1u8, e.to_string());
    if cmd != "sectors" && (a.bad_fill.is_some() || a.skip_fill.is_some()) {
        return Err(usage("--bad-fill y --skip-fill solo se aplican a sectors"));
    }

    match cmd.as_str() {
        "version" | "--version" => {
            println!("MemRescue engine {}", mr_api::ENGINE_VERSION);
        }
        "help" | "--help" | "-h" => print!("{USAGE}"),
        "probe" => {
            let p = one(&a)?;
            let f = mr_api::probe_format(&p).map_err(|e| op(&e))?;
            let soporte = if f.is_supported() {
                "soportado"
            } else {
                "todavía no soportado"
            };
            println!("{} ({soporte})", f.name());
        }
        "split-list" => {
            let p = one(&a)?;
            let segs = mr_api::discover_split(&p).map_err(|e| op(&e))?;
            for s in &segs {
                println!("{}", s.display());
            }
            eprintln!(
                "{} segmentos (ábralos con --split; no se ha abierto ninguno)",
                segs.len()
            );
        }
        "sectors" => {
            let p = one(&a)?;
            let lba = a.lba.ok_or_else(|| usage("falta --lba"))?;
            if a.count == 0 || a.count > 256 {
                return Err(usage("--count debe estar entre 1 y 256"));
            }
            if a.retries > 100 {
                return Err(usage("--retries debe estar entre 0 y 100"));
            }
            if a.read_back && a.jump == 0 {
                return Err(usage("--read-back requiere --jump N (N > 0)"));
            }
            let defaults = mr_api::MappedReadOptions::default();
            let opts = mr_api::MappedReadOptions {
                retries: a.retries,
                jump_sectors: a.jump,
                read_back: a.read_back,
                bad_fill: a.bad_fill.unwrap_or(defaults.bad_fill),
                skip_fill: a.skip_fill.unwrap_or(defaults.skip_fill),
            };
            mr_io::resilient::validate_fill_patterns(opts.bad_fill, opts.skip_fill)
                .map_err(|e| usage(&e.to_string()))?;
            let src = open_src(&a, &p).map_err(|e| op(&e))?;
            let ss = src.info().sector_size;
            let (data, states) =
                mr_api::read_sectors_mapped_with(src.as_ref(), lba, a.count, &opts)
                    .map_err(|e| op(&e))?;
            let mut untrusted = 0u64;
            for (i, (sector, st)) in data.chunks(ss as usize).zip(&states).enumerate() {
                let l = lba + i as u64;
                let off = l * u64::from(ss);
                println!(
                    "== LBA {l} (desplazamiento {off} = 0x{off:X}) [{}]",
                    state_label_one(*st)
                );
                if !st.is_trustworthy() {
                    untrusted += 1;
                    println!("!! NO FIABLE: el contenido mostrado es un patrón de relleno, NO datos del origen");
                }
                hexdump(off, sector);
            }
            if untrusted > 0 {
                return Err((3, format!("{untrusted} sectores no fiables")));
            }
        }
        "verify" => {
            let p = one(&a)?;
            if a.retries > 100 {
                return Err(usage("--retries debe estar entre 0 y 100"));
            }
            if a.read_back && a.jump == 0 {
                return Err(usage("--read-back requiere --jump N (N > 0)"));
            }
            let src = open_src(&a, &p).map_err(|e| op(&e))?;
            let opts = VerifyOptions {
                policy: ReadPolicy {
                    retries: a.retries,
                    on_error: OnError::FillAndContinue,
                    jump_sectors: a.jump,
                    read_back: a.read_back,
                    skip_error_codes: a.skip_codes.clone(),
                    ignore_all_errors: a.ignore_errors,
                    ..ReadPolicy::default()
                },
                ..VerifyOptions::default()
            };
            let task = mr_api::start_verify_task(src, opts).map_err(|e| op(&e))?;
            while task.state() == TaskState::Running {
                let (d, t) = task.progress();
                if !a.json && t > 0 {
                    eprint!("\r{:5.1}%  {d}/{t} sectores", d as f64 * 100.0 / t as f64);
                }
                thread::sleep(Duration::from_millis(200));
            }
            if !a.json {
                eprintln!();
            }
            let r = task.wait().map_err(|e| op(&e))?;
            let m = &r.map;
            let ok = m.count(SectorState::Read) + m.count(SectorState::Recovered);
            let bad = m.count(SectorState::Unreadable);
            let untrusted = bad + m.count(SectorState::Skipped);
            if a.json {
                let ranges: Vec<String> = m
                    .untrusted_runs()
                    .filter(|t| t.2 == SectorState::Unreadable)
                    .map(|(l, c, _)| format!("[{l},{c}]"))
                    .collect();
                let untrusted_runs: Vec<String> = m
                    .untrusted_runs()
                    .map(|(l, c, s)| format!("[{l},{c},\"{}\"]", state_name(s)))
                    .collect();
                let states: Vec<String> = SectorState::ALL
                    .iter()
                    .map(|&s| format!("\"{}\":{}", state_name(s), m.count(s)))
                    .collect();
                // Política de errores aplicada y su efecto (IO-09).
                let skip: Vec<String> = a.skip_codes.iter().map(i32::to_string).collect();
                let codes: Vec<String> = r
                    .error_codes
                    .iter()
                    .map(|(c, n)| format!("\"{c}\":{n}"))
                    .collect();
                println!(
                    "{{\"sectors\":{},\"ok\":{ok},\"bad\":{bad},\"bad_ranges\":[{}],\"states\":{{{}}},\"untrusted_ranges\":[{}],\"ranges_truncated\":{},\"error_policy\":{{\"retries\":{},\"skip_codes\":[{}],\"ignore_all\":{}}},\"omitted_errors\":{},\"error_codes\":{{{}}}}}",
                    r.sectors_requested,
                    ranges.join(","),
                    states.join(","),
                    untrusted_runs.join(","),
                    m.truncated(),
                    a.retries,
                    skip.join(","),
                    a.ignore_errors,
                    r.omitted_errors,
                    codes.join(",")
                );
            } else {
                println!(
                    "Sectores fiables (leídos, recuperados o dispersos): {} de {}",
                    r.sectors_requested - untrusted,
                    r.sectors_requested
                );
                for s in SectorState::ALL {
                    println!("  {:<11} {}", state_label(s), m.count(s));
                }
                // Política de errores aplicada y su efecto (IO-09).
                if a.ignore_errors {
                    println!("Política de errores: ignorar todos (sin reintentos)");
                } else if !a.skip_codes.is_empty() {
                    let l: Vec<String> = a.skip_codes.iter().map(i32::to_string).collect();
                    println!(
                        "Política de errores: sin reintentos para los códigos {}",
                        l.join(", ")
                    );
                }
                if a.ignore_errors || !a.skip_codes.is_empty() {
                    println!(
                        "  errores omitidos (ilegibles sin reintentar): {}",
                        r.omitted_errors
                    );
                }
                if !r.error_codes.is_empty() {
                    println!("Códigos de error del sistema en sectores ilegibles:");
                    for (c, n) in &r.error_codes {
                        println!("  código {c}: {n} sectores");
                    }
                }
                if untrusted > 0 {
                    println!("Tramos NO fiables (el contenido es un patrón de relleno, no datos):");
                    for (l, c, s) in m.untrusted_runs() {
                        println!(
                            "  LBA {l} – {} ({c} sectores, {})",
                            l + c - 1,
                            state_label(s)
                        );
                    }
                    if m.truncated() {
                        println!(
                            "  … lista incompleta (demasiados tramos); los recuentos son exactos"
                        );
                    }
                }
            }
            if untrusted > 0 {
                return Err((
                    3,
                    format!("{untrusted} sectores no fiables ({bad} ilegibles)"),
                ));
            }
        }
        "image-info" => {
            let p = one(&a)?;
            let src = open_src(&a, &p).map_err(|e| op(&e))?;
            print_info(src.info(), a.json);
        }
        "hash" => {
            let p = one(&a)?;
            let opts = HashOptions {
                algorithms: a.algos.clone(),
                store_path: a.db.clone(),
                ..HashOptions::default()
            };
            let src = open_src(&a, &p).map_err(|e| op(&e))?;
            let task = mr_api::start_hash_task_on(src, opts).map_err(|e| op(&e))?;
            let start = Instant::now();
            while task.state() == TaskState::Running {
                let (d, t) = task.progress();
                if !a.json && t > 0 {
                    eprint!("\r{:5.1}%  {d}/{t} bytes", d as f64 * 100.0 / t as f64);
                }
                thread::sleep(Duration::from_millis(200));
            }
            if !a.json {
                eprintln!();
            }
            let r = task.wait().map_err(|e| op(&e))?;
            if a.json {
                let pairs: Vec<String> = r
                    .digests
                    .iter()
                    .map(|(al, h)| format!("\"{al}\":\"{h}\""))
                    .collect();
                println!("{{\"bytes\":{},{}}}", r.bytes, pairs.join(","));
            } else {
                for (al, h) in &r.digests {
                    println!("{:<7} {h}", al.name().to_uppercase());
                }
                println!(
                    "{} bytes en {:.2} s",
                    r.bytes,
                    start.elapsed().as_secs_f64()
                );
            }
        }
        "db-init" => {
            let p = one(&a)?;
            let s = Store::open(&p).map_err(|e| op(&e))?;
            println!(
                "Base de datos de trabajo lista: {p} (esquema v{})",
                s.schema_version().map_err(|e| op(&e))?
            );
        }
        "jobs" => {
            let p = one(&a)?;
            let s = Store::open(&p).map_err(|e| op(&e))?;
            let jobs = s.jobs().map_err(|e| op(&e))?;
            if jobs.is_empty() {
                println!("(sin trabajos)");
            }
            for j in jobs {
                println!(
                    "#{:<4} {:<8} {:<11} {}/{} {}",
                    j.id,
                    j.kind,
                    j.state.as_str(),
                    j.progress_done,
                    j.progress_total,
                    j.result_json.or(j.error).unwrap_or_default()
                );
            }
        }
        other => return Err(usage(&format!("comando desconocido: {other}"))),
    }
    Ok(())
}

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, msg)) => {
            eprintln!("error: {msg}");
            ExitCode::from(code)
        }
    }
}

/// Nombre estable (JSON) de un estado de sector.
fn state_name(s: SectorState) -> &'static str {
    match s {
        SectorState::Read => "read",
        SectorState::Recovered => "recovered",
        SectorState::Unreadable => "unreadable",
        SectorState::Skipped => "skipped",
        SectorState::Sparse => "sparse",
    }
}

/// Nombre legible de un único sector.
fn state_label_one(s: SectorState) -> &'static str {
    match s {
        SectorState::Read => "leído",
        SectorState::Recovered => "recuperado tras reintentos",
        SectorState::Unreadable => "ILEGIBLE",
        SectorState::Skipped => "SALTADO (no leído)",
        SectorState::Sparse => "disperso (ceros del formato)",
    }
}

/// Nombre legible de un estado de sector.
fn state_label(s: SectorState) -> &'static str {
    match s {
        SectorState::Read => "leídos",
        SectorState::Recovered => "recuperados",
        SectorState::Unreadable => "ilegibles",
        SectorState::Skipped => "saltados",
        SectorState::Sparse => "dispersos",
    }
}
