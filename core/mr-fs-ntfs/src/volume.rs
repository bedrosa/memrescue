//! Apertura de un volumen NTFS: arranque (principal o copia), `$MFT`, `$MFTMirr` y `$Volume`.
//!
//! 1. **Arranque.** Se valida el sector 0 ([`crate::boot::parse_boot`]). Si es válido se comprueba
//!    la copia en el sector siguiente al último que cuenta el arranque (o uno más allá, variante
//!    que también existe). Si el principal no es válido o no se puede leer, se busca la copia en el
//!    último sector del origen (y en el penúltimo), y solo se acepta si su propio total de sectores
//!    la sitúa ahí. Usar la copia es una corrección (`ValidWithFixes`) y se informa.
//! 2. **`$MFT`.** Se lee el registro 0 en el LCN del arranque y se toman los runs de su `$DATA`
//!    sin nombre. Si el registro 0 no es utilizable se usa su copia en `$MFTMirr` (corrección
//!    informada); si tampoco, el volumen no se abre ([`NtfsError::MftUnusable`]).
//! 3. **`$MFTMirr`.** Se comparan los primeros registros de la MFT (tantos como dice el `$DATA` del
//!    registro 1, normalmente 4) con su copia; cada discrepancia se informa.
//! 4. **`$Volume`.** Etiqueta y versión (registro 3), solo informativas.

use std::fmt;
use std::ops::Range;
use std::sync::Arc;

use mr_common::Confidence;
use mr_io::BlockDevice;
use mr_part::Severity;

use crate::attr::{types, utf16_lossy, AttrBody};
use crate::boot::{parse_boot, same_bpb, BootError, BootNote, BootSector, BOOT_SIZE};
use crate::mft::{read_record_at, MftLayout, RecordOutcome, RecordState, Records, MAX_RECORDS};
use crate::read::read_span;
use crate::record::FileRecord;
use crate::runs::{decode_runs, Run};
use crate::NtfsError;

/// Reintentos por sector en las lecturas del lector NTFS.
pub const DEFAULT_RETRIES: u32 = 1;
/// Máximo de registros de `$MFTMirr` que se comparan.
pub const MAX_MIRROR_RECORDS: u64 = 64;
/// Registros que se comparan si `$MFTMirr` no dice cuántos tiene.
pub const DEFAULT_MIRROR_RECORDS: u64 = 4;

/// Resultado de examinar una posición de sector de arranque.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootProbe {
    Valid,
    Invalid(BootError),
    /// Algún sector no es fiable: no se interpreta.
    Unreadable {
        lbas: Vec<u64>,
    },
    /// La posición cae fuera del origen.
    OutsideSource,
}

impl BootProbe {
    /// Código estable: `valid`, `invalid`, `unreadable` u `outside_source`.
    pub fn code(&self) -> &'static str {
        match self {
            BootProbe::Valid => "valid",
            BootProbe::Invalid(_) => "invalid",
            BootProbe::Unreadable { .. } => "unreadable",
            BootProbe::OutsideSource => "outside_source",
        }
    }
}

impl fmt::Display for BootProbe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BootProbe::Valid => write!(f, "válido"),
            BootProbe::Invalid(e) => write!(f, "{e}"),
            BootProbe::Unreadable { lbas } => write!(f, "ilegible (LBA {lbas:?})"),
            BootProbe::OutsideSource => write!(f, "fuera del origen"),
        }
    }
}

/// Qué sector de arranque se usó.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootSource {
    Primary,
    /// La copia, en este desplazamiento (bytes desde el inicio del volumen).
    Backup {
        offset: u64,
    },
}

/// Estado de la copia del arranque.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupState {
    /// Válida y con el mismo BPB que el principal.
    Matches,
    /// Válida pero con otro BPB.
    Differs,
    Invalid(BootError),
    Unreadable {
        lbas: Vec<u64>,
    },
    OutsideSource,
    /// Es la que se usa (el principal no era válido).
    InUse,
}

impl BackupState {
    pub fn code(&self) -> &'static str {
        match self {
            BackupState::Matches => "matches",
            BackupState::Differs => "differs",
            BackupState::Invalid(_) => "invalid",
            BackupState::Unreadable { .. } => "unreadable",
            BackupState::OutsideSource => "outside_source",
            BackupState::InUse => "in_use",
        }
    }
}

