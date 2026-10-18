//! memrescue-cli: acceso al motor de MemRescue sin interfaz gráfica.
//!
//! Todas las operaciones son de solo lectura y solo aceptan ficheros de imagen.

mod ntfs_files;

use mr_api::mr_fs_ntfs::{
    BackupState, BootSource, NtfsVolume, RecordOutcome, RecordSource, RecordState,
};
use mr_api::mr_part::gpt::GptCopy;
use mr_api::mr_part::search::{Check, FoundTableKind, FoundVolume, SearchStatus};
use mr_api::mr_part::{Anomaly, Confidence, Origin, PartitionKind, Scheme, Severity};
use mr_api::task::TaskState;
use mr_api::{
    HashOptions, ImageInfo, OpenOptions, Partition, PartitionTable, SearchOptions, SearchResult,
    Source, VerifyOptions,
};
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
  memrescue-cli partitions <imagen> [ORIGEN] [--json]
  memrescue-cli find-partitions <imagen> [ORIGEN] [--json] [--max-reads N]
  memrescue-cli ntfs-info <imagen> [ORIGEN] [--json]
  memrescue-cli ntfs-stat <imagen> --record N [ORIGEN] [--json]
  memrescue-cli ntfs-cat <imagen> --record N [--stream NOMBRE] [-o SALIDA [--overwrite]] [ORIGEN]
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
  --partition N      trabajar solo con la partición N (numeración de «partitions», desde 1);
                     no se combina con --range
  --apply-shift      con --partition en una imagen desplazada (GPT): abrir la partición en
                     LBA del disco - desplazamiento (sin esta opción se rechaza; nunca se aplica
                     en silencio); con --found, abrir un volumen cuya posición depende del
                     desplazamiento detectado por la búsqueda
  --found N          trabajar solo con el volumen N encontrado por la búsqueda exprés
                     (numeración de «find-partitions», desde 1; la búsqueda se repite con las
                     mismas opciones); no se combina con --partition ni con --range

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

PARTICIONES (partitions):
  Lee el MBR y la cadena de EBR (o detecta el MBR protector o híbrido de GPT) sin abortar ante
  sectores dañados. Cada partición lleva su confianza (válida, sospechosa, inválida) y sus
  anomalías; con --json, códigos estables. Con GPT (MBR protector o híbrido, o firma GPT sin MBR)
  se listan las particiones GPT con nombre y GUID de tipo, la copia en uso (primaria o de
  respaldo) y el desplazamiento de la imagen si lo hay (las LBA son las del disco).

BÚSQUEDA DE PARTICIONES PERDIDAS (find-partitions):
  Búsqueda exprés acotada (nunca recorre el disco): sondea las posiciones que declara la tabla,
  las derivadas de lo encontrado, rejillas de alineación (CHS y 1 MiB) al inicio y al final y los
  huecos sin explicar, y reconoce NTFS, FAT12/16/32 y exFAT (principal, copia y estructuras
  básicas) y tablas MBR/EBR o GPT en esas posiciones. Solo se ejecuta a petición.
  --max-reads N      presupuesto de lecturas (por defecto 4096); al agotarse, resultado parcial
  Indicadores: E entrada de tabla, P principal, C copia coherente, S estructuras básicas,
  U inicio declarado coherente, A alineación; + sí, x no o dañada, ? no verificable (sector
  ilegible: nunca se interpreta su relleno), o fuera de la imagen, - no aplica.

VOLUMEN NTFS (ntfs-info):
  Abre el volumen NTFS que empieza en el origen (imagen sin tabla, --partition N o --found N):
  arranque usado (principal o copia del último sector) y estado de la copia, campos del arranque,
  número de serie, etiqueta y versión, ubicación y runs de $MFT, comparación con $MFTMirr, y
  recorre todos los registros de la MFT: en uso, no en uso (borrados o libres), vacíos, dañados
  (firma, fixups o cabecera; nunca se usan) e ilegibles (sector no fiable: nunca se interpretan).
  Las LBA de los sectores ilegibles son relativas al origen abierto (la partición, si la hay).
  Código de salida 3 si algún registro tiene sectores ilegibles.

