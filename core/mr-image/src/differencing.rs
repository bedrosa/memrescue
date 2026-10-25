//! Discos diferenciales VHD y VHDX con la cadena de padres **indicada por el usuario**
//! (incidencia 8, ADR-018 y su addendum).
//!
//! - El motor nunca abre un padre por su cuenta: el llamador pasa en [`ApprovedFiles`] los ficheros
//!   que el usuario eligió y confirmó, con los papeles [`parent_role`]`(1)` (padre inmediato),
//!   `parent_role(2)`… hasta el disco base. Cualquier otro papel se rechaza.
//! - Cada eslabón se valida antes de usarlo: mismo formato que el hijo, mismo tamaño virtual y
//!   tamaño de sector, vínculo de identidad (VHD: id único del padre = id del padre que guarda el
//!   hijo; VHDX: DataWriteGuid del padre = `parent_linkage` o `parent_linkage2`), sin ciclos (ni
//!   por ruta ni por identidad) y como mucho [`MAX_CHAIN_DEPTH`] padres. Un vínculo que no
//!   coincide se rechaza con `IoError::ExternalMismatch` salvo que el usuario lo acepte de forma
//!   explícita (`allow_mismatch`); entonces la descripción lo advierte.
//! - Si la cadena queda incompleta (el último fichero indicado también es diferencial) la apertura
//!   falla con `IoError::ExternalReference` con la referencia de ese eslabón; si sobran ficheros,
//!   con `ExternalMismatch`.
//! - La lectura de cada capa cae en su padre sector a sector según los mapas de bits del formato;
//!   los errores conservan el desplazamiento virtual (es el mismo en toda la cadena).

use crate::external::{ApprovedFiles, ExternalHint};
use mr_io::{probe_file, BlockDevice, ImageFormat, IoError, Result};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

/// Máximo de padres de una cadena de discos diferenciales.
pub const MAX_CHAIN_DEPTH: usize = 16;

/// Prefijo de los papeles de los padres en [`ApprovedFiles`].
pub const PARENT_ROLE_PREFIX: &str = "parent:";

/// Papel del padre número `n` de la cadena (1 = padre inmediato): `parent:1`, `parent:2`…
pub fn parent_role(n: usize) -> String {
    format!("{PARENT_ROLE_PREFIX}{n}")
}

/// Resultado de comprobar el vínculo entre un diferencial y el padre indicado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Link {
    /// Vínculo verificado, con notas informativas (p. ej. marca de tiempo distinta).
    Verified(Vec<String>),
    /// El vínculo no coincide o no se puede verificar: motivo.
    Mismatch(String),
}

/// Capa de una cadena de discos diferenciales (VHD o VHDX).
pub(crate) trait Layer: BlockDevice + Sized + 'static {
    const FORMAT: &'static str;
    const IMAGE_FORMAT: ImageFormat;
    /// Interpreta el fichero aceptando también discos diferenciales (sin padre enlazado todavía).
    fn open_layer(file: File, tolerant: bool) -> Result<Self>;
    fn is_differencing(&self) -> bool;
    /// Identidad del disco (id único del VHD, DataWriteGuid del VHDX), para detectar ciclos.
    fn identity(&self) -> [u8; 16];
    /// Comprueba que `parent` es el padre que referencia esta capa.
    fn link(&self, parent: &Self, parent_mtime: Option<SystemTime>) -> Link;
    /// Indicaciones sobre el padre (vacío si no es diferencial).
    fn hints(&self) -> Vec<ExternalHint>;
    /// Texto con el que se nombra el padre en `ExternalReference`.
    fn reference(&self) -> String;
    fn attach_parent(&mut self, parent: Arc<dyn BlockDevice>);
    fn describe(&self) -> String;
    fn discarded_blocks(&self) -> u64;
}

/// Cadena abierta: la capa superior con sus padres enlazados.
pub(crate) struct Chain<L> {
    pub top: L,
    pub description: String,
    pub discarded: u64,
    pub parents: Vec<PathBuf>,
}