/// Copia del arranque: dónde se esperaba o se encontró y su estado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupBoot {
    pub offset: u64,
    pub state: BackupState,
}

/// De dónde salió el registro 0 que localiza la MFT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordSource {
    Mft,
    Mirror,
}

/// Discrepancia entre un registro de la MFT y su copia en `$MFTMirr`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorMismatch {
    pub record: u64,
    pub mft: RecordState,
    pub mirror: RecordState,
    /// Detalle del daño (si alguno de los dos no es un registro válido).
    pub detail: Option<String>,
}

impl MirrorMismatch {
    /// `differs` (ambos válidos y distintos), `mirror_invalid`, `mft_invalid` o `both_invalid`.
    pub fn kind(&self) -> &'static str {
        let ok = |s: RecordState| matches!(s, RecordState::InUse | RecordState::NotInUse);
        match (ok(self.mft), ok(self.mirror)) {
            (true, true) => "differs",
            (true, false) => "mirror_invalid",
            (false, true) => "mft_invalid",
            (false, false) => "both_invalid",
        }
    }
}

/// Comprobación de `$MFTMirr`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorCheck {
    /// LCN de `$MFTMirr` según el arranque.
    pub lcn: u64,
    /// Runs de `$MFTMirr` (de su registro 1, o uno contiguo desde el LCN del arranque).
    pub runs: Vec<Run>,
    /// Registros comparados.
    pub records: u64,
    pub mismatches: Vec<MirrorMismatch>,
}

/// Anomalía del volumen (no de un registro concreto).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NtfsAnomaly {
    BootPrimaryInvalid(BootError),
    BootPrimaryUnreadable {
        lbas: Vec<u64>,
    },
    BootBackupInvalid {
        offset: u64,
        error: BootError,
    },
    BootBackupUnreadable {
        offset: u64,
        lbas: Vec<u64>,
    },
    BootBackupDiffers {
        offset: u64,
    },
    BootBackupOutsideSource {
        offset: u64,
    },
    /// La copia está un sector más allá de la posición estándar.
    BootBackupShifted {
        offset: u64,
    },
    BootNote(BootNote),
    /// Bytes por sector del arranque distintos del tamaño de sector del origen.
    SectorSizeMismatch {
        boot: u32,
        device: u32,
    },
    /// El volumen declarado es mayor que el origen.
    VolumeTruncated {
        volume_bytes: u64,
        source_bytes: u64,
    },
    /// El registro 0 de la MFT no era utilizable y se usó su copia de `$MFTMirr`.
    MftRecord0FromMirror {
        reason: String,
    },
    /// El primer run de `$MFT` no empieza en el LCN del arranque.
    MftLcnMismatch {
        boot: u64,
        runs: u64,
    },
    /// El tamaño de `$MFT` no es múltiplo del tamaño de registro.
    MftSizeNotMultiple {
        data_size: u64,
        record_size: u32,
    },
    /// Los runs del registro 0 no cubren toda la MFT (extensiones no seguidas).
    MftIncomplete {
        mapped_records: u64,
        record_count: u64,
    },
    /// El primer run de `$MFTMirr` no empieza en el LCN del arranque.
    MftMirrLcnMismatch {
        boot: u64,
        runs: u64,
    },
    /// El registro 1 (`$MFTMirr`) no dice dónde está la copia: se compara la posición del arranque.
    MftMirrRecordUnusable {
        reason: String,
    },
    MirrorMismatch(MirrorMismatch),
    /// El registro 3 (`$Volume`) no es utilizable: sin etiqueta ni versión.
    VolumeRecordUnusable {
        state: RecordState,
    },
}

