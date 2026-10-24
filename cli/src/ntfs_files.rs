//! `ntfs-cat` y `ntfs-stat`: contenido y atributos de un fichero NTFS por número de registro MFT
//! (#21) o por ruta (#22); `ntfs-ls`: directorios y árbol (#22).

use std::io::Write;
use std::path::Path;

use mr_api::mr_fs_ntfs::attr::type_name;
use mr_api::mr_fs_ntfs::file::{AttrValue, SegmentKind, StreamLayout};
use mr_api::mr_fs_ntfs::runs::{merge_contiguous, sparse_ranges};
use mr_api::mr_fs_ntfs::values::{filetime_to_unix, Times};
use std::collections::HashSet;
use std::sync::Arc;

use mr_api::mr_fs_ntfs::dir::{DirEntry, ROOT_RECORD};
use mr_api::mr_fs_ntfs::{
    CopyReport, EntryState, NtfsFile, NtfsVolume, RecordState, TreeAnomaly, TreeIssue, TreeNode,
    TreeOptions, TreeStatus,
};

use crate::json_str;

/// Máximo de tramos no fiables que se listan en la salida de errores.
const MAX_LISTED_RANGES: usize = 50;

/// Fecha y hora UTC (ISO 8601) de una marca FILETIME; `null`/`(anterior a 1970)` si no cabe.
pub fn filetime_iso(ft: u64) -> Option<String> {
    let (secs, frac) = filetime_to_unix(ft)?;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Días desde 1970-01-01 a fecha civil (algoritmo público de calendario proléptico gregoriano).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    Some(format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{frac:07}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    ))
}

fn time_json(ft: u64) -> String {
    filetime_iso(ft).map_or_else(|| "null".into(), |t| json_str(&t))
}

fn time_text(ft: u64) -> String {
    filetime_iso(ft).unwrap_or_else(|| format!("(FILETIME {ft})"))
}

fn times_json(t: &Times) -> String {
    format!(
        "\"created\":{},\"modified\":{},\"mft_modified\":{},\"accessed\":{}",
        time_json(t.created),
        time_json(t.modified),
        time_json(t.mft_modified),
        time_json(t.accessed)
    )
}

fn stream_label(name: &str) -> String {
    if name.is_empty() {
        "(principal)".into()
    } else {
        format!("«{name}»")
    }
}

/// `ntfs-cat`: escribe el flujo en `out` (o en la salida estándar) y avisa de los tramos no
/// fiables. Devuelve el código de salida 3 si los hay.
pub fn cat(
    v: &NtfsVolume,
    src: &mr_api::Source,
    record: u64,
    stream: &str,
    out: Option<&Path>,
    overwrite: bool,
) -> Result<(), (u8, String)> {
    let op = |e: &dyn std::fmt::Display| (1u8, e.to_string());
    // Se abre el flujo antes de crear la salida: un error no deja ficheros vacíos.
    let s = mr_api::ntfs_open_stream(v, record, stream).map_err(|e| op(&e))?;
    if !s.record_in_use() {
        eprintln!(
            "AVISO: el registro {record} no está en uso (fichero borrado): sus clústeres pueden \
             haberse reutilizado; el contenido se lee tal como está en el disco."
        );
    }
    let report: CopyReport = match out {
        Some(p) => {
            // Política común de salida (mr-api): temporal en el mismo directorio que se publica
            // al terminar sin sustituir nunca un fichero existente (salvo --overwrite), tampoco
            // uno creado entretanto; si falla, el temporal se borra y no queda nada con el
            // nombre final.
            let out_err = |e: mr_api::ApiError| match e {
                mr_api::ApiError::OutputExists(_) => {
                    (2u8, format!("{e} (use --overwrite para sustituirla)"))
                }
                e => op(&e),
            };
            let mut w = mr_api::create_output(src, p, overwrite).map_err(out_err)?;
            let r = s.copy_to(&mut w).map_err(|e| op(&e))?;
            w.commit().map_err(out_err)?;
            r
        }
        None => {
            let stdout = std::io::stdout();
            let mut w = std::io::BufWriter::new(stdout.lock());
            let r = s.copy_to(&mut w).map_err(|e| op(&e))?;
            w.flush().map_err(|e| op(&e))?;
            r
        }
    };
    let bad = report.untrusted_bytes();
    if bad == 0 {
        if let Some(p) = out {
            eprintln!("{} bytes escritos en {}", report.bytes, p.display());
        }
        return Ok(());
    }
    eprintln!(
        "AVISO: {bad} de {} bytes NO son datos del origen: llevan el patrón de relleno BAD! \
         (solo en estos tramos):",
        report.bytes
    );
    for r in report.untrusted.iter().take(MAX_LISTED_RANGES) {
        eprintln!("  {r}");
    }
    let hidden = report.untrusted.len().saturating_sub(MAX_LISTED_RANGES);
    if hidden > 0 || report.truncated {
        eprintln!("  ... y más tramos (los recuentos son exactos)");
    }
    Err((
        3,
        format!("{bad} bytes no fiables en el flujo (patrón de relleno, no datos)"),
    ))
}