/// Añade a un error de un padre el número y la ruta del eslabón.
fn in_parent(n: usize, path: &Path, e: IoError) -> IoError {
    let ctx = |d: String| format!("padre {n} ({}): {d}", path.display());
    match e {
        IoError::Corrupt { format, detail } => IoError::Corrupt {
            format,
            detail: ctx(detail),
        },
        IoError::UnsupportedFeature { format, detail } => IoError::UnsupportedFeature {
            format,
            detail: ctx(detail),
        },
        IoError::ExternalMismatch { format, detail } => IoError::ExternalMismatch {
            format,
            detail: ctx(detail),
        },
        other => other,
    }
}

/// Abre la capa superior (`file`, en `canon`) y enlaza los padres aprobados (`parent:1`,
/// `parent:2`…) en orden. Con `allow_mismatch`, un vínculo de identidad que no coincide se acepta y
/// se advierte en la descripción.
pub(crate) fn open_chain<L: Layer>(
    file: File,
    canon: &Path,
    tolerant: bool,
    parents: &ApprovedFiles,
    allow_mismatch: bool,
) -> Result<Chain<L>> {
    let mismatch = |detail: String| IoError::ExternalMismatch {
        format: L::FORMAT,
        detail,
    };
    // Los papeles deben ser exactamente parent:1 … parent:N.
    for e in parents.iter() {
        let ok = e
            .role()
            .strip_prefix(PARENT_ROLE_PREFIX)
            .and_then(|n| n.parse::<usize>().ok())
            .is_some_and(|n| n >= 1 && n <= parents.len() && parent_role(n) == e.role());
        if !ok {
            return Err(mismatch(format!(
                "papel «{}» no válido para un padre (se esperan {}1 … {}{})",
                crate::external::sanitize(e.role()),
                PARENT_ROLE_PREFIX,
                PARENT_ROLE_PREFIX,
                parents.len()
            )));
        }
    }
    let top = L::open_layer(file, tolerant)?;
    let mut layers: Vec<L> = vec![top];
    let mut paths: Vec<PathBuf> = vec![canon.to_path_buf()];
    let mut notes: Vec<String> = Vec::new();
    while layers.last().is_some_and(L::is_differencing) {
        let n = layers.len(); // número del padre que toca (1 = inmediato)
        let child = layers.last().expect("hay al menos una capa");
        let Some(entry) = parents.get(&parent_role(n)) else {
            return Err(IoError::ExternalReference {
                format: L::FORMAT,
                reference: child.reference(),
            });
        };
        let path = &entry.path().to_path_buf();
        if n > MAX_CHAIN_DEPTH {
            return Err(mismatch(format!(
                "la cadena de discos diferenciales tiene más de {MAX_CHAIN_DEPTH} padres"
            )));
        }
        if let Some(k) = paths.iter().position(|p| p == path) {
            return Err(mismatch(format!(
                "ciclo en la cadena: el padre {n} ({}) ya es el eslabón {k} (0 = imagen abierta)",
                path.display()
            )));
        }
        let f = entry.try_clone_file()?;
        let len = f.metadata()?.len();
        let fmt = probe_file(&f, len)?;
        if fmt != L::IMAGE_FORMAT {
            return Err(mismatch(format!(
                "el padre {n} ({}) es {} y el diferencial es {}",
                path.display(),
                fmt.name(),
                L::IMAGE_FORMAT.name()
            )));
        }
        let mtime = f.metadata().ok().and_then(|m| m.modified().ok());
        let parent = L::open_layer(f, tolerant).map_err(|e| in_parent(n, path, e))?;
        if layers.iter().any(|l| l.identity() == parent.identity()) {
            return Err(mismatch(format!(
                "ciclo en la cadena: el padre {n} ({}) tiene la misma identidad que otro eslabón",
                path.display()
            )));
        }
        if parent.size() != child.size() || parent.sector_size() != child.sector_size() {
            return Err(mismatch(format!(
                "el padre {n} ({}) mide {} bytes con sectores de {} B; el diferencial, {} bytes con sectores de {} B",
                path.display(),
                parent.size(),
                parent.sector_size(),
                child.size(),
                child.sector_size()
            )));
        }
        match child.link(&parent, mtime) {
            Link::Verified(v) => notes.extend(v.into_iter().map(|t| format!("padre {n}: {t}"))),
            Link::Mismatch(why) if allow_mismatch => notes.push(format!(
                "AVISO: vínculo con el padre {n} NO verificado ({why}); aceptado por petición explícita del usuario"
            )),
            Link::Mismatch(why) => {
                return Err(mismatch(format!(
                    "el padre {n} ({}) no es el que referencia el diferencial: {why}",
                    path.display()
                )))
            }
        }
        layers.push(parent);
        paths.push(path.clone());
    }
    let used = layers.len() - 1;
    if parents.len() > used {
        return Err(mismatch(format!(
            "se indicaron {} padres y la cadena solo usa {used}: {} no es un disco diferencial",
            parents.len(),
            paths
                .last()
                .map_or_else(String::new, |p| p.display().to_string())
        )));
    }

    let mut description = layers[0].describe();
    for (i, (l, p)) in layers.iter().zip(&paths).enumerate().skip(1) {
        description.push_str(&format!("; padre {i}: {} ({})", p.display(), l.describe()));
    }
    for n in &notes {
        description.push_str("; ");
        description.push_str(n);
    }
    let discarded = layers.iter().map(L::discarded_blocks).sum();
    // Se enlaza desde el disco base hacia arriba.
    let mut below: Option<Arc<dyn BlockDevice>> = None;
    let mut top = None;
    while let Some(mut l) = layers.pop() {
        if let Some(p) = below.take() {
            l.attach_parent(p);
        }
        if layers.is_empty() {
            top = Some(l);
        } else {
            below = Some(Arc::new(l));
        }
    }
    Ok(Chain {
        top: top.expect("la cadena tiene al menos una capa"),
        description,
        discarded,
        parents: paths.split_off(1),
    })
}