impl NtfsAnomaly {
    /// Código estable.
    pub fn code(&self) -> &'static str {
        match self {
            NtfsAnomaly::BootPrimaryInvalid(_) => "boot_primary_invalid",
            NtfsAnomaly::BootPrimaryUnreadable { .. } => "boot_primary_unreadable",
            NtfsAnomaly::BootBackupInvalid { .. } => "boot_backup_invalid",
            NtfsAnomaly::BootBackupUnreadable { .. } => "boot_backup_unreadable",
            NtfsAnomaly::BootBackupDiffers { .. } => "boot_backup_differs",
            NtfsAnomaly::BootBackupOutsideSource { .. } => "boot_backup_outside_source",
            NtfsAnomaly::BootBackupShifted { .. } => "boot_backup_shifted",
            NtfsAnomaly::BootNote(n) => n.code(),
            NtfsAnomaly::SectorSizeMismatch { .. } => "sector_size_mismatch",
            NtfsAnomaly::VolumeTruncated { .. } => "volume_truncated",
            NtfsAnomaly::MftRecord0FromMirror { .. } => "mft_record0_from_mirror",
            NtfsAnomaly::MftLcnMismatch { .. } => "mft_lcn_mismatch",
            NtfsAnomaly::MftSizeNotMultiple { .. } => "mft_size_not_multiple",
            NtfsAnomaly::MftIncomplete { .. } => "mft_incomplete",
            NtfsAnomaly::MftMirrLcnMismatch { .. } => "mftmirr_lcn_mismatch",
            NtfsAnomaly::MftMirrRecordUnusable { .. } => "mftmirr_record_unusable",
            NtfsAnomaly::MirrorMismatch(_) => "mftmirr_mismatch",
            NtfsAnomaly::VolumeRecordUnusable { .. } => "volume_record_unusable",
        }
    }

    /// Gravedad (la misma escala que las tablas de particiones, ADR-022).
    pub fn severity(&self) -> Severity {
        match self {
            NtfsAnomaly::BootPrimaryInvalid(_)
            | NtfsAnomaly::BootPrimaryUnreadable { .. }
            | NtfsAnomaly::BootBackupInvalid { .. }
            | NtfsAnomaly::BootBackupUnreadable { .. }
            | NtfsAnomaly::BootBackupDiffers { .. }
            | NtfsAnomaly::MftRecord0FromMirror { .. }
            | NtfsAnomaly::MirrorMismatch(_) => Severity::Compensated,
            NtfsAnomaly::SectorSizeMismatch { .. }
            | NtfsAnomaly::VolumeTruncated { .. }
            | NtfsAnomaly::MftLcnMismatch { .. }
            | NtfsAnomaly::MftSizeNotMultiple { .. }
            | NtfsAnomaly::MftIncomplete { .. }
            | NtfsAnomaly::MftMirrLcnMismatch { .. } => Severity::Suspicious,
            NtfsAnomaly::BootBackupOutsideSource { .. }
            | NtfsAnomaly::BootBackupShifted { .. }
            | NtfsAnomaly::BootNote(_)
            | NtfsAnomaly::MftMirrRecordUnusable { .. }
            | NtfsAnomaly::VolumeRecordUnusable { .. } => Severity::Notice,
        }
    }

    /// Registro MFT al que se refiere, si lo hay.
    pub fn record(&self) -> Option<u64> {
        match self {
            NtfsAnomaly::MirrorMismatch(m) => Some(m.record),
            NtfsAnomaly::MftRecord0FromMirror { .. } => Some(0),
            NtfsAnomaly::VolumeRecordUnusable { .. } => Some(3),
            NtfsAnomaly::MftMirrRecordUnusable { .. } => Some(1),
            _ => None,
        }
    }
}