FICHEROS NTFS (ntfs-stat, ntfs-cat), por número de registro MFT (el árbol de directorios llega
con #22):
  --record N         registro base del fichero (también no en uso: ficheros borrados)
  --stream NOMBRE    flujo alternativo ($DATA con nombre); sin él, el flujo principal
  -o, --output F     fichero de salida de ntfs-cat (sin él, la salida estándar). Se rechazan el
                     propio origen (o uno de sus segmentos), las rutas de dispositivo y los
                     directorios. Se escribe en un temporal del mismo directorio que se renombra
                     al terminar: una copia interrumpida nunca deja un fichero parcial con el
                     nombre F (el temporal se borra). No se detectan enlaces duros al origen.
  --overwrite        sustituir F si ya existe (sin esta opción: código de salida 2)
  ntfs-stat: cabecera, $STANDARD_INFORMATION, $FILE_NAME, atributos (registro en que está cada
  uno, también los de las extensiones de $ATTRIBUTE_LIST), flujos con tamaños, runs, dispersos,
  tramos sin mapa y anomalías.
  ntfs-cat: contenido del flujo (residente, no residente, disperso, comprimido LZNT1). Los
  dispersos y lo que queda más allá del tamaño inicializado son ceros del formato. Los tramos
  no fiables (sectores ilegibles, sin mapa, fuera del origen, unidad comprimida dañada) se
  escriben con el patrón de relleno BAD! solo en esos bytes, se listan en la salida de errores y
  el código de salida es 3: nunca son datos del origen.

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
    /// Partición (desde 1) con la que trabajar.
    partition: Option<usize>,
    /// Aplicar explícitamente el desplazamiento de la imagen al abrir una partición GPT.
    apply_shift: bool,
    /// Volumen encontrado (desde 1) con el que trabajar.
    found: Option<usize>,
    /// Presupuesto de lecturas de la búsqueda.
    max_reads: Option<u64>,
    jump: u64,
    read_back: bool,
    /// Política de omisión de errores (`verify`).
    skip_codes: Vec<i32>,
    ignore_errors: bool,
    /// Patrones de relleno (`sectors`); `None` = por defecto.
    bad_fill: Option<[u8; 4]>,
    skip_fill: Option<[u8; 4]>,
    /// Registro MFT (`ntfs-stat`, `ntfs-cat`).
    record: Option<u64>,
    /// Flujo alternativo (`ntfs-cat`).
    stream: Option<String>,
    /// Fichero de salida (`ntfs-cat`).
    output: Option<PathBuf>,
    /// Sustituir la salida si existe (`ntfs-cat -o`).
    overwrite: bool,
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
        partition: None,
        apply_shift: false,
        found: None,
        max_reads: None,
        jump: 0,
        read_back: false,
        skip_codes: Vec::new(),
        ignore_errors: false,
        bad_fill: None,
        skip_fill: None,
        record: None,
        stream: None,
        output: None,
        overwrite: false,
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
            "--partition" => {
                let n: usize = num(&mut it, "--partition")?;
                if n == 0 {
                    return Err("--partition empieza en 1".into());
                }
                a.partition = Some(n);
            }
            "--apply-shift" => a.apply_shift = true,
            "--found" => {
                let n: usize = num(&mut it, "--found")?;
                if n == 0 {
                    return Err("--found empieza en 1".into());
                }
                a.found = Some(n);
            }
            "--max-reads" => {
                let n: u64 = num(&mut it, "--max-reads")?;
                if n == 0 || n > mr_api::mr_part::search::LIMIT_MAX_READS {
                    return Err("--max-reads debe estar entre 1 y 1048576".into());
                }
                a.max_reads = Some(n);
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
            "--record" => a.record = Some(num(&mut it, "--record")?),
            "--stream" => {
                a.stream = Some(it.next().ok_or("falta el valor de --stream")?);
            }
            "--overwrite" => a.overwrite = true,
            "-o" | "--output" => {
                a.output = Some(it.next().ok_or("falta el valor de -o")?.into());
            }
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
    if let Some(n) = a.partition {
        let t = mr_api::partitions(src.as_ref())?;
        if n > t.partitions.len() {
            return Err(mr_api::ApiError::InvalidArgument(format!(
                "no existe la partición {n} (el origen tiene {})",
                t.partitions.len()
            )));
        }
        let part = mr_api::open_partition(&src, &t, n - 1, a.apply_shift)?;
        eprintln!("Partición {n}: {}", part.info().description);
        return Ok(part);
    }
    if let Some(n) = a.found {
        let ctx = mr_api::task::TaskContext::detached();
        let found = mr_api::search_partitions(src.as_ref(), &search_options(a), &ctx)?;
        let r = &found.result;
        if n > r.volumes.len() {
            return Err(mr_api::ApiError::InvalidArgument(format!(
                "no existe el volumen encontrado {n} (la búsqueda encontró {})",
                r.volumes.len()
            )));
        }
        let v = mr_api::open_found_volume(&src, r, n - 1, a.apply_shift)?;
        eprintln!("Volumen encontrado {n}: {}", v.info().description);
        return Ok(v);
    }
    match a.range {
        Some((start, count)) => mr_api::open_range(&src, start, count),
        None => Ok(src),
    }
}

/// Nombre estable (JSON) del esquema.
fn scheme_name(s: Scheme) -> &'static str {
    match s {
        Scheme::None => "none",
        Scheme::Mbr => "mbr",
        Scheme::Gpt => "gpt",
        Scheme::Hybrid => "hybrid",
    }
}

/// Nombre estable (JSON) de un nivel de confianza.
fn confidence_name(c: &Confidence) -> &'static str {
    match c {
        Confidence::Valid => "valid",
        Confidence::ValidWithFixes(_) => "valid_with_fixes",
        Confidence::Suspicious(_) => "suspicious",
        Confidence::Invalid(_) => "invalid",
    }
}

fn confidence_label(c: &Confidence) -> &'static str {
    match c {
        Confidence::Valid => "válida",
        Confidence::ValidWithFixes(_) => "corregida",
        Confidence::Suspicious(_) => "SOSPECHOSA",
        Confidence::Invalid(_) => "INVÁLIDA",
    }
}

fn severity_name(s: Severity) -> &'static str {
    match s {
        Severity::Notice => "notice",
        Severity::Compensated => "compensated",
        Severity::Suspicious => "suspicious",
        Severity::Invalid => "invalid",
    }
}

