//! Integración de los lectores MBR/EBR (#12), GPT (#13) y APM (#17): esquema del origen, unión de particiones y
//! anomalías, códigos estables de las anomalías GPT, nombres de tipo GPT y coordenadas para abrir una
//! partición sin aplicar nunca en silencio el desplazamiento de una imagen (ADR-022).

use crate::apm;
use crate::gpt::{self, GptAnomaly, GptCopy};
use crate::mbr::{self, MbrAnomaly};
use crate::{Anomaly, Origin, Partition, PartitionTable, Scheme, Severity};
use mr_io::BlockDevice;

/// Lee el particionado del origen.
///
/// 1. Lee el MBR de la LBA 0 y su cadena de EBR ([`mbr::read_mbr`]).
/// 2. Lee la GPT ([`gpt::read_gpt`]) y la incorpora si el MBR es protector (`Gpt`) o híbrido
///    (`Hybrid`), o si no hay tabla MBR utilizable (LBA 0 en blanco, ilegible, no plausible o con una
///    cabecera GPT) pero aparece alguna firma `EFI PART` en las posiciones de la GPT; en este último
///    caso el esquema pasa a `Gpt` (imágenes desplazadas o sin MBR protector). Con un MBR clásico
///    (`Mbr`) o un disco sin particionar (`Superfloppy`) la GPT no se mira: el MBR manda.
/// 3. Si la LBA 0 no tiene una tabla MBR utilizable, antes que la GPT se busca un Apple Partition
///    Map ([`apm::read_apm`]): con una entrada `PM` en el bloque 1 el esquema es `Apm` (la GPT no se
///    mira); un Driver Descriptor Map sin mapa solo da `Apm` (con `apm_map_missing`) si tampoco hay
///    GPT. Las particiones APM están en bloques del mapa y `sector_size` es ese tamaño de bloque.
/// 4. En un híbrido se listan las entradas MBR (distintas de 0xEE) y después las GPT, y cada entrada
///    MBR que no coincide con ninguna GPT recibe [`MbrAnomaly::HybridMismatch`].
///
/// Las particiones GPT conservan las LBA **del disco** declaradas por la GPT; si la imagen está
/// desplazada ([`PartitionTable::shift`]), abrir una exige pedirlo explícitamente
/// ([`open_coordinates`]). `sector_size` es el de la GPT cuando esta se interpretó con otro tamaño de
/// sector y no hay particiones MBR.
pub fn read_partition_table(dev: &(impl BlockDevice + ?Sized)) -> mr_io::Result<PartitionTable> {
    let scan = mbr::read_mbr(dev)?;
    let mut table = PartitionTable {
        scheme: scan.scheme,
        sector_size: dev.sector_size(),
        partitions: scan.partitions,
        anomalies: scan.anomalies,
        gpt: None,
        apm: None,
    };
    let superfloppy = table
        .anomalies
        .iter()
        .any(|a| matches!(a, Anomaly::Mbr(MbrAnomaly::Superfloppy { .. })));
    if table.scheme == Scheme::Mbr || superfloppy {
        return Ok(table);
    }
    let apm = if table.scheme == Scheme::None {
        apm::read_apm(dev)?
    } else {
        None
    };
    if apm.as_ref().is_some_and(|a| a.map_found) {
        return Ok(with_apm(table, apm));
    }
    let g = gpt::read_gpt(dev)?;
    match table.scheme {
        Scheme::Gpt | Scheme::Hybrid => {}
        _ if g.signature_found() || g.selected.is_some() => table.scheme = Scheme::Gpt,
        _ if apm.is_some() => return Ok(with_apm(table, apm)),
        _ => return Ok(table),
    }
    let mbr_partitions = !table.partitions.is_empty();
    let same_units = g.sector_size == dev.sector_size();
    if same_units || !mbr_partitions {
        table.sector_size = g.sector_size;
        if table.scheme == Scheme::Hybrid && g.shift == 0 {
            hybrid_mismatches(&mut table.partitions, &g);
        }
        table
            .partitions
            .extend(g.partitions.iter().map(|p| p.partition.clone()));
    }
    for a in &g.anomalies {
        if !table.anomalies.contains(a) {
            table.anomalies.push(a.clone());
        }
    }
    table.gpt = Some(g);
    Ok(table)
}