impl fmt::Display for NtfsAnomaly {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NtfsAnomaly::BootPrimaryInvalid(e) => write!(
                f,
                "el sector de arranque principal no es válido ({e}): se usa la copia"
            ),
            NtfsAnomaly::BootPrimaryUnreadable { lbas } => write!(
                f,
                "el sector de arranque principal no se pudo leer (LBA {lbas:?}): se usa la copia"
            ),
            NtfsAnomaly::BootBackupInvalid { offset, error } => write!(
                f,
                "la copia del arranque (byte {offset}) no es válida: {error}"
            ),
            NtfsAnomaly::BootBackupUnreadable { offset, lbas } => write!(
                f,
                "la copia del arranque (byte {offset}) no se pudo leer (LBA {lbas:?})"
            ),
            NtfsAnomaly::BootBackupDiffers { offset } => write!(
                f,
                "la copia del arranque (byte {offset}) describe otro volumen"
            ),
            NtfsAnomaly::BootBackupOutsideSource { offset } => write!(
                f,
                "la copia del arranque (byte {offset}) queda fuera del origen"
            ),
            NtfsAnomaly::BootBackupShifted { offset } => write!(
                f,
                "la copia del arranque está un sector más allá de lo habitual (byte {offset})"
            ),
            NtfsAnomaly::BootNote(n) => n.fmt(f),
            NtfsAnomaly::SectorSizeMismatch { boot, device } => write!(
                f,
                "el arranque declara sectores de {boot} B y el origen tiene {device} B"
            ),
            NtfsAnomaly::VolumeTruncated {
                volume_bytes,
                source_bytes,
            } => write!(
                f,
                "el volumen declara {volume_bytes} bytes y el origen tiene {source_bytes}: lo que falta no se lee"
            ),
            NtfsAnomaly::MftRecord0FromMirror { reason } => write!(
                f,
                "el registro 0 de $MFT no es utilizable ({reason}): se localiza la MFT con su copia de $MFTMirr"
            ),
            NtfsAnomaly::MftLcnMismatch { boot, runs } => write!(
                f,
                "$MFT empieza en el LCN {runs} según sus runs y en el {boot} según el arranque"
            ),
            NtfsAnomaly::MftSizeNotMultiple {
                data_size,
                record_size,
            } => write!(
                f,
                "el tamaño de $MFT ({data_size}) no es múltiplo del tamaño de registro ({record_size})"
            ),
            NtfsAnomaly::MftIncomplete {
                mapped_records,
                record_count,
            } => write!(
                f,
                "los runs del registro 0 solo cubren {mapped_records} de {record_count} registros (extensiones de $MFT no seguidas)"
            ),
            NtfsAnomaly::MftMirrLcnMismatch { boot, runs } => write!(
                f,
                "$MFTMirr empieza en el LCN {runs} según sus runs y en el {boot} según el arranque"
            ),
            NtfsAnomaly::MftMirrRecordUnusable { reason } => write!(
                f,
                "el registro 1 ($MFTMirr) no es utilizable ({reason}): se comparan {DEFAULT_MIRROR_RECORDS} registros en el LCN del arranque"
            ),
            NtfsAnomaly::MirrorMismatch(m) => {
                write!(
                    f,
                    "el registro {} no coincide con su copia de $MFTMirr ({}: MFT {}, copia {})",
                    m.record,
                    m.kind(),
                    m.mft.label(),
                    m.mirror.label()
                )?;
                if let Some(d) = &m.detail {
                    write!(f, ": {d}")?;
                }
                Ok(())
            }
            NtfsAnomaly::VolumeRecordUnusable { state } => write!(
                f,
                "el registro 3 ($Volume) no es utilizable ({}): sin etiqueta ni versión",
                state.label()
            ),
        }
    }
}

/// Lo que se sabe del volumen al abrirlo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeInfo {
    /// Arranque en uso (el principal o la copia).
    pub boot: BootSector,
    pub boot_source: BootSource,
    /// Examen del arranque principal.
    pub primary_boot: BootProbe,
    pub backup_boot: BackupBoot,
    pub device_sector_size: u32,
    /// Bytes del origen (sectores completos).
    pub source_bytes: u64,
    pub mft: MftLayout,
    pub mft_record0_source: RecordSource,
    pub mirror: MirrorCheck,
    /// Etiqueta (`$VOLUME_NAME` del registro 3).
    pub label: Option<String>,
    /// Versión NTFS (`$VOLUME_INFORMATION`: mayor, menor).
    pub version: Option<(u8, u8)>,
    pub anomalies: Vec<NtfsAnomaly>,
}

impl VolumeInfo {
    /// Confianza del volumen: la peor gravedad de sus anomalías (ADR-022).
    pub fn confidence(&self) -> Confidence {
        let mut fixes = Vec::new();
        let mut suspicious = Vec::new();
        let mut invalid = Vec::new();
        for a in &self.anomalies {
            match a.severity() {
                Severity::Notice => {}
                Severity::Compensated => fixes.push(a.to_string()),
                Severity::Suspicious => suspicious.push(a.to_string()),
                Severity::Invalid => invalid.push(a.to_string()),
            }
        }
        if !invalid.is_empty() {
            Confidence::Invalid(invalid.join("; "))
        } else if !suspicious.is_empty() {
            Confidence::Suspicious(suspicious)
        } else if !fixes.is_empty() {
            Confidence::ValidWithFixes(fixes)
        } else {
            Confidence::Valid
        }
    }
}

/// Volumen NTFS abierto (solo lectura).
pub struct NtfsVolume {
    dev: Arc<dyn BlockDevice>,
    info: VolumeInfo,
    retries: u32,
}

