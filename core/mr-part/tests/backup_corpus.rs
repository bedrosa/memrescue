//! Copia del particionado (#18) contra el corpus sintético de particiones (`tests/corpus`, #14).
//!
//! Requieren las imágenes generadas (`python -I -X utf8 tests/generators/gen_partition_images.py
//! tests/corpus/generated`; `MR_CORPUS_DIR` para otra ubicación). Se marcan `#[ignore]`; se
//! ejecutan con `cargo test -p mr-part --test backup_corpus -- --ignored`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use mr_io::{BlockDevice, FaultyDevice, ImageFile};
use mr_part::backup::{
    capture, compare, Difference, LayoutBackup, RegionContent, RegionKind, RegionStatus,
};
use mr_part::gpt::GptCopy;
use mr_part::read_partition_table;
use mr_part::search::{search, NoObserver, SearchOptions};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn generated() -> PathBuf {
    std::env::var_os("MR_CORPUS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo().join("tests/corpus/generated"))
}

/// Claves de nivel superior del manifiesto (lo único que necesitan estas pruebas).
fn manifests() -> Vec<HashMap<String, String>> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(repo().join("tests/corpus/manifests"))
        .expect("directorio de manifiestos")
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().ends_with(".manifest.toml"))
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|p| {
            let text = std::fs::read_to_string(p).unwrap();
            text.lines()
                .map(str::trim)
                .take_while(|l| !l.starts_with("[["))
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .filter_map(|l| l.split_once(" = "))
                .map(|(k, v)| (k.to_string(), v.trim_matches('"').to_string()))
                .collect()
        })
        .collect()
}

fn list(m: &HashMap<String, String>, k: &str) -> Vec<u64> {
    m.get(k)
        .map(|v| {
            v.trim_matches(|c| c == '[' || c == ']')
                .split(',')
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.trim().parse().unwrap())
                .collect()
        })
        .unwrap_or_default()
}

fn image_path(m: &HashMap<String, String>) -> PathBuf {
    let p = generated().join(&m["file"]);
    assert!(
        p.exists(),
        "falta {}: genere el corpus con tests/generators/gen_partition_images.py",
        p.display()
    );
    p
}

/// La imagen del escenario con sus sectores ilegibles simulados.
fn source(path: &Path, m: &HashMap<String, String>) -> FaultyDevice<ImageFile> {
    let ss: u32 = m["sector_size"].parse().unwrap();
    FaultyDevice::new(ImageFile::open(path, ss).unwrap()).with_bad(list(m, "unreadable_lbas"))
}

/// Copia con la búsqueda exprés incluida.
fn full_backup(dev: &dyn BlockDevice, name: &str) -> LayoutBackup {
    let table = read_partition_table(dev).unwrap();
    let found = search(dev, Some(&table), &SearchOptions::default(), &NoObserver).unwrap();
    capture(dev, name, Some(&found)).unwrap()
}

