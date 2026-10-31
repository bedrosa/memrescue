//! `recover`: recuperación de ficheros y directorios del árbol virtual de un volumen NTFS (#26)
//! a una carpeta de destino, con diario JSON Lines por fichero (`docs/formatos/recuperacion.md`
//! y `docs/formatos/diario-recuperacion.md`).

use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use mr_api::mr_fs_ntfs::NtfsVolume;
use mr_api::mr_recover::{ConflictPolicy, RecoverOptions, RunStatus, Selection};
use mr_api::mr_vfs::filter::{parse_categories, parse_data_states, Filter, NameRules};
use mr_api::mr_vfs::{Category, DataState, NodeId, NtfsBuildOptions};
use mr_api::task::{TaskContext, TaskState};
use mr_api::{RecoverRequest, Source};

/// Opciones de `recover`.
pub struct RecoverArgs<'a> {
    pub paths: &'a [String],
    pub dest: PathBuf,
    pub deleted: bool,
    pub masks: Option<&'a str>,
    pub categories: Option<&'a str>,
    pub states: Option<&'a str>,
    pub conflict: Option<&'a str>,
    pub confirm_overwrite: bool,
    pub include_overwritten: bool,
    pub include_reallocated_bytes: bool,
    pub journal: Option<PathBuf>,
}

/// Política de conflictos de la CLI.
pub fn parse_conflict(s: &str) -> Result<ConflictPolicy, String> {
    match s {
        "rename" => Ok(ConflictPolicy::Rename),
        "skip" => Ok(ConflictPolicy::Skip),
        "skip-identical" => Ok(ConflictPolicy::SkipIfIdentical),
        "overwrite" => Ok(ConflictPolicy::Overwrite),
        other => Err(format!(
            "--conflict debe ser rename, skip, skip-identical u overwrite, no {other}"
        )),
    }
}

/// Ejecuta la recuperación. Errores: (2, uso) o (1, operación); el código 3 si algún fichero no
/// es íntegro.
pub fn run(src: &Source, vol: NtfsVolume, a: &RecoverArgs<'_>) -> Result<(), (u8, String)> {
    let usage = |m: String| (2u8, m);
    let op = |e: &dyn std::fmt::Display| (1u8, e.to_string());
    let conflict = match a.conflict {
        Some(c) => parse_conflict(c).map_err(usage)?,
        None => ConflictPolicy::Rename,
    };
    if conflict == ConflictPolicy::Overwrite && !a.confirm_overwrite {
        return Err(usage(
            "--conflict overwrite sustituye ficheros existentes del destino: confírmelo con \
             --confirm-overwrite"
                .into(),
        ));
    }
    if a.confirm_overwrite && conflict != ConflictPolicy::Overwrite {
        return Err(usage(
            "--confirm-overwrite solo se aplica con --conflict overwrite".into(),
        ));
    }
    let categories = match a.categories {
        Some(c) => parse_categories(c).map_err(|e| usage(e.to_string()))?,
        // Por defecto todo salvo los ficheros del sistema (INV-01 §7.5).
        None => Category::Live.bit() | Category::Deleted.bit() | Category::Orphan.bit(),
    };
    let data_states = match a.states {
        Some(s) => parse_data_states(s).map_err(|e| usage(e.to_string()))?,
        None => 0,
    };
    let masks = a.masks.unwrap_or("").to_string();
    NameRules::parse(&masks).map_err(|e| usage(e.to_string()))?;
    let vol = Arc::new(vol);
    eprintln!("Construyendo el árbol virtual...");
    let vfs = mr_api::vfs_build_ntfs(&vol, &NtfsBuildOptions::default(), &TaskContext::detached())
        .map_err(|e| op(&e))?;
    let mut nodes = Vec::new();
    let wanted: Vec<String> = if a.paths.is_empty() {
        vec!["/".into()]
    } else {
        a.paths.to_vec()
    };
    for p in &wanted {
        let n = vfs.find(p).ok_or_else(|| {
            (
                1u8,
                format!("no existe {p} en el árbol virtual (ver vfs-ls)"),
            )
        })?;
        nodes.push(n);
    }
    if nodes.is_empty() {
        nodes.push(NodeId::ROOT);
    }
    let mut options = RecoverOptions::new(&a.dest);
    options.conflict = conflict;
    options.overwrite_confirmed = a.confirm_overwrite;
    if a.include_overwritten {
        options.write_states |= DataState::Sobrescrito.bit();
    }
    options.include_reallocated_bytes = a.include_reallocated_bytes;
    options.journal = a.journal.clone();
    let req = RecoverRequest {
        selection: Selection {
            nodes,
            filter: Filter {
                categories,
                data_states,
                deleted_only: a.deleted,
                files_only: true,
                ..Filter::default()
            },
            masks,
        },
        options,
    };
    let task = mr_api::start_recover_task(Arc::clone(src), vol, vfs, req).map_err(|e| op(&e))?;
    let mut last = String::new();
    while task.state() == TaskState::Running {
        let p = task.progress();
        let line = format!(
            "\r{}/{} ficheros, {}/{} bytes",
            p.files_done, p.files_total, p.bytes_done, p.bytes_total
        );
        if line != last && p.files_total > 0 {
            eprint!("{line}");
            last = line;
        }
        thread::sleep(Duration::from_millis(100));
    }
    eprintln!();
    let s = task.wait().map_err(|e| op(&e))?;
    let t = &s.totals;
    println!("Destino: {}", s.destination);
    println!("Diario: {}", s.journal.display());
    println!(
        "Ficheros: {} planificados, {} escritos ({} íntegros, {} sin errores pero de procedencia no verificable, {} parciales, {} sobrescritos), {} idénticos ya presentes, {} omitidos por su estado, {} omitidos por conflicto, {} no soportados, {} errores",
        t.files_planned,
        t.files_written,
        t.written(DataState::Integro),
        t.written(DataState::IntegroNoVerificable),
        t.written(DataState::Parcial),
        t.written(DataState::Sobrescrito),
        t.skipped_identical,
        t.skipped_state,
        t.skipped_conflict,
        t.unsupported,
        t.errors
    );
    println!(
        "Directorios: {} creados, {} reutilizados; {} bytes escritos, {} no fiables ({} con el patrón de relleno); {} renombrados por conflicto; {} ficheros incompletos marcados con .mrbad",
        t.dirs_created,
        t.dirs_merged,
        t.bytes_written,
        t.untrusted_bytes,
        t.filled_bytes,
        t.renamed,
        t.marked_incomplete
    );
    if t.long_paths > 0 {
        println!(
            "AVISO: {} ficheros tienen una ruta de más de 259 caracteres; algunos programas (el \
             explorador de Windows entre ellos) pueden no abrirlos.",
            t.long_paths
        );
    }
    if !s.capabilities.named_streams {
        println!(
            "Destino sin flujos con nombre: los flujos alternativos se escribieron como ficheros \
             hermanos «nombre%3Aflujo»."
        );
    }
    match s.status {
        RunStatus::Complete => {}
        RunStatus::Cancelled => return Err((1, "recuperación cancelada".into())),
        RunStatus::Failed => {
            return Err((
                1,
                format!(
                    "la recuperación se detuvo: {}",
                    s.reason.as_deref().unwrap_or("error")
                ),
            ))
        }
    }
    match s.exit_code() {
        0 => Ok(()),
        3 => Err((
            3,
            "hay ficheros que no son íntegros (escritos con tramos no fiables, de procedencia no \
             verificable u omitidos por su estado): ver el diario"
                .into(),
        )),
        _ => Err((1, format!("{} errores: ver el diario", t.errors))),
    }
}
