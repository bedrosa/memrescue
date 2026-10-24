//! Copia del particionado (#18): `partitions --save-backup` / `find-partitions --save-backup` y
//! `partition-backup-check`. Formato en `docs/formatos/copia-particionado.md`. Nada se escribe en
//! el origen: comparar solo lee.

use std::path::Path;

use mr_api::mr_part::backup::RegionContent;
use mr_api::{SearchResult, Source};

use super::{json_str, scheme_name};

/// Código de salida cuando el origen no coincide con la copia.
pub const EXIT_DIFFERENT: u8 = 4;

type CliResult = Result<(), (u8, String)>;

fn op(e: &dyn std::fmt::Display) -> (u8, String) {
    (1, e.to_string())
}

/// Toma la copia de `src` (con la búsqueda, si se da) y la guarda en `out`.
pub fn save(src: &Source, search: Option<&SearchResult>, out: &Path, overwrite: bool) -> CliResult {
    let b = mr_api::partition_backup(src, search).map_err(|e| op(&e))?;
    let path = mr_api::save_partition_backup(src, &b, out, overwrite).map_err(|e| match e {
        mr_api::ApiError::OutputExists(_) => {
            (2u8, format!("{e} (use --overwrite para sustituirla)"))
        }
        e => op(&e),
    })?;
    let unreadable = b
        .regions
        .iter()
        .filter(|r| matches!(r.content, RegionContent::Unreadable(_)))
        .count();
    eprintln!(
        "Copia del particionado guardada en {} ({} particiones, {} regiones de tabla{}{})",
        path.display(),
        b.layout.partitions.len(),
        b.regions.len(),
        if unreadable > 0 {
            format!(", {unreadable} ilegibles sin contenido")
        } else {
            String::new()
        },
        if search.is_some() {
            ", con la búsqueda exprés"
        } else {
            ""
        }
    );
    Ok(())
}

/// Valida la copia `backup` y la compara con `src`. Código de salida [`EXIT_DIFFERENT`] si hay
/// diferencias.
pub fn check(backup: &Path, src: &Source, json: bool) -> CliResult {
    let b = mr_api::load_partition_backup(backup).map_err(|e| op(&e))?;
    let c = mr_api::compare_partition_backup(src, &b).map_err(|e| op(&e))?;
    let data_bytes: u64 = b
        .regions
        .iter()
        .filter(|r| matches!(r.content, RegionContent::Data(_)))
        .map(|r| r.length)
        .sum();
    if json {
        let diffs: Vec<String> = c
            .differences
            .iter()
            .map(|d| {
                format!(
                    "{{\"code\":{},\"message\":{}}}",
                    json_str(d.code()),
                    json_str(&d.to_string())
                )
            })
            .collect();
        println!(
            "{{\"backup\":{{\"created_by\":{},\"description\":{},\"size_bytes\":{},\"sector_size\":{},\"scheme\":{},\"partitions\":{},\"regions\":{},\"region_bytes\":{},\"search\":{}}},\"identical\":{},\"differences\":[{}]}}",
            json_str(&b.created_by),
            json_str(&b.source.description),
            b.source.size_bytes,
            b.source.sector_size,
            json_str(scheme_name(b.layout.scheme)),
            b.layout.partitions.len(),
            b.regions.len(),
            data_bytes,
            b.search.is_some(),
            c.identical(),
            diffs.join(",")
        );
    } else {
        println!("Copia del particionado válida: {}", backup.display());
        println!("  creada por:  {}", b.created_by);
        println!("  origen:      {}", b.source.description);
        println!(
            "  tamaño:      {} bytes ({} sectores de {} B)",
            b.source.size_bytes, b.source.sector_count, b.source.sector_size
        );
        println!(
            "  esquema:     {}, {} particiones",
            scheme_name(b.layout.scheme),
            b.layout.partitions.len()
        );
        println!(
            "  regiones:    {} ({data_bytes} bytes de tabla guardados)",
            b.regions.len()
        );
        if let Some(s) = &b.search {
            println!("  búsqueda:    {} volúmenes encontrados", s.volumes.len());
        }
        if c.identical() {
            println!(
                "El origen coincide con la copia: mismo particionado y mismos bytes de tabla."
            );
        } else {
            println!("Diferencias con el origen ({}):", c.differences.len());
            for d in &c.differences {
                println!("  [{}] {d}", d.code());
            }
        }
        println!("Comparación de solo lectura: no se ha escrito nada en el origen.");
    }
    if c.identical() {
        Ok(())
    } else {
        Err((
            EXIT_DIFFERENT,
            format!(
                "el origen no coincide con la copia del particionado ({} diferencias)",
                c.differences.len()
            ),
        ))
    }
}
