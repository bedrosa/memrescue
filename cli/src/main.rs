//! memrescue-cli: acceso al motor de MemRescue sin interfaz gráfica.
//!
//! Todas las operaciones de M0 son de solo lectura y solo aceptan ficheros de imagen.

use mr_api::task::TaskState;
use mr_api::{HashOptions, ImageInfo};
use mr_common::HashAlgorithm;
use mr_store::Store;
use std::path::PathBuf;
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

const USAGE: &str = "\
memrescue-cli: motor de MemRescue

USO:
  memrescue-cli version
  memrescue-cli image-info <imagen> [--sector-size N] [--json]
  memrescue-cli hash <imagen> [--algo crc32,md5,sha1,sha256] [--sector-size N] [--db trabajo.mrdb] [--json]
  memrescue-cli db-init <trabajo.mrdb>
  memrescue-cli jobs <trabajo.mrdb>

Códigos de salida: 0 correcto, 1 error de operación, 2 uso incorrecto.
";

struct Args {
    positional: Vec<String>,
    sector_size: u32,
    algos: Vec<HashAlgorithm>,
    db: Option<PathBuf>,
    json: bool,
}

fn parse(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut a = Args {
        positional: vec![],
        sector_size: 512,
        algos: vec![HashAlgorithm::Sha256],
        db: None,
        json: false,
    };
    while let Some(s) = it.next() {
        match s.as_str() {
            "--sector-size" => {
                let v = it.next().ok_or("falta el valor de --sector-size")?;
                a.sector_size = v
                    .parse()
                    .map_err(|_| format!("tamaño de sector no válido: {v}"))?;
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

fn print_info(i: &ImageInfo, json: bool) {
    if json {
        println!(
            "{{\"path\":{},\"size_bytes\":{},\"sector_size\":{},\"sector_count\":{},\"trailing_bytes\":{}}}",
            json_str(&i.path.display().to_string()),
            i.size_bytes,
            i.sector_size,
            i.sector_count,
            i.trailing_bytes
        );
    } else {
        println!("Imagen:          {}", i.path.display());
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

    match cmd.as_str() {
        "version" | "--version" => {
            println!("MemRescue engine {} (ABI 1)", mr_api::ENGINE_VERSION);
        }
        "help" | "--help" | "-h" => print!("{USAGE}"),
        "image-info" => {
            let p = one(&a)?;
            let i = mr_api::image_info(&p, a.sector_size).map_err(|e| op(&e))?;
            print_info(&i, a.json);
        }
        "hash" => {
            let p = one(&a)?;
            let opts = HashOptions {
                algorithms: a.algos.clone(),
                store_path: a.db.clone(),
                ..HashOptions::default()
            };
            let task = mr_api::start_hash_task(PathBuf::from(&p), a.sector_size, opts)
                .map_err(|e| op(&e))?;
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