impl fmt::Debug for NtfsVolume {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NtfsVolume")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

fn source_bytes(dev: &dyn BlockDevice) -> u64 {
    dev.sector_count() * u64::from(dev.sector_size())
}

/// Examen de una posición de arranque: resultado y, si es válido, el arranque y sus bytes.
type Probe = (BootProbe, Option<(BootSector, Vec<u8>)>);

/// Examina 512 bytes en `offset` como sector de arranque.
fn probe_boot(dev: &dyn BlockDevice, offset: u64, retries: u32) -> Result<Probe, NtfsError> {
    if offset
        .checked_add(BOOT_SIZE as u64)
        .is_none_or(|e| e > source_bytes(dev))
    {
        return Ok((BootProbe::OutsideSource, None));
    }
    let s = read_span(dev, offset, BOOT_SIZE, retries)?;
    if !s.untrusted.is_empty() {
        let lbas = s.untrusted.iter().map(|&(l, _, _)| l).collect();
        return Ok((BootProbe::Unreadable { lbas }, None));
    }
    Ok(match parse_boot(&s.data) {
        Ok(b) => (BootProbe::Valid, Some((b, s.data))),
        Err(e) => (BootProbe::Invalid(e), None),
    })
}

/// Disposición provisional: `records` registros contiguos desde `lcn`.
fn contiguous_layout(boot: &BootSector, lcn: u64, records: u64) -> MftLayout {
    let rs = u64::from(boot.mft_record_size);
    let cluster = u64::from(boot.cluster_size);
    let bytes = records * rs;
    let length = bytes.div_ceil(cluster).max(1);
    MftLayout {
        record_size: boot.mft_record_size,
        cluster_size: boot.cluster_size,
        runs: vec![Run {
            vcn: 0,
            lcn: Some(lcn),
            length,
        }],
        allocated_size: length * cluster,
        data_size: bytes,
        initialized_size: bytes,
        record_count: records,
    }
}

/// `$DATA` sin nombre, no residente y desde el VCN 0 de un registro: (runs, tamaños).
fn unnamed_data(
    rec: &FileRecord,
    total_clusters: u64,
) -> Result<(Vec<Run>, u64, u64, u64), String> {
    let mut found = None;
    for a in rec.attributes() {
        let a = a.map_err(|e| e.to_string())?;
        if a.type_code == types::DATA && !a.is_named() && found.is_none() {
            match &a.body {
                AttrBody::Resident { .. } => return Err("$DATA residente".into()),
                AttrBody::NonResident(nr) => {
                    if nr.start_vcn != 0 {
                        return Err(format!("$DATA empieza en el VCN {}", nr.start_vcn));
                    }
                    let runs = decode_runs(nr.mapping_pairs, 0, total_clusters)
                        .map_err(|e| e.to_string())?;
                    let end = runs.last().map_or(0, |r| r.end_vcn());
                    if end != nr.last_vcn.wrapping_add(1) {
                        return Err(format!(
                            "los runs cubren {end} clústeres y el atributo dice {}",
                            nr.last_vcn.wrapping_add(1)
                        ));
                    }
                    found = Some((runs, nr.allocated_size, nr.data_size, nr.initialized_size));
                }
            }
        }
    }
    found.ok_or_else(|| "sin $DATA".to_string())
}

/// Disposición de la MFT a partir del registro 0 (de `$MFT` o de `$MFTMirr`).
fn layout_from_record0(
    outcome: RecordOutcome,
    boot: &BootSector,
    anomalies: &mut Vec<NtfsAnomaly>,
) -> Result<MftLayout, String> {
    let rec = match outcome {
        RecordOutcome::Parsed(r) => r,
        other => {
            return Err(other
                .detail()
                .unwrap_or_else(|| other.state().label().to_string()))
        }
    };
    if !rec.in_use() {
        return Err("el registro 0 no está en uso".into());
    }
    if !rec.header().is_base() {
        return Err("el registro 0 es una extensión de otro registro".into());
    }
    let (runs, allocated_size, data_size, initialized_size) =
        unnamed_data(&rec, boot.total_clusters)?;
    let rs = u64::from(boot.mft_record_size);
    if data_size < rs {
        return Err(format!("tamaño de $MFT {data_size} menor que un registro"));
    }
    if data_size > boot.volume_bytes() {
        return Err(format!("tamaño de $MFT {data_size} mayor que el volumen"));
    }
    let Some(first_lcn) = runs.first().and_then(|r| r.lcn) else {
        return Err("el primer run de $MFT no tiene clústeres".into());
    };
    let layout = MftLayout {
        record_size: boot.mft_record_size,
        cluster_size: boot.cluster_size,
        runs,
        allocated_size,
        data_size,
        initialized_size,
        record_count: (data_size / rs).min(MAX_RECORDS),
    };
    if first_lcn != boot.mft_lcn {
        anomalies.push(NtfsAnomaly::MftLcnMismatch {
            boot: boot.mft_lcn,
            runs: first_lcn,
        });
    }
    if !data_size.is_multiple_of(rs) {
        anomalies.push(NtfsAnomaly::MftSizeNotMultiple {
            data_size,
            record_size: boot.mft_record_size,
        });
    }
    let mapped = layout.mapped_records();
    // Con $ATTRIBUTE_LIST la MFT puede seguir en registros de extensión que no se siguen aquí.
    if mapped < layout.record_count {
        anomalies.push(NtfsAnomaly::MftIncomplete {
            mapped_records: mapped,
            record_count: layout.record_count,
        });
    }
    Ok(layout)
}

/// Bytes de un registro sin su matriz de secuencia de actualización (puede diferir entre copias).
fn without_usa(r: &FileRecord) -> Vec<u8> {
    let mut d = r.data().to_vec();
    let h = r.header();
    let a = usize::from(h.usa_offset);
    let b = (a + 2 * usize::from(h.usa_count)).min(d.len());
    d[a..b].fill(0);
    d
}

impl NtfsVolume {
    /// Abre el volumen NTFS que empieza en el byte 0 de `dev` (imagen completa, partición o
    /// volumen encontrado), con [`DEFAULT_RETRIES`] reintentos por sector.
    pub fn open(dev: Arc<dyn BlockDevice>) -> Result<Self, NtfsError> {
        Self::open_with_retries(dev, DEFAULT_RETRIES)
    }

