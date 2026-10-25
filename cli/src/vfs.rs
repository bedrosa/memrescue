//! `vfs-ls`: árbol virtual de un volumen NTFS (#25): vivos, borrados, huérfanos y metadatos con
//! la categoría, el estado de los datos y la fiabilidad del nombre de cada entrada.

use mr_api::mr_fs_ntfs::NtfsVolume;
use mr_api::mr_vfs::filter::{parse_categories, parse_data_states, Filter, NameRules};
use mr_api::mr_vfs::{flags, NodeId, NodeInfo, NodeKind, NtfsBuildOptions, Vfs};
use mr_api::task::TaskContext;

use crate::json_str;
use crate::ntfs_files::filetime_iso;

/// Opciones de `vfs-ls`.
pub struct LsOptions<'a> {
    pub path: &'a str,
    pub recursive: bool,
    pub json: bool,
    pub deleted: bool,
    pub masks: Option<&'a str>,
    pub categories: Option<&'a str>,
    pub states: Option<&'a str>,
}

fn time_json(ft: u64) -> String {
    if ft == 0 {
        return "null".into();
    }
    filetime_iso(ft).map_or_else(|| "null".into(), |t| json_str(&t))
}

fn node_json(v: &Vfs, n: &NodeInfo) -> String {
    let o = n.origin;
    let fl: Vec<String> = flags::names(n.flags)
        .into_iter()
        .map(|f| format!("\"{f}\""))
        .collect();
    let units: Vec<String> = v.name_units(n.id).iter().map(u16::to_string).collect();
    format!(
        "{{\"id\":{},\"path\":{},\"name\":{},\"name_utf16\":[{}],\"kind\":\"{}\",\"category\":\"{}\",\"name_reliability\":\"{}\",\"data\":{},\"size\":{},\"allocated_size\":{},\"record\":{},\"sequence\":{},\"parent_record\":{},\"parent_sequence\":{},\"flags\":[{}],\"times\":{{\"created\":{},\"modified\":{},\"changed\":{},\"accessed\":{}}},\"attributes\":{}}}",
        n.id.0,
        json_str(&v.path(n.id)),
        json_str(&v.name(n.id)),
        units.join(","),
        n.kind.code(),
        n.category.code(),
        n.name_reliability.code(),
        n.data
            .map_or_else(|| "null".into(), |d| format!("\"{}\"", d.code())),
        n.size,
        n.allocated_size,
        o.map_or_else(|| "null".into(), |o| o.record.to_string()),
        o.map_or_else(|| "null".into(), |o| o.sequence.to_string()),
        o.map_or_else(|| "null".into(), |o| o.parent_record.to_string()),
        o.map_or_else(|| "null".into(), |o| o.parent_sequence.to_string()),
        fl.join(","),
        time_json(n.times.created),
        time_json(n.times.modified),
        time_json(n.times.changed),
        time_json(n.times.accessed),
        n.attributes
    )
}

fn node_text(v: &Vfs, n: &NodeInfo) {
    let path = v.path(n.id);
    let (size, data) = if n.kind == NodeKind::File {
        (
            n.size.to_string(),
            n.data.map_or("-", |d| d.code()).to_string(),
        )
    } else {
        ("-".into(), "-".into())
    };
    let suffix = if n.kind.is_container() && n.id != NodeId::ROOT {
        "/"
    } else {
        ""
    };
    let mut marks = Vec::new();
    for (bit, m) in [
        (flags::RECORD_DAMAGED, "dañado"),
        (flags::NAME_LOSSY, "nombre con UTF-16 suelto"),
        (flags::PARENT_REUSED, "padre reutilizado"),
        (flags::DUPLICATE_NAME, "nombre duplicado"),
        (flags::DIR_INCOMPLETE, "directorio incompleto"),
        (flags::CYCLE_CUT, "ciclo cortado"),
    ] {
        if n.flags & bit != 0 {
            marks.push(m);
        }
    }
    let marks = if marks.is_empty() {
        String::new()
    } else {
        format!("  [{}]", marks.join(", "))
    };
    println!(
        "{:<10} {:<23} {:<10} {:>14}  {path}{suffix}{marks}",
        n.category.code(),
        data,
        n.name_reliability.code(),
        size
    );
}

