//! Destino de la recuperación (INV-01 §7.1, §7.4 y §7.9).
//!
//! **Raíz.** Se valida una vez antes de leer nada: no es una ruta de dispositivo (política de
//! rutas de ADR-014, `mr_io::path_policy`), no es ni está dentro del origen, de uno de sus
//! segmentos ni de sus ficheros externos ([`OutputSink::is_source`]), se crea si no existe y su
//! forma canónica (`\\?\X:\...` en Windows) es un directorio sin punto de reanálisis. Todas las
//! rutas de E/S se construyen desde esa forma canónica, así que las rutas largas (más de 259
//! caracteres) funcionan sin depender de `LongPathsEnabled`.
//!
//! **Contención.** Los nombres del origen se escapan ([`crate::names`]) y nunca contienen
//! separadores ni son `.`/`..`; además, cada directorio creado o reutilizado se comprueba: su forma
//! canónica sigue dentro de la raíz y no es un punto de reanálisis (un enlace o unión creado por
//! otro proceso entre la creación y el uso se rechaza).
//!
//! **Capacidades** ([`Capabilities`]), con una sonda en la raíz (ficheros `.mrprobe-*` que se
//! borran al momento):
//! - flujos alternativos: se crea un fichero y se intenta abrir `fichero:mrprobe`; si funciona, el
//!   destino admite flujos con nombre (NTFS, ReFS) y los flujos alternativos se escriben como
//!   flujos; si no (FAT, exFAT, red), como ficheros hermanos `nombre%3Aflujo`;
//! - mayúsculas: se crea `.mrprobe-<id>-a` y se mira si existe `.MRPROBE-<id>-A`; si no, el destino
//!   distingue mayúsculas y la comparación de nombres de la recuperación también;
//! - tamaño máximo de fichero: solo si no admite flujos y hace falta (un flujo de más de
//!   4 GiB − 1), se intenta fijar el tamaño de un fichero de sonda en 4 GiB; si el sistema lo
//!   rechaza por tamaño (no por espacio libre), el límite es 4 GiB − 1 (FAT32) y se divide.
//!
//! Las pruebas pueden fijar las capacidades ([`crate::RecoverOptions::capabilities`]) para
//! simular un destino sin flujos o FAT32.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Límite de tamaño de un fichero en FAT32.
pub const FAT32_MAX_FILE: u64 = (4 << 30) - 1;

/// Capacidades del destino.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// Admite flujos con nombre (`fichero:flujo`).
    pub named_streams: bool,
    /// Distingue mayúsculas en los nombres.
    pub case_sensitive: bool,
    /// Tamaño máximo de un fichero (`None` = sin límite conocido).
    pub max_file_size: Option<u64>,
    /// Origen de los valores: `"sonda"` o `"opciones"`.
    pub source: &'static str,
}

/// Error al crear o publicar una salida.
#[derive(Debug)]
pub enum CreateError {
    /// El nombre ya existe (antes de crear o al publicar sin sustituir).
    Exists(PathBuf),
    /// Ruta rechazada por la política (origen, dispositivo, directorio).
    Rejected(String),
    Io(std::io::Error),
}

impl std::fmt::Display for CreateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CreateError::Exists(p) => write!(f, "{} ya existe", p.display()),
            CreateError::Rejected(m) => write!(f, "ruta rechazada: {m}"),
            CreateError::Io(e) => write!(f, "{e}"),
        }
    }
}

/// Fichero de salida en curso: se escribe en un temporal y solo [`PendingFile::commit`] lo
/// publica con su nombre final (nunca queda un parcial con el nombre final). Al descartarlo sin
/// confirmar, el temporal se borra.
pub trait PendingFile: std::io::Write {
    /// Ruta del temporal (los flujos alternativos se escriben en `temporal:flujo` antes de
    /// confirmar).
    fn temp_path(&self) -> &Path;
    /// Vacía, sincroniza y publica. Sin sustitución pedida, falla con [`CreateError::Exists`] si
    /// el nombre apareció entretanto (el fichero ajeno no se toca) y el temporal se borra.
    fn commit(self: Box<Self>) -> Result<PathBuf, CreateError>;
}

/// Creación de salidas con la política común de la API (`mr_api::create_output`): nunca el
/// origen ni un dispositivo ni un directorio, temporal en el mismo directorio y publicación
/// atómica sin sustituir salvo petición explícita.
pub trait OutputSink: Sync {
    fn create(&self, path: &Path, overwrite: bool) -> Result<Box<dyn PendingFile>, CreateError>;
    /// `true` si `path` (canónica) es el origen, uno de sus segmentos o ficheros externos, o
    /// está dentro de uno de ellos.
    fn is_source(&self, path: &Path) -> bool;
    /// Rutas del origen (para el diario).
    fn source_paths(&self) -> Vec<PathBuf> {
        Vec::new()
    }
}

/// Raíz de destino validada.
#[derive(Debug, Clone)]
pub struct Root {
    /// Forma canónica (en Windows, `\\?\X:\...`).
    pub canonical: PathBuf,
    /// Forma para mostrar (sin el prefijo `\\?\`).
    pub display: String,
}