fn severity_label(s: Severity) -> &'static str {
    match s {
        Severity::Notice => "aviso",
        Severity::Compensated => "compensada",
        Severity::Suspicious => "sospechosa",
        Severity::Invalid => "INVÁLIDA",
    }
}

fn opt_json(v: Option<u64>) -> String {
    v.map_or_else(|| "null".into(), |x| x.to_string())
}

fn anomaly_json(an: &Anomaly) -> String {
    format!(
        "{{\"code\":{},\"severity\":\"{}\",\"lba\":{},\"target_lba\":{},\"value\":{},\"message\":{}}}",
        json_str(an.code()),
        severity_name(an.severity()),
        opt_json(an.lba()),
        opt_json(an.target()),
        if an.value_signed() {
            an.value()
                .map_or_else(|| "null".into(), |v| (v as i64).to_string())
        } else {
            opt_json(an.value())
        },
        json_str(&an.to_string())
    )
}

fn origin_parts(o: &Origin) -> (&'static str, u64, Option<u64>) {
    match *o {
        Origin::Primary { slot } => ("primary", u64::from(slot), None),
        Origin::Logical { ebr_lba, slot } => ("logical", u64::from(slot), Some(ebr_lba)),
        Origin::Gpt { index } => ("gpt", u64::from(index), None),
    }
}

fn kind_text(k: &PartitionKind) -> (String, &'static str) {
    let name = k.type_name().unwrap_or("desconocido");
    match k {
        PartitionKind::Mbr(t) => (format!("0x{t:02X}"), name),
        PartitionKind::Gpt(g) => (mr_api::mr_part::gpt::format_guid(g), name),
    }
}