/// Indicaciones sobre el padre de `file` (vacío si no es un disco diferencial). El fichero se
/// interpreta en modo tolerante: las entradas de asignación dañadas no impiden leer la referencia.
pub(crate) fn hints_of<L: Layer>(file: File) -> Result<Vec<ExternalHint>> {
    let l = L::open_layer(file, true)?;
    Ok(if l.is_differencing() {
        l.hints()
    } else {
        Vec::new()
    })
}

/// Fecha UTC legible de una marca de tiempo VHD (segundos desde 2000-01-01 00:00:00 UTC).
pub(crate) fn format_vhd_time(secs: u32) -> String {
    format_unix_time(946_684_800 + i64::from(secs))
}

/// Marca de tiempo VHD (segundos desde 2000) de una fecha del sistema, si cabe.
pub(crate) fn vhd_time(t: SystemTime) -> Option<u32> {
    let unix = t.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_secs();
    u32::try_from(unix.checked_sub(946_684_800)?).ok()
}

/// Fecha UTC «AAAA-MM-DD hh:mm:ss UTC» de unos segundos desde 1970 (algoritmo de días civiles).
fn format_unix_time(t: i64) -> String {
    let days = t.div_euclid(86_400);
    let rem = t.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vhd_times_are_formatted_in_utc() {
        assert_eq!(format_vhd_time(0), "2000-01-01 00:00:00 UTC");
        assert_eq!(format_vhd_time(86_400 * 60 - 1), "2000-02-29 23:59:59 UTC");
        assert_eq!(format_vhd_time(u32::MAX), "2136-02-07 06:28:15 UTC");
        let t = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(946_684_800 + 12_345);
        assert_eq!(vhd_time(t), Some(12_345));
        assert_eq!(vhd_time(SystemTime::UNIX_EPOCH), None);
    }
}