/// Incorpora la lectura APM: esquema `Apm`, LBA en bloques del mapa, particiones y anomalías.
fn with_apm(mut table: PartitionTable, apm: Option<apm::ApmRead>) -> PartitionTable {
    let Some(a) = apm else {
        return table;
    };
    table.scheme = Scheme::Apm;
    table.sector_size = a.block_size;
    table
        .partitions
        .extend(a.partitions.iter().map(|p| p.partition.clone()));
    table.anomalies.extend(a.anomalies.iter().cloned());
    table.apm = Some(a);
    table
}

/// Marca las entradas MBR de un híbrido que no describen exactamente una partición GPT.
fn hybrid_mismatches(mbr_parts: &mut [Partition], g: &gpt::GptRead) {
    for p in mbr_parts.iter_mut() {
        let Origin::Primary { slot } = p.origin else {
            continue;
        };
        let found = g.partitions.iter().any(|q| {
            q.partition.start_lba == p.start_lba && q.partition.sector_count == p.sector_count
        });
        if !found {
            p.anomalies
                .push(Anomaly::Mbr(MbrAnomaly::HybridMismatch { lba: 0, slot }));
            p.confidence = mbr::confidence_of(p.anomalies.iter());
        }
    }
}

/// Dónde abrir una partición en el origen (LBA de la imagen) y qué se hizo para llegar ahí.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenCoordinates {
    /// Primer sector en la imagen.
    pub image_start: u64,
    /// Sectores que se abren (recortados al final de la imagen).
    pub count: u64,
    /// Desplazamiento aplicado (`LBA imagen = LBA disco - shift`); 0 si no se aplicó ninguno.
    pub shift_applied: i64,
    /// Sectores de la partición que faltan al final de la imagen.
    pub missing_at_end: u64,
}

/// Calcula dónde abrir la partición `index` de `table` en un origen de `device_sectors` sectores de
/// `device_sector_size` bytes. Reglas (ADR-022, adenda):
/// - Si la tabla se interpretó con otro tamaño de sector, se rechaza: hay que reabrir el origen con
///   ese tamaño (las LBA no se convierten en silencio).
/// - Si la partición es GPT y la imagen está desplazada, se rechaza salvo `apply_shift`; con él, la
///   LBA de la imagen se calcula explícitamente con el desplazamiento informado por la GPT y se
///   devuelve en `shift_applied` para que la descripción lo diga.
/// - Si empieza fuera de la imagen (o antes de su inicio), se rechaza; si termina fuera, se recorta.
pub fn open_coordinates(
    table: &PartitionTable,
    index: usize,
    device_sector_size: u32,
    device_sectors: u64,
    apply_shift: bool,
) -> Result<OpenCoordinates, String> {
    let p = table
        .partitions
        .get(index)
        .ok_or_else(|| format!("no existe la partición {}", index + 1))?;
    if table.sector_size != device_sector_size {
        return Err(format!(
            "la tabla usa sectores de {} B y el origen está abierto con sectores de {device_sector_size} B: \
             vuelva a abrirlo con un tamaño de sector de {} B",
            table.sector_size, table.sector_size
        ));
    }
    let shift = match p.origin {
        Origin::Gpt { .. } => table.shift(),
        _ => 0,
    };
    if shift != 0 && !apply_shift {
        return Err(format!(
            "la imagen está desplazada {shift} sectores respecto al disco original (LBA disco = LBA imagen + {shift}): \
             la partición declara LBA del disco; ábrala pidiendo explícitamente aplicar el desplazamiento"
        ));
    }
    let start = i128::from(p.start_lba) - i128::from(shift);
    let start = u64::try_from(start).map_err(|_| {
        format!(
            "la partición empieza en la LBA {} del disco, antes del inicio de la imagen (desplazamiento {shift})",
            p.start_lba
        )
    })?;
    if p.sector_count == 0 || start >= device_sectors {
        return Err(format!(
            "la partición (LBA {start} de la imagen, {} sectores) empieza fuera del origen ({device_sectors} sectores)",
            p.sector_count
        ));
    }
    let count = p.sector_count.min(device_sectors - start);
    Ok(OpenCoordinates {
        image_start: start,
        count,
        shift_applied: shift,
        missing_at_end: p.sector_count - count,
    })
}

