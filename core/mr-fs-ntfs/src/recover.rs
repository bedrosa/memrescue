//! Recuperación del contenido de un fichero borrado con el estado de cada tramo (#23).
//!
//! [`recover_stream`] evalúa el registro con [`scan_deleted`] (centrado en ese registro: los
//! demás borrados se recorren igual para detectar clústeres compartidos, y sin límite de tramos
//! para el flujo pedido), abre el flujo y marca como sospechosos
//! ([`DataStream::with_suspect_clusters`]) todos los clústeres que no están libres. La copia
//! informa por tramo de bytes: `data` (clústeres libres: contenido original salvo escrituras sin
//! asignar, que no se pueden detectar), `possibly_overwritten` (bytes tal como están en el disco,
//! de clústeres con otro dueño, asignados o de asignación desconocida), los no fiables de siempre
//! con el patrón de relleno (`unreadable`, `unmapped`, `outside_source`, `corrupt`) y los ceros
//! del formato (`sparse`, `uninitialized`). Un registro dañado o ilegible nunca se recupera
//! (`NtfsError::RecordUnusable`). Un registro en uso se copia tal cual (sin evaluación).

use std::collections::BTreeMap;

use crate::deleted::{
    attach_backrefs, scan_deleted, ClusterState, OwnerInfo, ScanObserver, ScanOptions,
    StreamAssessment,
};
use crate::mft::RecordState;
use crate::stream::{CopyError, CopyReport, DataStream};
use crate::{NtfsError, NtfsVolume};

/// Tramo del flujo con clústeres sospechosos.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SuspectRange {
    /// Bytes del flujo `[offset, offset + length)` (recortados al tamaño de los datos). En un
    /// flujo comprimido los bytes afectados son unidades de compresión enteras: el informe de la
    /// copia tiene el detalle exacto.
    pub offset: u64,
    pub length: u64,
    pub vcn: u64,
    pub lcn: u64,
    pub clusters: u64,
    pub state: ClusterState,
}

/// Informe de una recuperación.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryReport {
    pub record: u64,
    pub sequence: u16,
    pub stream: String,
    /// El registro estaba en uso (copia normal, sin evaluación).
    pub in_use: bool,
    /// Evaluación del flujo (solo borrados).
    pub assessment: Option<StreamAssessment>,
    /// Bytes por estado y tramos no fiables (incluidos los `possibly_overwritten`).
    pub copy: CopyReport,
    /// Tramos con clústeres sospechosos y su estado (dueño incluido).
    pub suspect: Vec<SuspectRange>,
    /// Dueños vivos nombrados en `suspect`.
    pub owners: BTreeMap<u64, OwnerInfo>,
    /// Texto del motivo (como el `reason` del manifiesto).
    pub summary: Option<String>,
}

impl RecoveryReport {
    /// `true` si todo lo escrito es contenido fiable (ni relleno ni posiblemente sobrescrito).
    pub fn is_clean(&self) -> bool {
        self.copy.untrusted_bytes() == 0
    }
}

/// Abre el flujo `name` del registro `record` preparado para recuperar: si está borrado, con su
/// evaluación y los clústeres sospechosos marcados. `Ok(None)` si se canceló.
pub fn recovery_stream<'v>(
    vol: &'v NtfsVolume,
    record: u64,
    name: &str,
    obs: &dyn ScanObserver,
) -> Result<Option<(DataStream<'v>, RecoveryReport)>, NtfsError> {
    let mut f = vol.file(record)?;
    let mut report = RecoveryReport {
        record,
        sequence: f.header.sequence,
        stream: name.to_string(),
        in_use: f.in_use(),
        assessment: None,
        copy: CopyReport::default(),
        suspect: Vec::new(),
        owners: BTreeMap::new(),
        summary: None,
    };
    if f.in_use() {
        let s = vol.stream_of(&f, name)?;
        return Ok(Some((s, report)));
    }
    let opts = ScanOptions {
        focus: Some(record),
        max_ranges_per_stream: usize::MAX,
        ..ScanOptions::default()
    };
    let Some(scan) = scan_deleted(vol, &opts, obs)? else {
        return Ok(None);
    };
    let Some(entry) = scan.entry(record) else {
        return Err(NtfsError::RecordUnusable {
            number: record,
            state: RecordState::NotInUse,
            detail: Some("registro libre: sin $FILE_NAME, no es un fichero borrado".into()),
        });
    };
    if !entry.backref_extensions.is_empty() {
        let c: Vec<(u64, u16)> = entry
            .backref_extensions
            .iter()
            .map(|&e| (e, f.header.sequence))
            .collect();
        attach_backrefs(vol, &mut f, &c);
    }
    // Errores de disposición como en una lectura normal.
    let s = vol.stream_of(&f, name)?;
    let Some(a) = entry.streams.iter().find(|x| x.name == name).cloned() else {
        return Err(NtfsError::RecordUnusable {
            number: record,
            state: RecordState::NotInUse,
            detail: Some(format!("el flujo «{name}» no se ha evaluado")),
        });
    };
    let cluster = u64::from(s.layout().cluster_size);
    let size = s.size();
    for r in a.ranges.iter().filter(|r| r.state.is_suspect()) {
        let offset = r.vcn.saturating_mul(cluster);
        if offset >= size && !s.layout().is_compressed() {
            continue;
        }
        let length = r
            .length
            .saturating_mul(cluster)
            .min(size.saturating_sub(offset));
        report.suspect.push(SuspectRange {
            offset,
            length,
            vcn: r.vcn,
            lcn: r.lcn,
            clusters: r.length,
            state: r.state,
        });
        if let ClusterState::Owned { owner } = r.state {
            if let Some(o) = scan.owners.get(&owner) {
                report.owners.insert(owner, o.clone());
            }
        }
    }
    report.summary = Some(scan.summary(&a));
    let s = s.with_suspect_clusters(a.suspect_clusters());
    report.assessment = Some(a);
    Ok(Some((s, report)))
}

/// Copia el flujo `name` del registro `record` a `w` (ver el módulo). `Ok(None)` si se canceló.
pub fn recover_stream(
    vol: &NtfsVolume,
    record: u64,
    name: &str,
    w: &mut dyn std::io::Write,
    obs: &dyn ScanObserver,
) -> Result<Option<RecoveryReport>, CopyError> {
    let Some((s, mut report)) = recovery_stream(vol, record, name, obs)? else {
        return Ok(None);
    };
    report.copy = s.copy_to(w)?;
    Ok(Some(report))
}
