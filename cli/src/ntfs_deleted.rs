//! `ntfs-deleted` y `ntfs-recover`: ficheros borrados de la MFT con su recuperabilidad y
//! recuperación con el estado de cada tramo (#23).

use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use mr_api::mr_fs_ntfs::deleted::{ClusterState, DeletedEntry, DeletedScan, StreamAssessment};
use mr_api::mr_fs_ntfs::{NtfsVolume, RangeState, RecordState, RecoveryReport};
use mr_api::task::TaskState;

use crate::json_str;
use crate::ntfs_files::filetime_iso;

/// Máximo de tramos que se listan en la salida de errores.
const MAX_LISTED_RANGES: usize = 50;

fn time_json(ft: u64) -> String {
    filetime_iso(ft).map_or_else(|| "null".into(), |t| json_str(&t))
}

fn state_json(s: ClusterState) -> String {
    match s {
        ClusterState::Owned { owner } => format!("\"owned\",{owner}"),
        ClusterState::SharedWithDeleted { record } => format!("\"shared_with_deleted\",{record}"),
        s => format!("{},null", json_str(s.code())),
    }
}

fn owner_label(
    scan_owners: &std::collections::BTreeMap<u64, mr_api::mr_fs_ntfs::deleted::OwnerInfo>,
    o: u64,
) -> String {
    scan_owners
        .get(&o)
        .map_or_else(|| format!("registro {o}"), |i| i.display())
}

fn stream_json(s: &DeletedScan, a: &StreamAssessment) -> String {
    let owners: Vec<String> = a
        .owners
        .iter()
        .map(|o| {
            let path = s
                .owners
                .get(o)
                .and_then(|i| i.path.clone().or_else(|| i.name.clone()));
            format!(
                "{{\"record\":{o},\"path\":{}}}",
                path.map_or_else(|| "null".into(), |p| json_str(&p))
            )
        })
        .collect();
    let shared: Vec<String> = a.shared_with.iter().map(u64::to_string).collect();
    let reasons: Vec<String> = a
        .reasons
        .iter()
        .map(|r| {
            format!(
                "{{\"code\":{},\"message\":{}}}",
                json_str(r.code()),
                json_str(&r.to_string())
            )
        })
        .collect();
    let ranges: Vec<String> = a
        .ranges
        .iter()
        .map(|r| format!("[{},{},{},{}]", r.vcn, r.lcn, r.length, state_json(r.state)))
        .collect();
    format!(
        "{{\"name\":{},\"size\":{},\"resident\":{},\"compressed\":{},\"recoverable\":{},\
         \"clusters\":{},\"free\":{},\"owned\":{},\"allocated\":{},\"unknown\":{},\"shared\":{},\
         \"unmapped\":{},\"sparse\":{},\"possibly_overwritten\":{},\"owners\":[{}],\
         \"shared_with\":[{}],\"reason\":{},\"reasons\":[{}],\"ranges\":[{}],\
         \"ranges_truncated\":{}}}",
        json_str(&a.name),
        a.size,
        a.resident,
        a.compressed,
        json_str(a.recoverability.code()),
        a.clusters,
        a.free,
        a.owned,
        a.allocated,
        a.unknown,
        a.shared,
        a.unmapped,
        a.sparse,
        a.possibly_overwritten(),
        owners.join(","),
        shared.join(","),
        json_str(&s.summary(a)),
        reasons.join(","),
        ranges.join(","),
        a.ranges_truncated
    )
}

fn entry_json(s: &DeletedScan, e: &DeletedEntry) -> String {
    let streams: Vec<String> = e.streams.iter().map(|a| stream_json(s, a)).collect();
    let anomalies: Vec<String> = e
        .anomalies
        .iter()
        .map(|a| {
            format!(
                "{{\"code\":{},\"message\":{}}}",
                json_str(a.code()),
                json_str(&a.to_string())
            )
        })
        .collect();
    let list = |v: &[u64]| v.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    format!(
        "{{\"record\":{},\"sequence\":{},\"type\":{},\"name\":{},\"namespace\":{},\"path\":{},\
         \"parent\":{{\"record\":{},\"sequence\":{},\"state\":{}}},\"size\":{},\
         \"allocated_size\":{},\"times\":{{\"created\":{},\"modified\":{},\"mft_modified\":{},\
         \"accessed\":{}}},\"extensions\":[{}],\"backref_extensions\":[{}],\"anomalies\":[{}],\
         \"recoverable\":{},\"streams\":[{}]}}",
        e.record,
        e.sequence,
        json_str(if e.is_directory { "dir" } else { "file" }),
        json_str(&e.name),
        json_str(e.namespace.code()),
        e.path.as_deref().map_or_else(|| "null".into(), json_str),
        e.parent.record,
        e.parent.sequence,
        json_str(e.parent_state.code()),
        e.size,
        e.allocated_size,
        time_json(e.times.created),
        time_json(e.times.modified),
        time_json(e.times.mft_modified),
        time_json(e.times.accessed),
        list(&e.extensions),
        list(&e.backref_extensions),
        anomalies.join(","),
        e.recoverability()
            .map_or_else(|| "null".into(), |r| json_str(r.code())),
        streams.join(",")
    )
}