fn rejected(m: impl Into<String>) -> crate::RecoverError {
    crate::RecoverError::Destination(m.into())
}

/// Forma para mostrar de una ruta canónica (quita `\\?\` y `\\?\UNC\`).
pub fn display_path(p: &Path) -> String {
    let s = p.to_string_lossy();
    if let Some(r) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{r}")
    } else if let Some(r) = s.strip_prefix(r"\\?\") {
        r.to_string()
    } else {
        s.into_owned()
    }
}

/// `true` si `p` es un punto de reanálisis (enlace simbólico, unión...) sin seguirlo.
pub fn is_reparse_point(p: &Path) -> bool {
    match std::fs::symlink_metadata(p) {
        Ok(m) => {
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
                m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
                    || m.file_type().is_symlink()
            }
            #[cfg(not(windows))]
            {
                m.file_type().is_symlink()
            }
        }
        Err(_) => false,
    }
}

/// Comparación de rutas canónicas sin distinguir mayúsculas (Windows).
fn starts_with_ci(p: &Path, base: &Path) -> bool {
    let a = p.to_string_lossy().to_lowercase();
    let b = base.to_string_lossy().to_lowercase();
    let b = b.trim_end_matches(['\\', '/']);
    a == b || a.starts_with(&format!("{b}\\")) || a.starts_with(&format!("{b}/"))
}

/// Valida y prepara la raíz de destino (ver el módulo).
pub fn prepare_root(dest: &Path, sink: &dyn OutputSink) -> Result<Root, crate::RecoverError> {
    let raw = dest.to_string_lossy();
    if raw.trim().is_empty() {
        return Err(rejected("destino vacío"));
    }
    mr_io::path_policy::check_lexical(&raw).map_err(|e| rejected(e.to_string()))?;
    // Antes de crear nada: ni el origen ni algo dentro de él (con la ruta tal cual y, si ya
    // existe algún antecesor, con su forma canónica).
    if sink.is_source(dest) {
        return Err(rejected(format!(
            "el destino {} es el origen o está dentro de él: nunca se escribe en el origen",
            dest.display()
        )));
    }
    let mut probe = dest.to_path_buf();
    while !probe.exists() {
        match probe.parent() {
            Some(p) if !p.as_os_str().is_empty() => probe = p.to_path_buf(),
            _ => break,
        }
    }
    if let Ok(c) = std::fs::canonicalize(&probe) {
        if c.is_file() || sink.is_source(&c) {
            return Err(rejected(format!(
                "el destino {} está dentro de un fichero o del origen",
                dest.display()
            )));
        }
    }
    if dest.exists() && !dest.is_dir() {
        return Err(rejected(format!(
            "el destino {} existe y no es un directorio",
            dest.display()
        )));
    }
    std::fs::create_dir_all(dest).map_err(|e| {
        rejected(format!(
            "no se pudo crear el destino {}: {e}",
            dest.display()
        ))
    })?;
    let canonical = std::fs::canonicalize(dest)
        .map_err(|e| rejected(format!("destino {}: {e}", dest.display())))?;
    mr_io::path_policy::check_canonical(&canonical.to_string_lossy())
        .map_err(|e| rejected(e.to_string()))?;
    if !canonical.is_dir() {
        return Err(rejected(format!(
            "el destino {} no es un directorio",
            dest.display()
        )));
    }
    if is_reparse_point(dest) {
        return Err(rejected(format!(
            "el destino {} es un punto de reanálisis (enlace o unión): elija la carpeta real",
            dest.display()
        )));
    }
    if sink.is_source(&canonical) {
        return Err(rejected(format!(
            "el destino {} es el origen o está dentro de él",
            dest.display()
        )));
    }
    Ok(Root {
        display: display_path(&canonical),
        canonical,
    })
}

/// Comprueba que un directorio creado o reutilizado sigue dentro de la raíz y no es un punto
/// de reanálisis.
pub fn check_contained(root: &Root, dir: &Path) -> Result<(), String> {
    if is_reparse_point(dir) {
        return Err(format!(
            "{} es un punto de reanálisis (enlace o unión): no se escribe a través de él",
            display_path(dir)
        ));
    }
    let c = std::fs::canonicalize(dir).map_err(|e| format!("{}: {e}", display_path(dir)))?;
    if !starts_with_ci(&c, &root.canonical) {
        return Err(format!(
            "{} queda fuera del destino {}",
            display_path(&c),
            root.display
        ));
    }
    Ok(())
}

static PROBE: AtomicU64 = AtomicU64::new(0);

fn probe_name(tag: &str) -> String {
    format!(
        ".mrprobe-{}-{}-{tag}",
        std::process::id(),
        PROBE.fetch_add(1, Ordering::Relaxed)
    )
}