/// `vfs-ls`: construye el árbol virtual y lista el nodo de `path` (o su contenido) con el
/// filtro. Código 3 si el árbol no es completo o hay directorios que no se pudieron listar.
pub fn ls(vol: &NtfsVolume, o: &LsOptions<'_>) -> Result<(), (u8, String)> {
    let usage = |e: &dyn std::fmt::Display| (2u8, e.to_string());
    let mut filter = Filter {
        deleted_only: o.deleted,
        ..Filter::default()
    };
    if let Some(m) = o.masks {
        filter.names = NameRules::parse(m).map_err(|e| usage(&e))?;
        filter.files_only = true;
    }
    if let Some(c) = o.categories {
        filter.categories = parse_categories(c).map_err(|e| usage(&e))?;
    }
    if let Some(s) = o.states {
        filter.data_states = parse_data_states(s).map_err(|e| usage(&e))?;
        filter.files_only = true;
    }
    let v = mr_api::vfs_build_ntfs(vol, &NtfsBuildOptions::default(), &TaskContext::detached())
        .map_err(|e| (1u8, e.to_string()))?;
    let target = v
        .find(o.path)
        .ok_or_else(|| (1u8, format!("no existe {} en el árbol virtual", o.path)))?;
    let info = v.node(target).ok_or((1u8, "nodo no válido".to_string()))?;
    let ids: Vec<NodeId> = if info.kind == NodeKind::File {
        vec![target]
    } else {
        mr_api::vfs_query(&v, target, o.recursive, &filter).map_err(|e| usage(&e))?
    };
    let incomplete = v.is_partial() || ids.iter().any(|i| v.flags(*i) & flags::DIR_INCOMPLETE != 0);
    let s = v.stats();
    if o.json {
        let nodes: Vec<String> = ids
            .iter()
            .filter_map(|i| v.node(*i))
            .map(|n| node_json(&v, &n))
            .collect();
        let notes: Vec<String> = v
            .notes()
            .iter()
            .map(|n| {
                format!(
                    "{{\"code\":{},\"node\":{},\"text\":{}}}",
                    json_str(&n.code),
                    n.node.map_or_else(|| "null".into(), |i| i.0.to_string()),
                    json_str(&n.text)
                )
            })
            .collect();
        let cats: Vec<String> = mr_api::mr_vfs::Category::ALL
            .iter()
            .map(|c| format!("\"{}\":{}", c.code(), s.category(*c)))
            .collect();
        let states: Vec<String> = mr_api::mr_vfs::DataState::ALL
            .iter()
            .map(|d| format!("\"{}\":{}", d.code(), s.data(*d)))
            .collect();
        println!(
            "{{\"path\":{},\"target\":{},\"nodes\":[{}],\"stats\":{{\"nodes\":{},\"files\":{},\"directories\":{},\"synthetic\":{},\"deleted\":{},\"categories\":{{{}}},\"data\":{{{}}}}},\"partial\":{},\"notes\":[{}]}}",
            json_str(&v.path(target)),
            node_json(&v, &info),
            nodes.join(","),
            s.nodes,
            s.files,
            s.directories,
            s.synthetic,
            s.deleted,
            cats.join(","),
            states.join(","),
            v.is_partial(),
            notes.join(",")
        );
    } else {
        println!(
            "{:<10} {:<23} {:<10} {:>14}  RUTA",
            "CATEGORÍA", "DATOS", "NOMBRE", "TAMAÑO"
        );
        for i in &ids {
            if let Some(n) = v.node(*i) {
                node_text(&v, &n);
            }
        }
        eprintln!(
            "{} entradas listadas; árbol: {} nodos ({} vivos, {} borrados, {} huérfanos, {} de metadatos), {} ficheros íntegros, {} sin errores con procedencia no verificable, {} parciales, {} sobrescritos, {} sin datos, {} no disponibles, {} cifrados",
            ids.len(),
            s.nodes,
            s.category(mr_api::mr_vfs::Category::Live),
            s.category(mr_api::mr_vfs::Category::Deleted),
            s.category(mr_api::mr_vfs::Category::Orphan),
            s.category(mr_api::mr_vfs::Category::Metadata),
            s.data(mr_api::mr_vfs::DataState::Integro),
            s.data(mr_api::mr_vfs::DataState::IntegroNoVerificable),
            s.data(mr_api::mr_vfs::DataState::Parcial),
            s.data(mr_api::mr_vfs::DataState::Sobrescrito),
            s.data(mr_api::mr_vfs::DataState::SinDatos),
            s.data(mr_api::mr_vfs::DataState::NoDisponible),
            s.data(mr_api::mr_vfs::DataState::Cifrado),
        );
        for n in v.notes().iter().take(20) {
            eprintln!("nota: {}: {}", n.code, n.text);
        }
        if v.notes().len() > 20 {
            eprintln!("({} notas más; use --json)", v.notes().len() - 20);
        }
    }
    if incomplete {
        return Err((
            3,
            "el árbol virtual no es completo: hay directorios que no se pudieron listar o se alcanzó un límite".into(),
        ));
    }
    Ok(())
}