// ---------------------------------------------------------------------------------------------
// Anomalías GPT: código, gravedad y valores (la correspondencia con el manifiesto del corpus está en
// docs/formatos/gpt.md).
// ---------------------------------------------------------------------------------------------

fn by_copy(copy: GptCopy, primary: &'static str, backup: &'static str) -> &'static str {
    match copy {
        GptCopy::Primary => primary,
        GptCopy::Backup => backup,
    }
}

/// Código estable de una anomalía GPT.
pub(crate) fn gpt_code(a: &GptAnomaly) -> &'static str {
    use GptAnomaly::*;
    match a {
        HeaderMissing { copy, .. } => by_copy(
            *copy,
            "gpt_primary_header_missing",
            "gpt_backup_header_missing",
        ),
        HeaderOutsideImage { copy, .. } => by_copy(
            *copy,
            "gpt_primary_header_outside_image",
            "gpt_backup_header_outside_image",
        ),
        HeaderCrc { copy, .. } => by_copy(*copy, "gpt_primary_header_crc", "gpt_backup_header_crc"),
        HeaderSize { copy, .. } => {
            by_copy(*copy, "gpt_primary_header_size", "gpt_backup_header_size")
        }
        RevisionUnsupported { copy, .. } => by_copy(
            *copy,
            "gpt_primary_revision_unsupported",
            "gpt_backup_revision_unsupported",
        ),
        RevisionMinor { copy, .. } => by_copy(
            *copy,
            "gpt_primary_revision_minor",
            "gpt_backup_revision_minor",
        ),
        MyLbaMismatch { copy, .. } => by_copy(
            *copy,
            "gpt_primary_my_lba_mismatch",
            "gpt_backup_my_lba_mismatch",
        ),
        AlternateLbaMismatch { copy, .. } => by_copy(
            *copy,
            "gpt_primary_alternate_lba_mismatch",
            "gpt_backup_alternate_lba_mismatch",
        ),
        UsableRangeInvalid { copy, .. } => by_copy(
            *copy,
            "gpt_primary_usable_range_invalid",
            "gpt_backup_usable_range_invalid",
        ),
        EntriesLocationInvalid { copy, .. } => by_copy(
            *copy,
            "gpt_primary_entries_location_invalid",
            "gpt_backup_entries_location_invalid",
        ),
        EntryArrayLimits { copy, .. } => by_copy(
            *copy,
            "gpt_primary_entry_array_limits",
            "gpt_backup_entry_array_limits",
        ),
        EntriesCrc { copy, .. } => {
            by_copy(*copy, "gpt_primary_entries_crc", "gpt_backup_entries_crc")
        }
        EntriesOutsideImage { copy, .. } => by_copy(
            *copy,
            "gpt_primary_entries_outside_image",
            "gpt_backup_entries_outside_image",
        ),
        BackupInUse { .. } => "gpt_backup_in_use",
        CopiesDisagree { .. } => "gpt_copies_disagree",
        ImageShifted { .. } => "image_shifted",
        ImageTruncated { .. } => "gpt_image_truncated",
        UsableRangeOutsideImage { .. } => "gpt_usable_range_outside_image",
        SectorSizeMismatch { .. } => "gpt_sector_size_mismatch",
        EntryInverted { .. } => "gpt_entry_inverted",
        EntryOutsideUsable { .. } => "gpt_entry_outside_usable",
        EntryOutsideImage { .. } => "gpt_entry_outside_image",
        EntryOverlap { .. } => "gpt_entry_overlap",
        DuplicateUniqueGuid { .. } => "gpt_duplicate_unique_guid",
        NameInvalid { .. } => "gpt_name_invalid",
    }
}