fn runs_json(l: &StreamLayout) -> (String, String) {
    let runs: Vec<String> = merge_contiguous(&l.runs)
        .iter()
        .filter_map(|r| r.lcn.map(|lcn| format!("[{},{lcn},{}]", r.vcn, r.length)))
        .collect();
    let sparse: Vec<String> = sparse_ranges(&l.runs)
        .iter()
        .map(|(v, n)| format!("[{v},{n}]"))
        .collect();
    (
        format!("[{}]", runs.join(",")),
        format!("[{}]", sparse.join(",")),
    )
}

fn unmapped(l: &StreamLayout) -> Vec<(u64, u64)> {
    l.segments
        .iter()
        .filter(|s| s.kind == SegmentKind::Unmapped)
        .map(|s| (s.vcn, s.length))
        .collect()
}

fn stream_json(v: &NtfsVolume, f: &NtfsFile, name: &str) -> String {
    match v.stream_of(f, name) {
        Err(e) => format!(
            "{{\"name\":{},\"error\":{{\"code\":{},\"message\":{}}}}}",
            json_str(name),
            json_str(e.code()),
            json_str(&e.to_string())
        ),
        Ok(s) => {
            let l = s.layout();
            let (runs, sparse) = runs_json(l);
            let issues: Vec<String> = l
                .issues
                .iter()
                .map(|i| {
                    format!(
                        "{{\"code\":{},\"message\":{}}}",
                        json_str(i.code()),
                        json_str(&i.to_string())
                    )
                })
                .collect();
            let extents: Vec<String> = l
                .extents
                .iter()
                .map(|e| {
                    format!(
                        "{{\"record\":{},\"start_vcn\":{},\"last_vcn\":{},\"valid\":{}}}",
                        e.record,
                        e.start_vcn,
                        e.last_vcn,
                        e.error.is_none()
                    )
                })
                .collect();
            let unm: Vec<String> = unmapped(l)
                .iter()
                .map(|(a, n)| format!("[{a},{n}]"))
                .collect();
            format!(
                "{{\"name\":{},\"resident\":{},\"size\":{},\"allocated_size\":{},\"initialized_size\":{},\"compressed_size\":{},\"compressed\":{},\"sparse_flag\":{},\"compression_unit_clusters\":{},\"runs\":{runs},\"sparse\":{sparse},\"unmapped\":[{}],\"extents\":[{}],\"issues\":[{}],\"error\":null}}",
                json_str(name),
                l.is_resident(),
                l.data_size,
                l.allocated_size,
                l.initialized_size,
                l.compressed_size
                    .map_or_else(|| "null".into(), |c| c.to_string()),
                l.is_compressed(),
                l.is_sparse_flag(),
                l.compression_unit_clusters,
                unm.join(","),
                extents.join(","),
                issues.join(",")
            )
        }
    }
}