/// Sonda de flujos con nombre y de mayúsculas en la raíz (ver el módulo).
pub fn probe(root: &Root) -> Capabilities {
    let base = probe_name("a");
    let p = root.canonical.join(&base);
    let mut caps = Capabilities {
        named_streams: false,
        case_sensitive: false,
        max_file_size: None,
        source: "sonda",
    };
    if std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&p)
        .is_ok()
    {
        let ads = PathBuf::from(format!("{}:mrprobe", p.to_string_lossy()));
        caps.named_streams = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&ads)
            .is_ok();
        let upper = root.canonical.join(base.to_uppercase());
        caps.case_sensitive = !upper.exists();
        let _ = std::fs::remove_file(&p);
    }
    caps
}

/// Sonda del límite de 4 GiB (solo en destinos sin flujos con nombre): fija el tamaño de un
/// fichero vacío en 4 GiB. Un error que no es de espacio libre indica un sistema con el límite de
/// FAT32.
pub fn probe_max_file_size(root: &Root) -> Option<u64> {
    let p = root.canonical.join(probe_name("t"));
    let f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&p)
        .ok()?;
    let r = f.set_len(FAT32_MAX_FILE + 1);
    drop(f);
    let _ = std::fs::remove_file(&p);
    match r {
        Ok(()) => None,
        // ERROR_DISK_FULL (112), ERROR_HANDLE_DISK_FULL (39), ENOSPC (28): sin espacio, no límite.
        Err(e) if matches!(e.raw_os_error(), Some(112 | 39 | 28)) => None,
        Err(_) => Some(FAT32_MAX_FILE),
    }
}

/// FILETIME (100 ns desde 1601) como `SystemTime` (`None` si es 0).
pub fn filetime_to_system(ft: u64) -> Option<std::time::SystemTime> {
    if ft == 0 {
        return None;
    }
    const EPOCH: u64 = 116_444_736_000_000_000;
    let unix = std::time::UNIX_EPOCH;
    if ft >= EPOCH {
        let d = ft - EPOCH;
        unix.checked_add(std::time::Duration::new(
            d / 10_000_000,
            (d % 10_000_000) as u32 * 100,
        ))
    } else {
        let d = EPOCH - ft;
        unix.checked_sub(std::time::Duration::new(
            d / 10_000_000,
            (d % 10_000_000) as u32 * 100,
        ))
    }
}

/// FILETIME de un `SystemTime` (pruebas y comparación).
pub fn system_to_filetime(t: std::time::SystemTime) -> u64 {
    const EPOCH: u64 = 116_444_736_000_000_000;
    match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => EPOCH + d.as_secs() * 10_000_000 + u64::from(d.subsec_nanos() / 100),
        Err(e) => {
            let d = e.duration();
            EPOCH.saturating_sub(d.as_secs() * 10_000_000 + u64::from(d.subsec_nanos() / 100))
        }
    }
}

/// Fija las fechas (creación, modificación, acceso) de un fichero o directorio ya escrito. En
/// directorios se abre con `FILE_FLAG_BACKUP_SEMANTICS`. Solo se fijan las fechas conocidas.
pub fn set_times(
    p: &Path,
    created: u64,
    modified: u64,
    accessed: u64,
    dir: bool,
) -> std::io::Result<()> {
    let mut t = std::fs::FileTimes::new();
    let mut any = false;
    if let Some(m) = filetime_to_system(modified) {
        t = t.set_modified(m);
        any = true;
    }
    if let Some(a) = filetime_to_system(accessed) {
        t = t.set_accessed(a);
        any = true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileTimesExt;
        if let Some(c) = filetime_to_system(created) {
            t = t.set_created(c);
            any = true;
        }
    }
    #[cfg(not(windows))]
    let _ = created;
    if !any {
        return Ok(());
    }
    let mut o = std::fs::OpenOptions::new();
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        o.access_mode(FILE_WRITE_ATTRIBUTES);
        o.custom_flags(
            FILE_FLAG_OPEN_REPARSE_POINT | if dir { FILE_FLAG_BACKUP_SEMANTICS } else { 0 },
        );
    }
    #[cfg(not(windows))]
    {
        if dir {
            o.read(true);
        } else {
            o.write(true);
        }
    }
    let f = o.open(p)?;
    f.set_times(t)
}

/// Marca un fichero como de solo lectura (al final, tras fijar las fechas).
pub fn set_readonly(p: &Path) -> std::io::Result<()> {
    let mut perm = std::fs::metadata(p)?.permissions();
    perm.set_readonly(true);
    std::fs::set_permissions(p, perm)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetime_round_trip() {
        for ft in [1u64, 116_444_736_000_000_000, 134_116_992_000_000_001] {
            assert_eq!(system_to_filetime(filetime_to_system(ft).unwrap()), ft);
        }
        assert!(filetime_to_system(0).is_none());
        assert_eq!(display_path(Path::new(r"\\?\C:\x")), r"C:\x");
        assert_eq!(display_path(Path::new(r"\\?\UNC\s\r\x")), r"\\s\r\x");
        assert!(starts_with_ci(Path::new(r"C:\A\b"), Path::new(r"c:\a")));
        assert!(!starts_with_ci(Path::new(r"C:\Ab"), Path::new(r"c:\a")));
    }
}