/// Gravedad de una anomalía GPT, coherente con la confianza que da el lector GPT: los daños de una
/// copia o de su posición quedan compensados si la otra copia es utilizable (la confianza de la tabla
/// la fija entonces [`gpt::GptRead::confidence`]); lo que afecta a la imagen o a la copia elegida es
/// sospechoso; una entrada invertida, inválida.
pub(crate) fn gpt_severity(a: &GptAnomaly) -> Severity {
    use GptAnomaly::*;
    match a {
        NameInvalid { .. } => Severity::Notice,
        EntryInverted { .. } => Severity::Invalid,
        CopiesDisagree { .. }
        | ImageShifted { .. }
        | ImageTruncated { .. }
        | UsableRangeOutsideImage { .. }
        | SectorSizeMismatch { .. }
        | RevisionMinor { .. }
        | EntryOutsideUsable { .. }
        | EntryOutsideImage { .. }
        | EntryOverlap { .. }
        | DuplicateUniqueGuid { .. } => Severity::Suspicious,
        _ => Severity::Compensated,
    }
}

/// LBA de la imagen donde se detectó; `None` si la estructura no tiene posición en la imagen.
pub(crate) fn gpt_lba(a: &GptAnomaly) -> Option<u64> {
    use GptAnomaly::*;
    match *a {
        HeaderMissing { lba, .. }
        | HeaderCrc { lba, .. }
        | HeaderSize { lba, .. }
        | RevisionUnsupported { lba, .. }
        | RevisionMinor { lba, .. }
        | MyLbaMismatch { lba, .. }
        | AlternateLbaMismatch { lba, .. }
        | UsableRangeInvalid { lba, .. }
        | EntriesLocationInvalid { lba, .. }
        | EntryArrayLimits { lba, .. }
        | EntriesCrc { lba, .. }
        | UsableRangeOutsideImage { lba, .. } => Some(lba),
        _ => None,
    }
}

pub(crate) fn gpt_target(a: &GptAnomaly) -> Option<u64> {
    match *a {
        GptAnomaly::MyLbaMismatch { expected, .. } => Some(expected),
        _ => None,
    }
}

pub(crate) fn gpt_value(a: &GptAnomaly) -> Option<u64> {
    use GptAnomaly::*;
    match *a {
        HeaderOutsideImage { disk_lba, .. } | EntriesOutsideImage { disk_lba, .. } => {
            Some(disk_lba)
        }
        HeaderSize { size, .. } => Some(u64::from(size)),
        RevisionUnsupported { revision, .. } | RevisionMinor { revision, .. } => {
            Some(u64::from(revision))
        }
        MyLbaMismatch { my_lba, .. } => Some(my_lba),
        AlternateLbaMismatch { alternate_lba, .. } => Some(alternate_lba),
        EntriesLocationInvalid { entries_lba, .. } => Some(entries_lba),
        EntryArrayLimits { entry_count, .. } => Some(u64::from(entry_count)),
        // Desplazamiento con signo en complemento a dos (Anomaly::value_signed).
        ImageShifted { offset } => Some(offset as u64),
        ImageTruncated { missing } => Some(missing),
        SectorSizeMismatch { used, .. } => Some(u64::from(used)),
        EntryInverted { index, .. }
        | EntryOutsideUsable { index }
        | EntryOutsideImage { index }
        | EntryOverlap { index, .. }
        | DuplicateUniqueGuid { index, .. }
        | NameInvalid { index } => Some(u64::from(index)),
        _ => None,
    }
}