/// JSON de `ntfs-stat`.
pub fn stat_json(v: &NtfsVolume, f: &NtfsFile) -> String {
    let h = &f.header;
    let si = f.standard_information().map_or_else(
        || "null".into(),
        |s| {
            format!(
                "{{{},\"file_attributes\":{},\"owner_id\":{},\"security_id\":{},\"usn\":{}}}",
                times_json(&s.times),
                s.file_attributes,
                s.v3.map_or_else(|| "null".into(), |x| x.owner_id.to_string()),
                s.v3.map_or_else(|| "null".into(), |x| x.security_id.to_string()),
                s.v3.map_or_else(|| "null".into(), |x| x.usn.to_string())
            )
        },
    );
    let names: Vec<String> = f
        .file_names()
        .iter()
        .map(|n| {
            format!(
                "{{\"name\":{},\"namespace\":\"{}\",\"parent\":{},\"parent_sequence\":{},\"allocated_size\":{},\"data_size\":{},\"file_attributes\":{},{}}}",
                json_str(&n.name),
                n.namespace.code(),
                n.parent.record,
                n.parent.sequence,
                n.allocated_size,
                n.data_size,
                n.file_attributes,
                times_json(&n.times)
            )
        })
        .collect();
    let attrs: Vec<String> = f
        .attributes
        .iter()
        .map(|a| {
            let form = match &a.value {
                AttrValue::Resident { value, .. } => {
                    format!("\"resident\":true,\"value_length\":{}", value.len())
                }
                AttrValue::NonResident(n) => format!(
                    "\"resident\":false,\"start_vcn\":{},\"last_vcn\":{},\"allocated_size\":{},\"data_size\":{},\"initialized_size\":{}",
                    n.start_vcn, n.last_vcn, n.allocated_size, n.data_size, n.initialized_size
                ),
            };
            format!(
                "{{\"type\":{},\"type_name\":{},\"name\":{},\"id\":{},\"record\":{},\"flags\":{},{form}}}",
                a.type_code,
                type_name(a.type_code).map_or_else(|| "null".into(), json_str),
                json_str(&a.name),
                a.id,
                a.record,
                a.flags
            )
        })
        .collect();
    let streams: Vec<String> = f
        .data_streams()
        .iter()
        .map(|n| stream_json(v, f, n))
        .collect();
    let anomalies: Vec<String> = f
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
    let ext: Vec<String> = f.extensions.iter().map(u64::to_string).collect();
    format!(
        "{{\"record\":{},\"sequence\":{},\"in_use\":{},\"directory\":{},\"link_count\":{},\"lsn\":{},\"extensions\":[{}],\"attribute_list_entries\":{},\"standard_information\":{si},\"file_names\":[{}],\"object_id\":{},\"attributes\":[{}],\"streams\":[{}],\"anomalies\":[{}]}}",
        f.number,
        h.sequence,
        f.in_use(),
        f.is_directory(),
        h.link_count,
        h.lsn,
        ext.join(","),
        f.attribute_list.len(),
        names.join(","),
        f.object_id().map_or_else(
            || "null".into(),
            |o| json_str(&o.object_id.iter().map(|b| format!("{b:02x}")).collect::<String>())
        ),
        attrs.join(","),
        streams.join(","),
        anomalies.join(",")
    )
}