fn scan_json(s: &DeletedScan) -> String {
    let counts: Vec<String> = RecordState::ALL
        .iter()
        .map(|st| {
            format!(
                "{}:{}",
                json_str(st.code()),
                s.counts.get(st).copied().unwrap_or(0)
            )
        })
        .collect();
    let entries: Vec<String> = s.entries.iter().map(|e| entry_json(s, e)).collect();
    let problems: Vec<String> = s
        .problems
        .iter()
        .map(|p| {
            format!(
                "{{\"record\":{},\"state\":{},\"detail\":{}}}",
                p.record,
                json_str(p.state.code()),
                p.detail.as_deref().map_or_else(|| "null".into(), json_str)
            )
        })
        .collect();
    let bitmap = s.bitmap_issue.as_ref().map_or_else(
        || "null".into(),
        |b| {
            format!(
                "{{\"code\":{},\"message\":{}}}",
                json_str(b.code()),
                json_str(&b.to_string())
            )
        },
    );
    format!(
        "{{\"records\":{},\"counts\":{{{}}},\"free_records\":{},\"loose_extensions\":{},\
         \"entries_truncated\":{},\"claims_truncated\":{},\"directories_truncated\":{},\
         \"bitmap_issue\":{},\"entries\":[{}],\"problems\":[{}],\"problems_truncated\":{},\
         \"index_entries\":\"pending\"}}",
        s.records,
        counts.join(","),
        s.free_records,
        s.loose_extensions,
        s.entries_truncated,
        s.claims_truncated,
        s.directories_truncated,
        bitmap,
        entries.join(","),
        problems.join(","),
        s.problems_truncated
    )
}

fn scan_text(s: &DeletedScan) {
    let dirs = s.entries.iter().filter(|e| e.is_directory).count();
    println!(
        "Borrados en la MFT: {} ({} ficheros, {} directorios) de {} registros; {} registros \
         libres; {} extensiones no en uso",
        s.entries.len(),
        s.entries.len() - dirs,
        dirs,
        s.records,
        s.free_records,
        s.loose_extensions
    );
    if let Some(b) = &s.bitmap_issue {
        println!("AVISO: {b}");
    }
    if s.entries_truncated {
        println!("AVISO: demasiados borrados: lista incompleta");
    }
    if s.claims_truncated {
        println!("AVISO: índice de propietarios lleno: algunos dueños no se nombran");
    }
    println!(
        "{:>8} {:>5} T  {:<11} {:>12}  RUTA",
        "REGISTRO", "SEC", "RECUPERABLE", "TAMAÑO"
    );
    for e in &s.entries {
        let rec = e.recoverability().map_or("-", |r| r.label());
        println!(
            "{:>8} {:>5} {}  {:<11} {:>12}  {}",
            e.record,
            e.sequence,
            if e.is_directory { "D" } else { "F" },
            rec,
            e.size,
            e.display_path()
        );
        if e.parent_state.is_orphan() {
            println!("{:28}{}", "", e.parent_state);
        }
        for a in &e.streams {
            let label = if a.name.is_empty() {
                String::new()
            } else {
                format!("flujo «{}» ({}): ", a.name, a.recoverability.label())
            };
            if !a.name.is_empty() || a.recoverability != mr_api::mr_fs_ntfs::Recoverability::Full {
                println!("{:28}{label}{}", "", s.summary(a));
            }
        }
        for an in &e.anomalies {
            println!("{:28}[{}] {an}", "", an.code());
        }
    }
    if !s.problems.is_empty() {
        println!("Registros no interpretables (nunca se listan como borrados ni se recuperan):");
        for p in &s.problems {
            println!(
                "  {}: {}{}",
                p.record,
                p.state.label(),
                p.detail
                    .as_deref()
                    .map(|d| format!(": {d}"))
                    .unwrap_or_default()
            );
        }
        if s.problems_truncated {
            println!("  ... y más");
        }
    }
    println!(
        "Entradas INDX residuales: pendientes (se añadirán tras el árbol de directorios, #22)."
    );
}