/// Nombre legible de un GUID de tipo de partición GPT (en el orden de bytes del disco), o `None` si
/// no figura en la lista. Lista propia de identificadores documentados públicamente.
pub fn gpt_type_name(guid: &[u8; 16]) -> Option<&'static str> {
    const KNOWN: &[(&str, &str)] = &[
        ("c12a7328-f81f-11d2-ba4b-00a0c93ec93b", "Sistema EFI"),
        (
            "024dee41-33e7-11d3-9d69-0008c781f39f",
            "Esquema MBR heredado",
        ),
        ("21686148-6449-6e6f-744e-656564454649", "Arranque BIOS"),
        (
            "e3c9e316-0b5c-4db8-817d-f92df00215ae",
            "Reservada de Microsoft (MSR)",
        ),
        (
            "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7",
            "Datos básicos de Microsoft",
        ),
        (
            "de94bba4-06d1-4d40-a16a-bfd50179d6ac",
            "Entorno de recuperación de Windows",
        ),
        (
            "5808c8aa-7e8f-42e0-85d2-e1e90434cfb3",
            "Metadatos de disco dinámico (LDM)",
        ),
        (
            "af9b60a0-1431-4f62-bc68-3311714a69ad",
            "Datos de disco dinámico (LDM)",
        ),
        (
            "e75caf8f-f680-4cee-afa3-b001e56efc2d",
            "Espacios de almacenamiento",
        ),
        (
            "0fc63daf-8483-4772-8e79-3d69d8477de4",
            "Sistema de archivos Linux",
        ),
        (
            "0657fd6d-a4ab-43c4-84e5-0933c84b4f4f",
            "Intercambio de Linux",
        ),
        ("e6d6d379-f507-44c2-a23c-238f2a3df928", "Linux LVM"),
        ("a19d880f-05fc-4d3b-a006-743f0f84911e", "RAID de Linux"),
        ("933ac7e1-2eb4-4f13-b844-0e14e2aef915", "Linux /home"),
        (
            "4f68bce3-e8cd-4db1-96e7-fbcaf984b709",
            "Raíz de Linux (x86-64)",
        ),
        (
            "44479540-f297-41b2-9af7-d131d5f0458a",
            "Raíz de Linux (x86)",
        ),
        (
            "bc13c2ff-59e6-4262-a352-b275fd6f7172",
            "Arranque extendido de Linux",
        ),
        ("ca7d7ccb-63ed-4c53-861c-1742536059cc", "Linux LUKS"),
        ("48465300-0000-11aa-aa11-00306543ecac", "Apple HFS+"),
        ("7c3457ef-0000-11aa-aa11-00306543ecac", "Apple APFS"),
        ("55465300-0000-11aa-aa11-00306543ecac", "Apple UFS"),
        ("426f6f74-0000-11aa-aa11-00306543ecac", "Arranque de Apple"),
        ("52414944-0000-11aa-aa11-00306543ecac", "RAID de Apple"),
        ("516e7cb4-6ecf-11d6-8ff8-00022d09712b", "Datos de FreeBSD"),
        (
            "516e7cb5-6ecf-11d6-8ff8-00022d09712b",
            "Intercambio de FreeBSD",
        ),
        ("516e7cb6-6ecf-11d6-8ff8-00022d09712b", "UFS de FreeBSD"),
        ("516e7cba-6ecf-11d6-8ff8-00022d09712b", "ZFS de FreeBSD"),
        (
            "6a898cc3-1dd2-11b2-99a6-080020736631",
            "ZFS (Solaris / Apple)",
        ),
    ];
    let text = gpt::format_guid(guid);
    KNOWN.iter().find(|(g, _)| *g == text).map(|(_, n)| *n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpt::format_guid;
    use crate::Confidence;
    use mr_io::MemDevice;

    /// Disco GPT de 256 sectores de 512 B según UEFI con una partición en [64, 128) y, en la LBA 0,
    /// el MBR indicado (protector si `mbr` está vacío). Cada sector de datos vale su LBA (mod 256).
    /// Con `cut` > 0 se quitan los `cut` primeros sectores.
    fn gpt_disk(mbr: &[(u8, u32, u32)], cut: usize) -> Vec<u8> {
        const TOTAL: u64 = 256;
        let mut d: Vec<u8> = (0..TOTAL as usize * 512).map(|i| (i / 512) as u8).collect();
        let mut entries = vec![0u8; 128 * 128];
        entries[..16].fill(0x42);
        entries[16..32].fill(0x5A);
        entries[32..40].copy_from_slice(&64u64.to_le_bytes());
        entries[40..48].copy_from_slice(&127u64.to_le_bytes());
        let ecrc = crc32fast::hash(&entries);
        let header = |my: u64, alt: u64, at: u64| {
            let mut h = vec![0u8; 512];
            h[..8].copy_from_slice(b"EFI PART");
            h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
            h[12..16].copy_from_slice(&92u32.to_le_bytes());
            h[24..32].copy_from_slice(&my.to_le_bytes());
            h[32..40].copy_from_slice(&alt.to_le_bytes());
            h[40..48].copy_from_slice(&34u64.to_le_bytes());
            h[48..56].copy_from_slice(&(TOTAL - 34).to_le_bytes());
            h[72..80].copy_from_slice(&at.to_le_bytes());
            h[80..84].copy_from_slice(&128u32.to_le_bytes());
            h[84..88].copy_from_slice(&128u32.to_le_bytes());
            h[88..92].copy_from_slice(&ecrc.to_le_bytes());
            let crc = crc32fast::hash(&h[..92]);
            h[16..20].copy_from_slice(&crc.to_le_bytes());
            h
        };
        d[..512].fill(0);
        let protective = [(0xEE, 1, (TOTAL - 1) as u32)];
        for (i, &(kind, start, count)) in (if mbr.is_empty() { &protective[..] } else { mbr })
            .iter()
            .enumerate()
        {
            let e = &mut d[0x1BE + 16 * i..0x1BE + 16 * (i + 1)];
            e[4] = kind;
            e[8..12].copy_from_slice(&start.to_le_bytes());
            e[12..16].copy_from_slice(&count.to_le_bytes());
        }
        d[510] = 0x55;
        d[511] = 0xAA;
        d[512..1024].copy_from_slice(&header(1, TOTAL - 1, 2));
        d[1024..1024 + entries.len()].copy_from_slice(&entries);
        let back = (TOTAL as usize - 33) * 512;
        d[back..back + entries.len()].copy_from_slice(&entries);
        d[(TOTAL as usize - 1) * 512..].copy_from_slice(&header(TOTAL - 1, 1, TOTAL - 33));
        d.split_off(cut * 512)
    }

    fn table(data: Vec<u8>) -> PartitionTable {
        read_partition_table(&MemDevice::new(data, 512).unwrap()).unwrap()
    }

    #[test]
    fn protective_mbr_brings_the_gpt_partitions() {
        let t = table(gpt_disk(&[], 0));
        assert_eq!(t.scheme, Scheme::Gpt);
        assert_eq!(t.partitions.len(), 1);
        let p = &t.partitions[0];
        assert_eq!(
            (p.origin, p.start_lba, p.sector_count),
            (Origin::Gpt { index: 0 }, 64, 64)
        );
        assert_eq!(t.gpt_entry(p).unwrap().unique_guid, [0x5A; 16]);
        assert_eq!(t.confidence(), Confidence::Valid);
        let c = open_coordinates(&t, 0, 512, 256, false).unwrap();
        assert_eq!((c.image_start, c.count, c.shift_applied), (64, 64, 0));
        // Origen abierto con otro tamaño de sector: se rechaza, no se convierte.
        assert!(open_coordinates(&t, 0, 4096, 32, false)
            .unwrap_err()
            .contains("tamaño de sector"));
        assert!(open_coordinates(&t, 1, 512, 256, false).is_err());
    }

    #[test]
    fn hybrid_keeps_mbr_entries_and_reports_mismatches() {
        let mbr = [(0xEE, 1, 63), (0x07, 64, 64), (0x83, 200, 10)];
        let t = table(gpt_disk(&mbr, 0));
        assert_eq!(t.scheme, Scheme::Hybrid);
        let origins: Vec<Origin> = t.partitions.iter().map(|p| p.origin).collect();
        assert_eq!(
            origins,
            vec![
                Origin::Primary { slot: 1 },
                Origin::Primary { slot: 2 },
                Origin::Gpt { index: 0 }
            ]
        );
        assert!(t.partitions[0].anomalies.is_empty());
        assert_eq!(
            t.partitions[1].anomalies,
            vec![Anomaly::Mbr(MbrAnomaly::HybridMismatch { lba: 0, slot: 2 })]
        );
        assert_eq!(t.partitions[1].anomalies[0].code(), "hybrid_mbr_mismatch");
        assert!(matches!(t.confidence(), Confidence::Suspicious(_)));
    }

    #[test]
    fn classic_mbr_ignores_stale_gpt_and_blank_lba0_finds_it() {
        let t = table(gpt_disk(&[(0x07, 64, 64)], 0));
        assert_eq!((t.scheme, t.partitions.len()), (Scheme::Mbr, 1));
        assert!(t.gpt.is_none());
        let mut blank = gpt_disk(&[], 0);
        blank[..512].fill(0);
        let t = table(blank);
        assert_eq!((t.scheme, t.partitions.len()), (Scheme::Gpt, 1));
        assert!(t.anomalies.iter().any(|a| a.code() == "mbr_missing"));
    }

    #[test]
    fn shifted_image_never_applies_the_shift_silently() {
        let t = table(gpt_disk(&[], 5));
        assert_eq!((t.scheme, t.shift()), (Scheme::Gpt, 5));
        assert_eq!(t.partitions[0].start_lba, 64, "LBA del disco");
        let err = open_coordinates(&t, 0, 512, 251, false).unwrap_err();
        assert!(err.contains("desplazada 5 sectores"), "{err}");
        let c = open_coordinates(&t, 0, 512, 251, true).unwrap();
        assert_eq!((c.image_start, c.count, c.shift_applied), (59, 64, 5));
        // Más desplazada que el inicio de la partición: se rechaza aunque se pida aplicar.
        let t = table(gpt_disk(&[], 70));
        assert_eq!(t.shift(), 70);
        assert!(open_coordinates(&t, 0, 512, 186, true)
            .unwrap_err()
            .contains("antes del inicio"));
    }

    #[test]
    fn known_gpt_types_have_names() {
        // EFI System en el orden de bytes del disco.
        let efi = [
            0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E,
            0xC9, 0x3B,
        ];
        assert_eq!(format_guid(&efi), "c12a7328-f81f-11d2-ba4b-00a0c93ec93b");
        assert_eq!(gpt_type_name(&efi), Some("Sistema EFI"));
        assert_eq!(gpt_type_name(&[0x11; 16]), None);
    }

    #[test]
    fn gpt_anomalies_have_codes_and_signed_shift() {
        let a = Anomaly::Gpt(GptAnomaly::ImageShifted { offset: -8 });
        assert_eq!(a.code(), "image_shifted");
        assert!(a.value_signed());
        assert_eq!(a.value().map(|v| v as i64), Some(-8));
        assert_eq!(a.lba(), None);
        let a = Anomaly::Gpt(GptAnomaly::HeaderCrc {
            copy: GptCopy::Backup,
            lba: 4095,
            stored: 1,
            computed: 2,
        });
        assert_eq!(
            (a.code(), a.lba(), a.severity()),
            ("gpt_backup_header_crc", Some(4095), Severity::Compensated)
        );
        assert!(a.to_string().contains("CRC-32"));
    }
}