/// Imprime la tabla de particiones (texto o JSON).
fn print_partitions(t: &PartitionTable, json: bool) {
    let end = |p: &Partition| p.start_lba.saturating_add(p.sector_count.saturating_sub(1));
    if json {
        let parts: Vec<String> = t
            .partitions
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let (origin, slot, ebr) = origin_parts(&p.origin);
                let kind = match p.kind {
                    PartitionKind::Mbr(k) => k.to_string(),
                    PartitionKind::Gpt(_) => json_str(&kind_text(&p.kind).0),
                };
                let anomalies: Vec<String> = p.anomalies.iter().map(anomaly_json).collect();
                let gpt = match t.gpt_entry(p) {
                    Some(e) => format!(
                        ",\"name\":{},\"unique_guid\":\"{}\",\"attributes\":{}",
                        json_str(p.name.as_deref().unwrap_or("")),
                        mr_api::mr_part::gpt::format_guid(&e.unique_guid),
                        e.attributes
                    ),
                    None => String::new(),
                };
                format!(
                    "{{\"number\":{},\"origin\":\"{origin}\",\"slot\":{slot},\"ebr_lba\":{},\"start_lba\":{},\"sectors\":{},\"end_lba\":{},\"type\":{kind},\"type_name\":{},\"active\":{},\"confidence\":\"{}\"{gpt},\"anomalies\":[{}]}}",
                    i + 1,
                    opt_json(ebr),
                    p.start_lba,
                    p.sector_count,
                    end(p),
                    p.kind.type_name().map_or_else(|| "null".into(), json_str),
                    p.active,
                    confidence_name(&p.confidence),
                    anomalies.join(",")
                )
            })
            .collect();
        let anomalies: Vec<String> = t.anomalies.iter().map(anomaly_json).collect();
        let gpt = match &t.gpt {
            Some(g) => format!(
                "{{\"selected\":{},\"shift\":{},\"trailing_sectors_missing\":{},\"sector_size\":{},\"disk_guid\":{}}}",
                match g.selected {
                    Some(GptCopy::Primary) => "\"primary\"",
                    Some(GptCopy::Backup) => "\"backup\"",
                    None => "null",
                },
                g.shift,
                g.trailing_sectors_missing,
                g.sector_size,
                g.header().map_or_else(
                    || "null".into(),
                    |h| json_str(&mr_api::mr_part::gpt::format_guid(&h.disk_guid))
                )
            ),
            None => "null".into(),
        };
        println!(
            "{{\"scheme\":\"{}\",\"sector_size\":{},\"confidence\":\"{}\",\"gpt\":{gpt},\"partitions\":[{}],\"anomalies\":[{}]}}",
            scheme_name(t.scheme),
            t.sector_size,
            confidence_name(&t.confidence()),
            parts.join(","),
            anomalies.join(",")
        );
        return;
    }
    let scheme = match t.scheme {
        Scheme::None => "sin tabla de particiones reconocible",
        Scheme::Mbr => "MBR",
        Scheme::Gpt => "GPT",
        Scheme::Hybrid => "híbrido MBR + GPT (entradas del MBR y después las de la GPT)",
    };
    println!("Esquema:         {scheme}");
    println!("Tamaño sector:   {} bytes", t.sector_size);
    println!("Confianza:       {}", confidence_label(&t.confidence()));
    if let Some(g) = &t.gpt {
        let copy = match g.selected {
            Some(GptCopy::Primary) => "primaria",
            Some(GptCopy::Backup) => "COPIA DE RESPALDO EN USO (la primaria no es utilizable)",
            None => "NINGUNA (ninguna copia es utilizable)",
        };
        println!("Copia GPT:       {copy}");
        if let Some(h) = g.header() {
            println!(
                "GUID de disco:   {}",
                mr_api::mr_part::gpt::format_guid(&h.disk_guid)
            );
        }
        if g.shift != 0 {
            println!(
                "Desplazamiento:  {} sectores (LBA disco = LBA imagen + {}); las LBA mostradas son del disco; \
                 para abrir una partición use --partition N --apply-shift",
                g.shift, g.shift
            );
        }
        if g.trailing_sectors_missing > 0 {
            println!(
                "Faltan al final: {} sectores del disco",
                g.trailing_sectors_missing
            );
        }
    }
    if t.partitions.is_empty() {
        println!("(sin particiones)");
    } else {
        println!(
            "{:>3}  {:<22} {:>3}  {:>12}  {:>12}  {:>12}  {:<34} {:<20} Confianza",
            "N", "Origen", "Act", "LBA inicial", "LBA final", "Sectores", "Tipo", "Nombre"
        );
        for (i, p) in t.partitions.iter().enumerate() {
            let origin = match p.origin {
                Origin::Primary { slot } => format!("primaria {}", u32::from(slot) + 1),
                Origin::Logical { ebr_lba, .. } => format!("lógica (EBR {ebr_lba})"),
                Origin::Gpt { index } => format!("GPT {}", u64::from(index) + 1),
            };
            let (code, name) = kind_text(&p.kind);
            let kind = match p.kind {
                PartitionKind::Mbr(_) => format!("{code} {name}"),
                PartitionKind::Gpt(_) => name.to_string(),
            };
            println!(
                "{:>3}  {:<22} {:>3}  {:>12}  {:>12}  {:>12}  {:<34} {:<20} {}",
                i + 1,
                origin,
                if p.active { "*" } else { "" },
                p.start_lba,
                end(p),
                p.sector_count,
                kind,
                p.name.as_deref().unwrap_or(""),
                confidence_label(&p.confidence)
            );
            if let PartitionKind::Gpt(_) = p.kind {
                println!("       tipo GUID {code}");
            }
            for an in &p.anomalies {
                println!(
                    "       - [{}] {}: {an}",
                    severity_label(an.severity()),
                    an.code()
                );
            }
        }
    }
    if !t.anomalies.is_empty() {
        println!("Anomalías de la tabla:");
        for an in &t.anomalies {
            println!(
                "  - [{}] {}: {an}",
                severity_label(an.severity()),
                an.code()
            );
        }
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
    if a.partition.is_some() && a.range.is_some() {
        return Err(usage("--partition y --range no se pueden combinar"));
    }
    if a.found.is_some() && (a.partition.is_some() || a.range.is_some()) {
        return Err(usage(
            "--found no se puede combinar con --partition ni con --range",
        ));
    }
    if a.found.is_some() && cmd == "find-partitions" {
        return Err(usage("--found no se aplica a find-partitions"));
    }
    if cmd != "sectors" && (a.bad_fill.is_some() || a.skip_fill.is_some()) {
        return Err(usage("--bad-fill y --skip-fill solo se aplican a sectors"));
    }
    let ntfs_file_cmd = cmd == "ntfs-cat" || cmd == "ntfs-stat";
    if !ntfs_file_cmd && a.record.is_some() {
        return Err(usage("--record solo se aplica a ntfs-cat y ntfs-stat"));
    }
    if cmd != "ntfs-cat" && (a.stream.is_some() || a.output.is_some()) {
        return Err(usage("--stream y -o solo se aplican a ntfs-cat"));
    }
    if a.overwrite && a.output.is_none() {
        return Err(usage("--overwrite solo se aplica a ntfs-cat con -o"));
    }
    if cmd == "ntfs-cat" && a.json {
        return Err(usage(
            "ntfs-cat no admite --json (escribe el contenido del flujo)",
        ));
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
        "partitions" => {
            let p = one(&a)?;
            let src = open_src(&a, &p).map_err(|e| op(&e))?;
            let t = mr_api::partitions(src.as_ref()).map_err(|e| op(&e))?;
            print_partitions(&t, a.json);
            let unreadable = t
                .all_anomalies()
                .filter(|x| matches!(x, Anomaly::UnreadableSector { .. }))
                .count();
            if unreadable > 0 {
                return Err((
                    3,
                    format!("{unreadable} sectores de tabla ilegibles: no se interpretaron"),
                ));
            }
        }
        "find-partitions" => {
            let p = one(&a)?;
            let src = open_src(&a, &p).map_err(|e| op(&e))?;
            let task =
                mr_api::start_partition_search_task(src, search_options(&a)).map_err(|e| op(&e))?;
            while task.state() == TaskState::Running {
                let (d, t) = task.progress();
                if !a.json && t > 0 {
                    eprint!("\r{d}/{t} lecturas");
                }
                thread::sleep(Duration::from_millis(100));
            }
            if !a.json {
                eprintln!();
            }
            let found = task.wait().map_err(|e| op(&e))?;
            print_search(&found.result, a.json);
            let bad = found.result.stats.unverifiable_sectors;
            if bad > 0 {
                return Err((
                    3,
                    format!("{bad} sectores ilegibles durante la búsqueda: no se interpretaron"),
                ));
            }
        }
        "ntfs-info" => {
            let p = one(&a)?;
            let src = open_src(&a, &p).map_err(|e| op(&e))?;
            let v = mr_api::ntfs_open(&src).map_err(|e| {
                let hint = if a.partition.is_none() && a.found.is_none() && a.range.is_none() {
                    " (si el origen es un disco con particiones, use --partition N)"
                } else {
                    ""
                };
                (1u8, format!("{e}{hint}"))
            })?;
            let sum = ntfs_summary(&v).map_err(|e| op(&e))?;
            print_ntfs(&v, &sum, a.json);
            if !sum.unreadable.is_empty() {
                return Err((
                    3,
                    format!(
                        "{} registros MFT con sectores ilegibles: no se interpretaron",
                        sum.unreadable.len()
                    ),
                ));
            }
        }
        "ntfs-stat" | "ntfs-cat" => {
            let p = one(&a)?;
            let record = a.record.ok_or_else(|| usage("falta --record N"))?;
            let src = open_src(&a, &p).map_err(|e| op(&e))?;
            let v = mr_api::ntfs_open(&src).map_err(|e| op(&e))?;
            if cmd == "ntfs-cat" {
                let stream = a.stream.clone().unwrap_or_default();
                ntfs_files::cat(&v, &src, record, &stream, a.output.as_deref(), a.overwrite)?;
            } else {
                let f = mr_api::ntfs_file(&v, record).map_err(|e| op(&e))?;
                if a.json {
                    println!("{}", ntfs_files::stat_json(&v, &f));
                } else {
                    ntfs_files::stat_text(&v, &f);
                }
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

/// Recorrido completo de la MFT para `ntfs-info`.
struct NtfsSummary {
    counts: std::collections::BTreeMap<RecordState, u64>,
    directories: u64,
    /// (registro, código, mensaje)
    damaged: Vec<(u64, &'static str, String)>,
    /// (registro, LBA del origen no fiables)
    unreadable: Vec<(u64, Vec<u64>)>,
}

fn ntfs_summary(v: &NtfsVolume) -> Result<NtfsSummary, mr_api::mr_fs_ntfs::NtfsError> {
    let mut s = NtfsSummary {
        counts: RecordState::ALL.iter().map(|&k| (k, 0)).collect(),
        directories: 0,
        damaged: Vec::new(),
        unreadable: Vec::new(),
    };
    for r in v.records() {
        let (n, o) = r?;
        *s.counts.entry(o.state()).or_default() += 1;
        match &o {
            RecordOutcome::Parsed(rec) if rec.in_use() && rec.is_directory() => s.directories += 1,
            RecordOutcome::Damaged(d) => s.damaged.push((n, d.code(), d.to_string())),
            RecordOutcome::Unreadable { lbas } => s.unreadable.push((n, lbas.clone())),
            _ => {}
        }
    }
    Ok(s)
}

fn backup_state_label(s: &BackupState) -> &'static str {
    match s {
        BackupState::Matches => "coincide con el principal",
        BackupState::Differs => "DISTINTA del principal",
        BackupState::Invalid(_) => "NO VÁLIDA",
        BackupState::Unreadable { .. } => "ILEGIBLE",
        BackupState::OutsideSource => "fuera del origen",
        BackupState::InUse => "en uso (el principal no es válido)",
    }
}

fn runs_json(runs: &[mr_api::mr_fs_ntfs::runs::Run]) -> String {
    let v: Vec<String> = runs
        .iter()
        .map(|r| {
            format!(
                "[{},{},{}]",
                r.vcn,
                r.lcn.map_or_else(|| "null".into(), |l| l.to_string()),
                r.length
            )
        })
        .collect();
    format!("[{}]", v.join(","))
}

/// JSON de `ntfs-info`.
fn ntfs_json(v: &NtfsVolume, s: &NtfsSummary) -> String {
    let i = v.info();
    let b = &i.boot;
    let (src, boot_off) = match i.boot_source {
        BootSource::Primary => ("primary", 0),
        BootSource::Backup { offset } => ("backup", offset),
    };
    let mism: Vec<String> = i
        .mirror
        .mismatches
        .iter()
        .map(|m| {
            format!(
                "{{\"record\":{},\"kind\":\"{}\",\"mft\":\"{}\",\"mirror\":\"{}\"}}",
                m.record,
                m.kind(),
                m.mft.code(),
                m.mirror.code()
            )
        })
        .collect();
    let counts: Vec<String> = s
        .counts
        .iter()
        .map(|(k, n)| format!("\"{}\":{n}", k.code()))
        .collect();
    let damaged: Vec<String> = s
        .damaged
        .iter()
        .map(|(n, c, m)| {
            format!(
                "{{\"record\":{n},\"code\":\"{c}\",\"message\":{}}}",
                json_str(m)
            )
        })
        .collect();
    let unreadable: Vec<String> = s
        .unreadable
        .iter()
        .map(|(n, l)| {
            let l: Vec<String> = l.iter().map(u64::to_string).collect();
            format!("{{\"record\":{n},\"lbas\":[{}]}}", l.join(","))
        })
        .collect();
    let anomalies: Vec<String> = i
        .anomalies
        .iter()
        .map(|a| {
            format!(
                "{{\"code\":\"{}\",\"severity\":\"{}\",\"record\":{},\"message\":{}}}",
                a.code(),
                severity_name(a.severity()),
                opt_json(a.record()),
                json_str(&a.to_string())
            )
        })
        .collect();
    let mut o = Vec::new();
    o.push(format!("\"boot_source\":\"{src}\""));
    o.push(format!("\"boot_offset\":{boot_off}"));
    o.push(format!("\"primary_boot\":\"{}\"", i.primary_boot.code()));
    o.push(format!(
        "\"backup_boot\":{{\"offset\":{},\"state\":\"{}\"}}",
        i.backup_boot.offset,
        i.backup_boot.state.code()
    ));
    o.push(format!("\"serial\":\"{:016x}\"", b.serial));
    o.push(format!(
        "\"label\":{}",
        i.label.as_deref().map_or_else(|| "null".into(), json_str)
    ));
    o.push(format!(
        "\"version\":{}",
        i.version
            .map_or_else(|| "null".into(), |(a, b)| format!("\"{a}.{b}\""))
    ));
    o.push(format!("\"bytes_per_sector\":{}", b.bytes_per_sector));
    o.push(format!("\"device_sector_size\":{}", i.device_sector_size));
    o.push(format!("\"sectors_per_cluster\":{}", b.sectors_per_cluster));
    o.push(format!("\"cluster_size\":{}", b.cluster_size));
    o.push(format!("\"total_sectors\":{}", b.total_sectors));
    o.push(format!("\"volume_bytes\":{}", b.volume_bytes()));
    o.push(format!("\"mft_lcn\":{}", b.mft_lcn));
    o.push(format!("\"mftmirr_lcn\":{}", b.mftmirr_lcn));
    o.push(format!("\"mft_record_size\":{}", b.mft_record_size));
    o.push(format!("\"index_record_size\":{}", b.index_record_size));
    o.push(format!(
        "\"mft\":{{\"record0_source\":\"{}\",\"runs\":{},\"allocated_size\":{},\"data_size\":{},\"initialized_size\":{},\"record_count\":{}}}",
        match i.mft_record0_source {
            RecordSource::Mft => "mft",
            RecordSource::Mirror => "mirror",
        },
        runs_json(&i.mft.runs),
        i.mft.allocated_size,
        i.mft.data_size,
        i.mft.initialized_size,
        i.mft.record_count
    ));
    o.push(format!(
        "\"mirror\":{{\"lcn\":{},\"records\":{},\"mismatches\":[{}]}}",
        i.mirror.lcn,
        i.mirror.records,
        mism.join(",")
    ));
    o.push(format!(
        "\"records\":{{{},\"directories\":{},\"damaged_records\":[{}],\"unreadable_records\":[{}]}}",
        counts.join(","),
        s.directories,
        damaged.join(","),
        unreadable.join(",")
    ));
    o.push(format!(
        "\"confidence\":\"{}\"",
        confidence_name(&i.confidence())
    ));
    o.push(format!("\"anomalies\":[{}]", anomalies.join(",")));
    format!("{{{}}}", o.join(","))
}

/// Imprime la información del volumen NTFS (texto o JSON).
fn print_ntfs(v: &NtfsVolume, s: &NtfsSummary, json: bool) {
    if json {
        println!("{}", ntfs_json(v, s));
        return;
    }
    let i = v.info();
    let b = &i.boot;
    println!(
        "Volumen NTFS (confianza: {})",
        confidence_label(&i.confidence())
    );
    let used = match i.boot_source {
        BootSource::Primary => "principal (sector 0)".to_string(),
        BootSource::Backup { offset } => {
            format!("COPIA en el byte {offset} (el principal no es válido)")
        }
    };
    println!("  Arranque usado:       {used}");
    println!(
        "  Copia del arranque:   byte {}: {}",
        i.backup_boot.offset,
        backup_state_label(&i.backup_boot.state)
    );
    println!("  Número de serie:      {:016x}", b.serial);
    println!(
        "  Etiqueta:             {}",
        i.label.as_deref().unwrap_or("(sin etiqueta)")
    );
    println!(
        "  Versión NTFS:         {}",
        i.version
            .map_or_else(|| "(desconocida)".into(), |(a, b)| format!("{a}.{b}"))
    );
    println!(
        "  Bytes por sector:     {} (origen: {})",
        b.bytes_per_sector, i.device_sector_size
    );
    println!(
        "  Clúster:              {} B ({} sectores)",
        b.cluster_size, b.sectors_per_cluster
    );
    println!(
        "  Total de sectores:    {} ({} bytes)",
        b.total_sectors,
        b.volume_bytes()
    );
    println!(
        "  Registros:            MFT de {} B, índice de {} B",
        b.mft_record_size, b.index_record_size
    );
    println!(
        "  $MFT:                 LCN {}, {} registros (datos {} B, inicializados {} B, asignados {} B){}",
        b.mft_lcn,
        i.mft.record_count,
        i.mft.data_size,
        i.mft.initialized_size,
        i.mft.allocated_size,
        if i.mft_record0_source == RecordSource::Mirror {
            ", localizada con la copia de $MFTMirr"
        } else {
            ""
        }
    );
    for r in &i.mft.runs {
        println!(
            "    run: VCN {:<8} LCN {:<10} {} clústeres",
            r.vcn,
            r.lcn.map_or_else(|| "disperso".into(), |l| l.to_string()),
            r.length
        );
    }
    println!(
        "  $MFTMirr:             LCN {}, {} registros comparados, {}",
        i.mirror.lcn,
        i.mirror.records,
        if i.mirror.mismatches.is_empty() {
            "coinciden".to_string()
        } else {
            format!("{} NO COINCIDEN", i.mirror.mismatches.len())
        }
    );
    println!("Registros de la MFT:");
    for (k, n) in &s.counts {
        if *n > 0 {
            println!("  {:<34} {n}", k.label());
        }
    }
    println!("  {:<34} {}", "(directorios en uso)", s.directories);
    const MAX_LISTED: usize = 50;
    for (n, c, m) in s.damaged.iter().take(MAX_LISTED) {
        println!("  DAÑADO   registro {n}: {c}: {m}");
    }
    for (n, l) in s.unreadable.iter().take(MAX_LISTED) {
        println!("  ILEGIBLE registro {n}: sectores {l:?}");
    }
    let hidden =
        s.damaged.len().saturating_sub(MAX_LISTED) + s.unreadable.len().saturating_sub(MAX_LISTED);
    if hidden > 0 {
        println!("  ... y {hidden} más (use --json para la lista completa)");
    }
    if !i.anomalies.is_empty() {
        println!("Anomalías:");
        for a in &i.anomalies {
            println!("  [{}] {}: {a}", severity_label(a.severity()), a.code());
        }
    }
}

/// Opciones de la búsqueda según los argumentos (presupuesto de lecturas).
fn search_options(a: &Args) -> SearchOptions {
    SearchOptions {
        max_reads: a
            .max_reads
            .unwrap_or(mr_api::mr_part::search::DEFAULT_MAX_READS),
        ..SearchOptions::default()
    }
}

fn check_label(c: Check) -> &'static str {
    match c {
        Check::Yes => "sí",
        Check::No => "no o dañada",
        Check::Unverifiable => "NO VERIFICABLE (sector ilegible)",
        Check::Outside => "fuera de la imagen",
        Check::NotApplicable => "no aplica",
    }
}

fn opt_i64_json(v: Option<i64>) -> String {
    v.map_or_else(|| "null".into(), |x| x.to_string())
}

fn volume_json(i: usize, v: &FoundVolume, ss: u32) -> String {
    let lba = |b: i64| b.div_euclid(i64::from(ss));
    let anomalies: Vec<String> = v
        .anomalies
        .iter()
        .map(|an| {
            format!(
                "{{\"code\":{},\"severity\":\"{}\",\"lba\":{},\"value\":{},\"message\":{}}}",
                json_str(an.code()),
                severity_name(an.severity()),
                opt_i64_json(an.lba().map(|l| l as i64).or(an.byte().map(lba))),
                opt_i64_json(an.value()),
                json_str(&an.to_string())
            )
        })
        .collect();
    let evidence: Vec<String> = v.evidence().iter().map(|e| json_str(e)).collect();
    format!(
        "{{\"number\":{},\"fs\":\"{}\",\"start_byte\":{},\"length_bytes\":{},\"start_lba\":{},\"sectors\":{},\"end_lba\":{},\"fs_sector_size\":{},\
\"entry\":\"{}\",\"primary\":\"{}\",\"backup\":\"{}\",\"structures\":\"{}\",\"self_location\":\"{}\",\"aligned\":\"{}\",\"indicators\":{},\
\"evidence\":[{}],\"partial\":{},\"listed\":{},\"table_partition\":{},\"found_table\":{},\"requires_shift\":{},\"tier\":\"{}\",\"origin\":{},\
\"boot_lba\":{},\"backup_boot_lba\":{},\"hidden_sectors\":{},\"serial\":\"{:0width$x}\",\"suggested_mbr_type\":{},\"confidence\":\"{}\",\"score\":{},\"anomalies\":[{}]}}",
        i + 1,
        v.fs.code(),
        v.start_byte,
        v.length_bytes,
        v.start_lba(ss),
        v.sector_count(ss),
        v.start_lba(ss) + v.sector_count(ss) as i64 - 1,
        v.fs_sector_size,
        v.entry.code(),
        v.primary.code(),
        v.backup.code(),
        v.structures.code(),
        v.self_location.code(),
        v.aligned.code(),
        json_str(&v.indicators()),
        evidence.join(","),
        v.partial(),
        v.listed(),
        opt_json(v.table_partition.map(|x| x as u64 + 1)),
        opt_json(v.found_table.map(|x| x as u64 + 1)),
        v.requires_shift(),
        v.tier.code(),
        json_str(&v.origin),
        lba(v.primary_byte),
        opt_i64_json(v.backup_byte.map(lba)),
        v.declared_start,
        v.serial,
        v.fs.suggested_mbr_type(),
        confidence_name(&v.confidence),
        v.score,
        anomalies.join(","),
        width = v.fs.serial_digits()
    )
}

/// Imprime el resultado de la búsqueda (texto o JSON).
fn print_search(r: &SearchResult, json: bool) {
    let ss = r.sector_size;
    if json {
        let volumes: Vec<String> = r
            .volumes
            .iter()
            .enumerate()
            .map(|(i, v)| volume_json(i, v, ss))
            .collect();
        let tables: Vec<String> = r
            .tables
            .iter()
            .map(|t| {
                let entries: Vec<String> = t
                    .entries
                    .iter()
                    .map(|e| {
                        format!(
                            "{{\"start_byte\":{},\"length_bytes\":{},\"mbr_type\":{},\"extended\":{}}}",
                            e.start_byte, e.length_bytes, e.mbr_type, e.extended
                        )
                    })
                    .collect();
                format!(
                    "{{\"kind\":\"{}\",\"lba\":{},\"declared_lba\":{},\"entries\":[{}]}}",
                    match t.kind {
                        FoundTableKind::Mbr => "mbr",
                        FoundTableKind::Gpt => "gpt",
                    },
                    t.lba,
                    t.declared_lba,
                    entries.join(",")
                )
            })
            .collect();
        let anomalies: Vec<String> = r
            .anomalies
            .iter()
            .map(|an| {
                format!(
                    "{{\"code\":{},\"severity\":\"{}\",\"lba\":{},\"value\":{},\"message\":{}}}",
                    json_str(an.code()),
                    severity_name(an.severity()),
                    an.lba(),
                    opt_i64_json(an.value()),
                    json_str(&an.to_string())
                )
            })
            .collect();
        let pending: Vec<String> = r.pending.iter().map(u64::to_string).collect();
        println!(
            "{{\"status\":\"{}\",\"sector_size\":{ss},\"sectors\":{},\"reads\":{},\"max_reads\":{},\"bytes_read\":{},\"unverifiable_sectors\":{},\"probed\":{},\"pending\":[{}],\"volume_limit_reached\":{},\"shift_bytes\":{},\"anomalies\":[{}],\"tables\":[{}],\"volumes\":[{}]}}",
            r.status.code(),
            r.sectors,
            r.stats.reads,
            r.options.max_reads,
            r.stats.bytes,
            r.stats.unverifiable_sectors,
            r.probed,
            pending.join(","),
            r.volume_limit_reached,
            opt_i64_json(r.shift_bytes),
            anomalies.join(","),
            tables.join(","),
            volumes.join(",")
        );
        return;
    }
    let status = match r.status {
        SearchStatus::Complete => "completa",
        SearchStatus::BudgetExhausted => "PRESUPUESTO AGOTADO (resultado parcial)",
        SearchStatus::Cancelled => "CANCELADA (resultado parcial)",
    };
    println!("Búsqueda exprés: {status}");
    println!(
        "Lecturas:        {} de {} ({} bytes); candidatos sondeados {}, pendientes {}",
        r.stats.reads,
        r.options.max_reads,
        r.stats.bytes,
        r.probed,
        r.pending.iter().sum::<u64>()
    );
    if r.stats.unverifiable_sectors > 0 {
        println!(
            "Sectores no verificables: {} (ilegibles: su relleno no se interpretó)",
            r.stats.unverifiable_sectors
        );
    }
    for an in &r.anomalies {
        println!(
            "  - [{}] {}: {an}",
            severity_label(an.severity()),
            an.code()
        );
    }
    for t in &r.tables {
        let kind = match t.kind {
            FoundTableKind::Mbr => "MBR/EBR",
            FoundTableKind::Gpt => "cabecera GPT",
        };
        println!(
            "Tabla encontrada: {kind} en la LBA {} con {} entradas",
            t.lba,
            t.entries.len()
        );
    }
    if r.volumes.is_empty() {
        println!("(no se encontró ningún volumen)");
        return;
    }
    println!(
        "{:>3}  {:<11} {:<6} {:>12}  {:>12}  {:>12}  {:<18} {:<11} Origen",
        "N", "Estado", "FS", "LBA inicial", "LBA final", "Sectores", "Indicadores", "Confianza"
    );
    for (i, v) in r.volumes.iter().enumerate() {
        let state = match v.table_partition {
            Some(p) => format!("tabla #{}", p + 1),
            None if v.partial() => "parcial".to_string(),
            None => "encontrada".to_string(),
        };
        let start = v.start_lba(ss);
        println!(
            "{:>3}  {:<11} {:<6} {:>12}  {:>12}  {:>12}  {:<18} {:<11} {}",
            i + 1,
            state,
            v.fs.name(),
            start,
            start + v.sector_count(ss) as i64 - 1,
            v.sector_count(ss),
            v.indicators(),
            confidence_label(&v.confidence),
            v.origin
        );
        println!(
            "       principal: {}; copia: {}; estructuras: {}",
            check_label(v.primary),
            check_label(v.backup),
            check_label(v.structures)
        );
        for an in &v.anomalies {
            println!(
                "       - [{}] {}: {an}",
                severity_label(an.severity()),
                an.code()
            );
        }
    }
    println!(
        "Indicadores: E entrada de tabla, P principal, C copia, S estructuras, U inicio declarado, \
         A alineación (+ sí, x no, ? no verificable, o fuera de la imagen, - no aplica)."
    );
    println!("Para abrir un volumen: --found N (y --apply-shift si depende del desplazamiento).");
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