#[test]
#[ignore = "requiere el corpus generado"]
fn round_trip_over_the_whole_partition_corpus() {
    let all = manifests();
    assert_eq!(all.len(), 38, "número de escenarios del corpus");
    let mut regions = 0;
    for m in &all {
        let name = &m["name"];
        let dev = source(&image_path(m), m);
        let b = full_backup(&dev, name);
        assert_eq!(b.source.size_bytes, m["size_bytes"].parse::<u64>().unwrap());
        // Siempre la LBA 0 (como datos o como ilegible).
        assert_eq!(b.regions[0].kind, RegionKind::Mbr, "{name}");
        for r in &b.regions {
            if let RegionContent::Unreadable(lbas) = &r.content {
                assert!(
                    lbas.iter().all(|l| list(m, "unreadable_lbas").contains(l)),
                    "{name}"
                );
            }
        }
        regions += b.regions.len();
        let text = b.to_text();
        let back = LayoutBackup::from_text(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(back, b, "{name}: ida y vuelta con pérdidas");
        assert_eq!(back.to_text(), text, "{name}: escritura no determinista");
        let c = compare(&back, &dev).unwrap();
        assert!(c.identical(), "{name}: {:#?}", c.differences);
    }
    assert!(regions > all.len());
}

/// Copia la imagen a un directorio temporal y aplica `edit` a sus bytes (el original no se toca).
fn modified_copy(src: &Path, dir: &Path, edit: impl FnOnce(&mut Vec<u8>)) -> PathBuf {
    let mut bytes = std::fs::read(src).unwrap();
    edit(&mut bytes);
    let out = dir.join(src.file_name().unwrap());
    std::fs::write(&out, bytes).unwrap();
    out
}

fn manifest(name: &str) -> HashMap<String, String> {
    manifests()
        .into_iter()
        .find(|m| m["name"] == name)
        .unwrap_or_else(|| panic!("falta el escenario {name}"))
}

fn region(b: &LayoutBackup, kind: RegionKind) -> (u64, u64) {
    let r = b.regions.iter().find(|r| r.kind == kind).unwrap();
    (r.offset, r.length)
}

#[test]
#[ignore = "requiere el corpus generado"]
fn comparison_reports_differences_on_modified_copies() {
    let tmp = tempfile::tempdir().unwrap();

    // MBR con cadena de EBR: se borra el primer EBR y se cambia el tipo de la entrada 0.
    let m = manifest("mbr_basic");
    let original = image_path(&m);
    let before = std::fs::read(&original).unwrap();
    let b = full_backup(&source(&original, &m), "mbr_basic");
    let (ebr, len) = region(&b, RegionKind::Ebr);
    let copy = modified_copy(&original, tmp.path(), |d| {
        d[ebr as usize..(ebr + len) as usize].fill(0);
        d[446 + 4] ^= 0x01;
    });
    let c = compare(&b, &source(&copy, &m)).unwrap();
    let kinds: Vec<RegionKind> = c
        .differences
        .iter()
        .filter_map(|x| match x {
            Difference::Region { kind, .. } => Some(*kind),
            _ => None,
        })
        .collect();
    assert!(kinds.contains(&RegionKind::Mbr) && kinds.contains(&RegionKind::Ebr));
    assert!(c.differences.iter().any(
        |x| matches!(x, Difference::PartitionChanged { fields, .. } if fields.contains(&"tipo"))
    ));
    assert!(c
        .differences
        .iter()
        .any(|x| matches!(x, Difference::PartitionRemoved { .. })));
    // El original sigue intacto y sigue coincidiendo con su copia.
    assert_eq!(std::fs::read(&original).unwrap(), before);
    assert!(compare(&b, &source(&original, &m)).unwrap().identical());

    // GPT: entradas primarias dañadas (pasa a usarse la de respaldo).
    let m = manifest("gpt_basic");
    let original = image_path(&m);
    let b = full_backup(&source(&original, &m), "gpt_basic");
    let (entries, _) = region(&b, RegionKind::GptPrimaryEntries);
    let copy = modified_copy(&original, tmp.path(), |d| d[entries as usize + 40] ^= 0xFF);
    let c = compare(&b, &source(&copy, &m)).unwrap();
    assert!(c.differences.contains(&Difference::GptSelected {
        backup: Some(GptCopy::Primary),
        current: Some(GptCopy::Backup)
    }));
    assert!(c.differences.iter().any(|x| matches!(
        x,
        Difference::Region {
            kind: RegionKind::GptPrimaryEntries,
            ..
        }
    )));

    // GPT recortada: la copia de respaldo ya no existe en el origen.
    let copy = modified_copy(&original, tmp.path(), |d| d.truncate(d.len() / 2));
    let c = compare(&b, &source(&copy, &m)).unwrap();
    assert!(c
        .differences
        .iter()
        .any(|x| matches!(x, Difference::SourceSize { .. })));
    assert!(c.differences.iter().any(|x| matches!(
        x,
        Difference::Region {
            kind: RegionKind::GptBackupHeader,
            current: RegionStatus::Absent,
            ..
        }
    )));
}