/// Texto de `ntfs-stat`.
pub fn stat_text(v: &NtfsVolume, f: &NtfsFile) {
    let h = &f.header;
    println!(
        "Registro {} (secuencia {}): {}, {}, {} enlaces, LSN {}",
        f.number,
        h.sequence,
        if f.in_use() {
            "en uso"
        } else {
            "NO EN USO (borrado o libre)"
        },
        if f.is_directory() {
            "directorio"
        } else {
            "fichero"
        },
        h.link_count,
        h.lsn
    );
    if !f.extensions.is_empty() || !f.attribute_list.is_empty() {
        println!(
            "  $ATTRIBUTE_LIST: {} entradas; registros de extensión usados: {:?}",
            f.attribute_list.len(),
            f.extensions
        );
    }
    if let Some(s) = f.standard_information() {
        println!(
            "  $STANDARD_INFORMATION: creado {}, modificado {}, MFT {}, acceso {}, atributos 0x{:X}",
            time_text(s.times.created),
            time_text(s.times.modified),
            time_text(s.times.mft_modified),
            time_text(s.times.accessed),
            s.file_attributes
        );
    }
    for n in f.file_names() {
        println!(
            "  $FILE_NAME [{}]: «{}» en el registro {} (secuencia {}), tamaño {} (asignado {}), modificado {}",
            n.namespace.code(),
            n.name,
            n.parent.record,
            n.parent.sequence,
            n.data_size,
            n.allocated_size,
            time_text(n.times.modified)
        );
    }
    if let Some(o) = f.object_id() {
        let hex: String = o.object_id.iter().map(|b| format!("{b:02x}")).collect();
        println!("  $OBJECT_ID: {hex}");
    }
    println!("Atributos:");
    for a in &f.attributes {
        let name = if a.name.is_empty() {
            String::new()
        } else {
            format!(" «{}»", a.name)
        };
        let form = match &a.value {
            AttrValue::Resident { value, .. } => format!("residente, {} B", value.len()),
            AttrValue::NonResident(n) => format!(
                "no residente, VCN {}..={}, datos {} B",
                n.start_vcn, n.last_vcn, n.data_size
            ),
        };
        println!(
            "  0x{:03X} {}{name}: {form}, id {}, registro {}, indicadores 0x{:04X}",
            a.type_code,
            type_name(a.type_code).unwrap_or("(desconocido)"),
            a.id,
            a.record,
            a.flags
        );
    }
    println!("Flujos de datos:");
    if f.data_streams().is_empty() {
        println!("  (ninguno)");
    }
    for n in f.data_streams() {
        let label = stream_label(&n);
        match v.stream_of(f, &n) {
            Err(e) => println!("  {label}: NO LEGIBLE: {e}"),
            Ok(s) => {
                let l = s.layout();
                let mut kind = vec![if l.is_resident() {
                    "residente"
                } else {
                    "no residente"
                }];
                if l.is_compressed() {
                    kind.push("comprimido LZNT1");
                }
                if l.is_sparse_flag() {
                    kind.push("disperso");
                }
                println!(
                    "  {label}: {} B, {}, asignado {} B, inicializado {} B",
                    l.data_size,
                    kind.join(", "),
                    l.allocated_size,
                    l.initialized_size
                );
                for r in merge_contiguous(&l.runs) {
                    match r.lcn {
                        Some(lcn) => println!(
                            "    run: VCN {:<8} LCN {:<10} {} clústeres",
                            r.vcn, lcn, r.length
                        ),
                        None => println!(
                            "    run: VCN {:<8} disperso   {} clústeres",
                            r.vcn, r.length
                        ),
                    }
                }
                for (a, n) in unmapped(l) {
                    println!("    SIN MAPA: VCN {a}, {n} clústeres (no fiable)");
                }
                for i in &l.issues {
                    println!("    aviso {}: {i}", i.code());
                }
            }
        }
    }
    if !f.anomalies.is_empty() {
        println!("Anomalías:");
        for a in &f.anomalies {
            println!("  {}: {a}", a.code());
        }
    }
}

// ---------------------------------------------------------------------------------------------
// ntfs-ls (#22)
// ---------------------------------------------------------------------------------------------

fn entry_type(e: &DirEntry) -> &'static str {
    if e.is_directory {
        "dir"
    } else {
        "file"
    }
}

fn entry_json(e: &DirEntry, path: &str) -> String {
    format!(
        "{{\"name\":{},\"path\":{},\"type\":\"{}\",\"record\":{},\"sequence\":{},\"parent\":{},\"size\":{},\"allocated_size\":{},{},\"attributes\":{},\"namespace\":\"{}\",\"dos_name\":{},\"name_lossy\":{},\"links\":{},\"system\":{},\"state\":\"{}\",\"state_detail\":{}}}",
        json_str(&e.name),
        json_str(path),
        entry_type(e),
        e.reference.record,
        e.reference.sequence,
        e.parent.record,
        e.size,
        e.allocated_size,
        times_json(&e.times),
        e.file_attributes,
        e.namespace.code(),
        e.dos_name.as_deref().map_or_else(|| "null".into(), json_str),
        e.name_lossy,
        e.link_count,
        e.system,
        e.state.code(),
        if e.state.is_live() {
            "null".into()
        } else {
            json_str(&e.state.to_string())
        }
    )
}