    /// Como [`NtfsVolume::open`], con `retries` reintentos por sector.
    pub fn open_with_retries(dev: Arc<dyn BlockDevice>, retries: u32) -> Result<Self, NtfsError> {
        let d: &dyn BlockDevice = &*dev;
        let size = source_bytes(d);
        let ss = d.sector_size();
        let mut anomalies = Vec::new();

        // 1. Arranque.
        let (primary, parsed) = probe_boot(d, 0, retries)?;
        let (boot, boot_source, backup_boot) = match parsed {
            Some((boot, raw)) => {
                let std = boot.backup_offset();
                let (probe, copy) = probe_boot(d, std, retries)?;
                let mut state = match (&probe, &copy) {
                    (BootProbe::Valid, Some((_, c))) if same_bpb(c, &raw) => BackupState::Matches,
                    (BootProbe::Valid, _) => BackupState::Differs,
                    (BootProbe::Invalid(e), _) => BackupState::Invalid(e.clone()),
                    (BootProbe::Unreadable { lbas }, _) => {
                        BackupState::Unreadable { lbas: lbas.clone() }
                    }
                    (BootProbe::OutsideSource, _) => BackupState::OutsideSource,
                };
                let mut offset = std;
                if state != BackupState::Matches {
                    // Variante con la copia un sector más allá.
                    let alt = std + u64::from(boot.bytes_per_sector);
                    if let (BootProbe::Valid, Some((_, c))) = probe_boot(d, alt, retries)? {
                        if same_bpb(&c, &raw) {
                            anomalies.push(NtfsAnomaly::BootBackupShifted { offset: alt });
                            state = BackupState::Matches;
                            offset = alt;
                        }
                    }
                }
                match &state {
                    BackupState::Differs => {
                        anomalies.push(NtfsAnomaly::BootBackupDiffers { offset })
                    }
                    BackupState::Invalid(e) => anomalies.push(NtfsAnomaly::BootBackupInvalid {
                        offset,
                        error: e.clone(),
                    }),
                    BackupState::Unreadable { lbas } => {
                        anomalies.push(NtfsAnomaly::BootBackupUnreadable {
                            offset,
                            lbas: lbas.clone(),
                        })
                    }
                    BackupState::OutsideSource => {
                        anomalies.push(NtfsAnomaly::BootBackupOutsideSource { offset })
                    }
                    BackupState::Matches | BackupState::InUse => {}
                }
                (boot, BootSource::Primary, BackupBoot { offset, state })
            }
            None => {
                // Copia en el último sector del origen (o en el penúltimo), para cada tamaño de
                // sector posible; solo vale si su propio total la sitúa ahí.
                let mut candidates = Vec::new();
                for b in [u64::from(ss), 512, 1024, 2048, 4096] {
                    let end = size / b * b;
                    for k in 1..=2 {
                        if let Some(o) = end.checked_sub(k * b) {
                            if o > 0 && !candidates.contains(&o) {
                                candidates.push(o);
                            }
                        }
                    }
                }
                let mut first_probe = None;
                let mut chosen = None;
                for o in candidates {
                    let (probe, copy) = probe_boot(d, o, retries)?;
                    if let Some((b, _)) = copy {
                        let std = b.backup_offset();
                        if o == std || o == std + u64::from(b.bytes_per_sector) {
                            chosen = Some((b, o));
                            break;
                        }
                    }
                    first_probe.get_or_insert(probe);
                }
                let Some((boot, offset)) = chosen else {
                    return Err(NtfsError::NotNtfs {
                        primary,
                        backup: first_probe.unwrap_or(BootProbe::OutsideSource),
                    });
                };
                anomalies.push(match &primary {
                    BootProbe::Unreadable { lbas } => {
                        NtfsAnomaly::BootPrimaryUnreadable { lbas: lbas.clone() }
                    }
                    BootProbe::Invalid(e) => NtfsAnomaly::BootPrimaryInvalid(e.clone()),
                    // No ocurre: con una copia válida el origen tiene más de un sector y el
                    // principal se ha leído y no era válido.
                    BootProbe::Valid | BootProbe::OutsideSource => {
                        NtfsAnomaly::BootPrimaryInvalid(BootError::TooShort(0))
                    }
                });
                if offset != boot.backup_offset() {
                    anomalies.push(NtfsAnomaly::BootBackupShifted { offset });
                }
                (
                    boot,
                    BootSource::Backup { offset },
                    BackupBoot {
                        offset,
                        state: BackupState::InUse,
                    },
                )
            }
        };
        anomalies.extend(boot.notes.iter().map(|&n| NtfsAnomaly::BootNote(n)));
        if boot.bytes_per_sector != ss {
            anomalies.push(NtfsAnomaly::SectorSizeMismatch {
                boot: boot.bytes_per_sector,
                device: ss,
            });
        }
        if boot.volume_bytes() > size {
            anomalies.push(NtfsAnomaly::VolumeTruncated {
                volume_bytes: boot.volume_bytes(),
                source_bytes: size,
            });
        }

        // 2. $MFT desde su registro 0 (o desde la copia de $MFTMirr).
        let mft0 = read_record_at(d, &contiguous_layout(&boot, boot.mft_lcn, 1), 0, retries)?;
        let mut mft_anoms = Vec::new();
        let (layout, record0_source) = match layout_from_record0(mft0, &boot, &mut mft_anoms) {
            Ok(l) => (l, RecordSource::Mft),
            Err(reason) => {
                let mirr0 = read_record_at(
                    d,
                    &contiguous_layout(&boot, boot.mftmirr_lcn, 1),
                    0,
                    retries,
                )?;
                mft_anoms.clear();
                match layout_from_record0(mirr0, &boot, &mut mft_anoms) {
                    Ok(l) => {
                        anomalies.push(NtfsAnomaly::MftRecord0FromMirror {
                            reason: reason.clone(),
                        });
                        (l, RecordSource::Mirror)
                    }
                    Err(mirror) => {
                        return Err(NtfsError::MftUnusable {
                            mft: reason,
                            mirror,
                        })
                    }
                }
            }
        };
        anomalies.extend(mft_anoms);

        // 3. $MFTMirr: posición y tamaño según el registro 1; comparación registro a registro.
        let mirror_default = DEFAULT_MIRROR_RECORDS.min(layout.record_count);
        let mut mirror_layout = contiguous_layout(&boot, boot.mftmirr_lcn, mirror_default);
        let rec1 = if layout.record_count > 1 {
            read_record_at(d, &layout, 1, retries)?
        } else {
            RecordOutcome::Unmapped
        };
        let from_rec1 = match &rec1 {
            RecordOutcome::Parsed(r) => unnamed_data(r, boot.total_clusters),
            other => Err(other
                .detail()
                .unwrap_or_else(|| other.state().label().to_string())),
        };
        match from_rec1 {
            Ok((runs, alloc, data, init)) => {
                let rs = u64::from(boot.mft_record_size);
                let n = (data / rs)
                    .clamp(1, MAX_MIRROR_RECORDS)
                    .min(layout.record_count);
                if let Some(l) = runs.first().and_then(|r| r.lcn) {
                    if l != boot.mftmirr_lcn {
                        anomalies.push(NtfsAnomaly::MftMirrLcnMismatch {
                            boot: boot.mftmirr_lcn,
                            runs: l,
                        });
                    }
                }
                mirror_layout = MftLayout {
                    record_size: boot.mft_record_size,
                    cluster_size: boot.cluster_size,
                    runs,
                    allocated_size: alloc,
                    data_size: n * rs,
                    initialized_size: init.min(n * rs),
                    record_count: n,
                };
            }
            Err(reason) => anomalies.push(NtfsAnomaly::MftMirrRecordUnusable { reason }),
        }
        let mut mismatches = Vec::new();
        for i in 0..mirror_layout.record_count {
            let a = read_record_at(d, &layout, i, retries)?;
            let b = read_record_at(d, &mirror_layout, i, retries)?;
            let same = match (a.record(), b.record()) {
                (Some(x), Some(y)) => without_usa(x) == without_usa(y),
                _ => false,
            };
            if !same {
                mismatches.push(MirrorMismatch {
                    record: i,
                    mft: a.state(),
                    mirror: b.state(),
                    detail: b.detail().or_else(|| a.detail()),
                });
            }
        }
        anomalies.extend(mismatches.iter().cloned().map(NtfsAnomaly::MirrorMismatch));
        let mirror = MirrorCheck {
            lcn: boot.mftmirr_lcn,
            runs: mirror_layout.runs.clone(),
            records: mirror_layout.record_count,
            mismatches,
        };

        // 4. $Volume: etiqueta y versión.
        let (mut label, mut version) = (None, None);
        if layout.record_count > 3 {
            match read_record_at(d, &layout, 3, retries)? {
                RecordOutcome::Parsed(r) if r.in_use() => {
                    for a in r.attributes().flatten() {
                        match (a.type_code, a.resident_value()) {
                            (types::VOLUME_NAME, Some(v)) => label = Some(utf16_lossy(v)),
                            (types::VOLUME_INFORMATION, Some(v)) if v.len() >= 10 => {
                                version = Some((v[8], v[9]));
                            }
                            _ => {}
                        }
                    }
                }
                other => anomalies.push(NtfsAnomaly::VolumeRecordUnusable {
                    state: other.state(),
                }),
            }
        }

        let info = VolumeInfo {
            boot,
            boot_source,
            primary_boot: primary,
            backup_boot,
            device_sector_size: ss,
            source_bytes: size,
            mft: layout,
            mft_record0_source: record0_source,
            mirror,
            label,
            version,
            anomalies,
        };
        Ok(NtfsVolume { dev, info, retries })
    }