/// `ntfs-deleted`: lista los borrados con su recuperabilidad. Código 3 si hay registros de la MFT
/// ilegibles o `$Bitmap` no es utilizable (la evaluación no es completa).
pub fn list(v: NtfsVolume, json: bool) -> Result<(), (u8, String)> {
    let op = |e: &dyn std::fmt::Display| (1u8, e.to_string());
    let task = mr_api::start_ntfs_deleted_task(Arc::new(v)).map_err(|e| op(&e))?;
    while task.state() == TaskState::Running {
        let (d, t) = task.progress();
        if !json && t > 0 {
            eprint!("\r{d}/{t}");
        }
        thread::sleep(Duration::from_millis(100));
    }
    if !json {
        eprint!("\r{:30}\r", "");
    }
    let s = task.wait().map_err(|e| op(&e))?;
    if json {
        println!("{}", scan_json(&s));
    } else {
        scan_text(&s);
    }
    let unreadable = s
        .problems
        .iter()
        .filter(|p| p.state == RecordState::Unreadable)
        .count();
    if unreadable > 0 || s.bitmap_issue.is_some() {
        return Err((
            3,
            format!(
                "evaluación incompleta: {unreadable} registros MFT ilegibles{}",
                s.bitmap_issue
                    .as_ref()
                    .map(|b| format!("; {b}"))
                    .unwrap_or_default()
            ),
        ));
    }
    Ok(())
}

fn print_report(r: &RecoveryReport, out: &Path) {
    let what = if r.stream.is_empty() {
        "flujo principal".to_string()
    } else {
        format!("flujo «{}»", r.stream)
    };
    if r.in_use {
        eprintln!(
            "AVISO: el registro {} está en uso (no es un borrado): se copia tal cual.",
            r.record
        );
    }
    if let (Some(a), Some(sum)) = (&r.assessment, &r.summary) {
        eprintln!(
            "Registro {} (secuencia {}), {what}: recuperabilidad {}: {sum}",
            r.record,
            r.sequence,
            a.recoverability.label()
        );
    }
    eprintln!("{} bytes escritos en {}", r.copy.bytes, out.display());
    let po = r.copy.count(RangeState::PossiblyOverwritten);
    if po > 0 {
        eprintln!(
            "AVISO: {po} bytes POSIBLEMENTE SOBRESCRITOS (copiados tal como están en el disco; \
             pueden ser de otro fichero):"
        );
        for x in r.suspect.iter().take(MAX_LISTED_RANGES) {
            let who = match x.state {
                ClusterState::Owned { owner } => {
                    format!("asignados a {}", owner_label(&r.owners, owner))
                }
                ClusterState::Allocated => "asignados en $Bitmap sin dueño conocido".into(),
                ClusterState::Unknown => "asignación desconocida".into(),
                ClusterState::SharedWithDeleted { record } => {
                    format!("reclamados también por el borrado {record}")
                }
                ClusterState::Free => "libres".into(),
            };
            eprintln!(
                "  bytes {}..{} ({} B): LCN {}..{} ({} clústeres) {who}",
                x.offset,
                x.offset + x.length,
                x.length,
                x.lcn,
                x.lcn + x.clusters,
                x.clusters
            );
        }
        if r.suspect.len() > MAX_LISTED_RANGES {
            eprintln!("  ... y más tramos");
        }
    }
    let filled = r.copy.filled_bytes();
    if filled > 0 {
        eprintln!(
            "AVISO: {filled} bytes NO son datos del origen: llevan el patrón de relleno BAD! \
             (solo en estos tramos):"
        );
        let filled: Vec<_> = r
            .copy
            .untrusted
            .iter()
            .filter(|x| x.state.is_filled())
            .collect();
        for x in filled.iter().take(MAX_LISTED_RANGES) {
            eprintln!("  {x}");
        }
        if filled.len() > MAX_LISTED_RANGES || r.copy.truncated {
            eprintln!("  ... y más tramos (los recuentos son exactos)");
        }
    }
}

/// `ntfs-recover`: recupera un flujo de un registro en `out`. Código 3 si algún byte no es
/// fiable (relleno o posiblemente sobrescrito), con los tramos en la salida de errores.
pub fn recover(
    v: &NtfsVolume,
    src: &mr_api::Source,
    record: u64,
    stream: &str,
    out: &Path,
    overwrite: bool,
) -> Result<(), (u8, String)> {
    let r = mr_api::ntfs_recover_stream(
        src,
        v,
        record,
        stream,
        out,
        overwrite,
        &mr_api::task::TaskContext::detached(),
    )
    .map_err(|e| match e {
        mr_api::ApiError::OutputExists(_) => {
            (2u8, format!("{e} (use --overwrite para sustituirla)"))
        }
        e => (1u8, e.to_string()),
    })?;
    print_report(&r, out);
    if r.is_clean() {
        return Ok(());
    }
    Err((
        3,
        format!(
            "{} bytes no fiables ({} posiblemente sobrescritos, {} con patrón de relleno)",
            r.copy.untrusted_bytes(),
            r.copy.count(RangeState::PossiblyOverwritten),
            r.copy.filled_bytes()
        ),
    ))
}