fn entry_text(e: &DirEntry, shown: &str) {
    let modified = filetime_iso(e.times.modified).unwrap_or_else(|| "-".into());
    let state = if e.state.is_live() {
        String::new()
    } else {
        format!("   [{}: {}]", e.state.code(), e.state)
    };
    println!(
        "{:<4} {:>12}  {:<28}  {:>8}-{:<5} {}{}{}",
        if e.is_directory { "DIR" } else { "" },
        if e.is_directory {
            String::new()
        } else {
            e.size.to_string()
        },
        modified,
        e.reference.record,
        e.reference.sequence,
        shown,
        if e.system { "  (sistema)" } else { "" },
        state
    );
}

fn join(parent: &str, name: &str) -> String {
    if parent == "/" {
        format!("/{name}")
    } else {
        format!("{parent}/{name}")
    }
}

/// `ntfs-ls`: contenido de un directorio (o la entrada de un fichero) y, con `-r`, el subárbol
/// (con los huérfanos si se lista la raíz). Código 3 si hay nodos o registros ilegibles.
pub fn ls(
    v: &Arc<NtfsVolume>,
    path: &str,
    recursive: bool,
    json: bool,
) -> Result<(), (u8, String)> {
    let op = |e: &dyn std::fmt::Display| (1u8, e.to_string());
    let target = mr_api::ntfs_resolve(v, path).map_err(|e| op(&e))?;
    // Ruta canónica (nombres tal como están en el volumen).
    let canonical = mr_api::ntfs_path_of(v, target.record()).unwrap_or_else(|_| path.to_string());
    if !target.is_directory {
        if json {
            println!(
                "{{\"path\":{},\"entries\":[{}],\"anomalies\":[]}}",
                json_str(&canonical),
                entry_json(&target, &canonical)
            );
        } else {
            entry_text(&target, &canonical);
        }
        return Ok(());
    }
    let unreadable = if recursive {
        ls_tree(v, &target, &canonical, json)?
    } else {
        let l = mr_api::ntfs_list_dir(v, target.record()).map_err(|e| op(&e))?;
        if json {
            let entries: Vec<String> = l
                .entries
                .iter()
                .map(|e| entry_json(e, &join(&canonical, &e.name)))
                .collect();
            let anomalies: Vec<String> = l
                .anomalies
                .iter()
                .map(|a| {
                    format!(
                        "{{\"code\":\"{}\",\"node\":{},\"message\":{}}}",
                        a.issue.code(),
                        json_str(&a.node.to_string()),
                        json_str(&a.issue.to_string())
                    )
                })
                .collect();
            println!(
                "{{\"path\":{},\"directory\":{},\"entries\":[{}],\"anomalies\":[{}]}}",
                json_str(&canonical),
                target.record(),
                entries.join(","),
                anomalies.join(",")
            );
        } else {
            println!(
                "Directorio {canonical} (registro {}): {} entradas, {} vivas",
                target.record(),
                l.entries.len(),
                l.live().count()
            );
            for e in &l.entries {
                entry_text(e, &e.name);
            }
            for a in &l.anomalies {
                println!("ANOMALÍA [{}] {a}", a.issue.code());
            }
        }
        l.has_unreadable()
    };
    if unreadable {
        return Err((
            3,
            "nodos de índice o registros con sectores ilegibles: no se interpretaron".into(),
        ));
    }
    Ok(())
}