    pub fn info(&self) -> &VolumeInfo {
        &self.info
    }

    pub fn device(&self) -> &Arc<dyn BlockDevice> {
        &self.dev
    }

    pub fn layout(&self) -> &MftLayout {
        &self.info.mft
    }

    /// Registros de la MFT (`data_size / record_size`).
    pub fn record_count(&self) -> u64 {
        self.info.mft.record_count
    }

    /// Lee el registro `number`. Un registro dañado, ilegible o fuera del origen no es un error:
    /// es su resultado. Error si el número está fuera de la MFT.
    pub fn read_record(&self, number: u64) -> Result<RecordOutcome, NtfsError> {
        if number >= self.record_count() {
            return Err(NtfsError::RecordOutOfRange {
                number,
                count: self.record_count(),
            });
        }
        read_record_at(&*self.dev, &self.info.mft, number, self.retries)
    }

    /// Todos los registros de la MFT en orden, con su resultado.
    pub fn records(&self) -> Records<'_> {
        self.records_in(0..self.record_count())
    }

    /// Registros de `range` (acotado a la MFT) en orden, con su resultado.
    pub fn records_in(&self, range: Range<u64>) -> Records<'_> {
        let end = range.end.min(self.record_count());
        Records {
            dev: &*self.dev,
            layout: &self.info.mft,
            retries: self.retries,
            next: range.start.min(end),
            end,
            pending: Default::default(),
            failed: false,
        }
    }
}

#[cfg(test)]
#[path = "volume_tests.rs"]
mod tests;
