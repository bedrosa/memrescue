//! Lector NTFS (M3, issue #20): sector de arranque, localización de la MFT y registros FILE.
//!
//! Solo lectura y tolerante a daños, a partir de la documentación pública del formato en disco de
//! NTFS (estructuras del sector de arranque, de los registros FILE con su secuencia de
//! actualización, de las cabeceras de atributo y de los *mapping pairs*). Detalle, límites y
//! tolerancias en `docs/formatos/ntfs.md`.
//!
//! - [`boot`]: sector de arranque con validación estricta y límites.
//! - [`record`]: registro FILE: secuencia de actualización (fixups) comprobada por tramo y cabecera.
//! - [`attr`]: iterador acotado de cabeceras de atributo.
//! - [`runs`]: decodificación de *mapping pairs* (lo mínimo para la MFT; #21 lo amplía).
//! - [`mft`]: disposición de la MFT, lectura de registros por número y enumeración.
//! - [`volume`]: [`NtfsVolume::open`]: arranque principal o copia, `$MFT`, `$MFTMirr`, etiqueta.
//!
//! Contrato común con `mr-part`: toda lectura pasa por `mr_part::sector::read_trusted` (un sector
//! ilegible nunca se interpreta), aritmética comprobada, límites explícitos, ningún pánico con datos
//! del origen y confianza (ADR-003, `mr_common::Confidence`) en lugar de aceptar o rechazar.

pub mod attr;
pub mod boot;
pub mod mft;
mod read;
pub mod record;
pub mod runs;
pub mod volume;

#[cfg(test)]
mod testvol;

pub use boot::{parse_boot, BootError, BootSector};
pub use mft::{MftLayout, RecordOutcome, RecordState, Records};
pub use mr_common::Confidence;
pub use mr_part::Severity;
pub use record::{FileRecord, FileReference, RecordDamage, RecordHeader};
pub use volume::{
    BackupBoot, BackupState, BootProbe, BootSource, MirrorCheck, MirrorMismatch, NtfsAnomaly,
    NtfsVolume, RecordSource, VolumeInfo,
};

use thiserror::Error;

/// Error al abrir un volumen o leer un registro. Los daños de estructuras concretas no son errores:
/// se informan como anomalías o como estado del registro.
#[derive(Debug, Error)]
pub enum NtfsError {
    /// Fallo de E/S no recuperable (no un sector ilegible: esos se informan en el resultado).
    #[error(transparent)]
    Io(#[from] mr_io::IoError),
    /// Ni el arranque principal ni su copia son un arranque NTFS válido.
    #[error("no es un volumen NTFS: arranque principal: {primary}; copia: {backup}")]
    NotNtfs {
        primary: BootProbe,
        backup: BootProbe,
    },
    /// No se puede localizar la MFT (registro 0 inutilizable en `$MFT` y en `$MFTMirr`).
    #[error("no se puede localizar la MFT: en $MFT: {mft}; en $MFTMirr: {mirror}")]
    MftUnusable { mft: String, mirror: String },
    /// Número de registro fuera de la MFT.
    #[error("el registro {number} no existe (la MFT tiene {count})")]
    RecordOutOfRange { number: u64, count: u64 },
}