/// `ntfs-ls -r`: el árbol del volumen (tarea con progreso) recortado al subárbol pedido.
/// Devuelve si hubo nodos o registros ilegibles en él.
fn ls_tree(
    v: &Arc<NtfsVolume>,
    target: &DirEntry,
    canonical: &str,
    json: bool,
) -> Result<bool, (u8, String)> {
    let op = |e: &dyn std::fmt::Display| (1u8, e.to_string());
    let task = mr_api::start_ntfs_tree_task(
        Arc::clone(v),
        TreeOptions {
            orphans: target.record() == ROOT_RECORD,
            ..TreeOptions::default()
        },
    )
    .map_err(|e| op(&e))?;
    while task.state() == mr_api::task::TaskState::Running {
        let (d, t) = task.progress();
        if !json && t > 0 {
            eprint!("\r{d}/{t}");
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    if !json {
        eprintln!();
    }
    let t = task.wait().map_err(|e| op(&e))?;
    // Subárbol: nodos que tienen como antecesor el del directorio pedido.
    let start = t
        .nodes
        .iter()
        .position(|n| n.entry.record() == target.record() && n.entry.state.is_live())
        .ok_or_else(|| op(&format!("{canonical} no se alcanza desde la raíz")))?;
    let mut inside = vec![false; t.nodes.len()];
    inside[start] = true;
    for i in start + 1..t.nodes.len() {
        if let Some(p) = t.nodes[i].parent {
            inside[i] = inside[p];
        }
    }
    let dirs: HashSet<u64> = t
        .nodes
        .iter()
        .enumerate()
        .filter(|(i, n)| inside[*i] && n.entry.is_directory)
        .map(|(_, n)| n.entry.record())
        .collect();
    let anomalies: Vec<&TreeAnomaly> = t
        .anomalies
        .iter()
        .filter(|a| dirs.contains(&a.directory))
        .collect();
    let unreadable = anomalies.iter().any(|a| match &a.issue {
        TreeIssue::Index(i) => i.issue.is_unreadable(),
        _ => false,
    }) || t.nodes.iter().enumerate().any(|(i, n)| {
        inside[i]
            && matches!(
                &n.entry.state,
                EntryState::RecordUnusable { state, .. } if *state == RecordState::Unreadable
            )
    });
    let nodes: Vec<&TreeNode> = t
        .nodes
        .iter()
        .enumerate()
        .filter(|(i, _)| inside[*i] && *i != start)
        .map(|(_, n)| n)
        .collect();
    if json {
        let entries: Vec<String> = nodes
            .iter()
            .map(|n| entry_json(&n.entry, &n.path))
            .collect();
        let anoms: Vec<String> = anomalies
            .iter()
            .map(|a| {
                format!(
                    "{{\"code\":\"{}\",\"path\":{},\"message\":{}}}",
                    a.issue.code(),
                    json_str(&a.path),
                    json_str(&a.issue.to_string())
                )
            })
            .collect();
        let orphans: Vec<String> = t
            .orphans
            .iter()
            .map(|o| {
                format!(
                    "{{\"record\":{},\"sequence\":{},\"name\":{},\"parent\":{},\"directory\":{},\"reason\":\"{}\",\"message\":{}}}",
                    o.record,
                    o.sequence,
                    json_str(&o.name),
                    o.parent.record,
                    o.is_directory,
                    o.reason.code(),
                    json_str(&o.reason.to_string())
                )
            })
            .collect();
        println!(
            "{{\"path\":{},\"status\":\"{}\",\"entries\":[{}],\"anomalies\":[{}],\"orphans\":[{}]}}",
            json_str(canonical),
            t.status.code(),
            entries.join(","),
            anoms.join(","),
            orphans.join(",")
        );
    } else {
        for n in &nodes {
            entry_text(&n.entry, &n.path);
        }
        for a in &anomalies {
            println!("ANOMALÍA [{}] {}: {}", a.issue.code(), a.path, a.issue);
        }
        for o in &t.orphans {
            println!(
                "HUÉRFANO registro {} «{}» (padre {}): [{}] {}",
                o.record,
                o.name,
                o.parent.record,
                o.reason.code(),
                o.reason
            );
        }
        if t.status != TreeStatus::Complete {
            println!("Árbol incompleto: {}", t.status.code());
        }
    }
    Ok(unreadable)
}
