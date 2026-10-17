//! Política de admisión de rutas: solo se aceptan **ficheros normales** en volúmenes con letra o
//! recursos UNC. Cualquier forma de ruta que pueda designar un dispositivo (disco físico, volumen,
//! CD, tubería, consola…) se rechaza **antes de abrir nada**.
//!
//! Defensa en profundidad:
//! 1. Comprobación léxica con lista blanca (no lista negra) de formas de ruta.
//! 2. Resolución manual de la cadena de enlaces simbólicos/uniones del componente final con
//!    `read_link`, que **no abre** el destino; cada destino pasa la comprobación léxica.
//! 3. Ruta canónica obligatoria con forma `\\?\X:\…` o `\\?\UNC\…`.
//! 4. Apertura con `FILE_FLAG_OPEN_REPARSE_POINT` y verificación de que el objeto abierto es un
//!    fichero normal (ni directorio, ni dispositivo, ni punto de reanálisis). Así, si el fichero se
//!    sustituye por un enlace entre las comprobaciones y la apertura, se rechaza.

use crate::{IoError, Result};
use std::path::{Path, PathBuf};

const MAX_LINK_HOPS: usize = 32;

/// Nombres reservados de dispositivos DOS/Windows (se comparan sin extensión ni flujo).
const RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "conin$", "conout$", "clock$", "com0", "com1", "com2", "com3",
    "com4", "com5", "com6", "com7", "com8", "com9", "lpt0", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5",
    "lpt6", "lpt7", "lpt8", "lpt9", "com¹", "com²", "com³", "lpt¹", "lpt²", "lpt³",
];

fn reject(path: &str, why: &str) -> IoError {
    IoError::DevicePathRejected(format!("{path} ({why})"))
}

fn is_drive_prefix(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\'
}

/// Comprobación léxica (no toca el sistema de archivos). Pública para pruebas.
pub fn check_lexical(raw: &str) -> Result<()> {
    if raw.is_empty() {
        return Err(reject(raw, "ruta vacía"));
    }
    if raw.contains('\0') {
        return Err(reject(raw, "carácter nulo"));
    }
    let s = raw.replace('/', "\\");
    let lower = s.to_lowercase();

    // Espacio de nombres de dispositivos y del administrador de objetos.
    for p in [
        "\\\\.\\",
        "\\??\\",
        "\\device\\",
        "\\global??\\",
        "\\globalroot\\",
    ] {
        if lower.starts_with(p) {
            return Err(reject(raw, "espacio de nombres de dispositivos"));
        }
    }
    if let Some(rest) = lower.strip_prefix("\\\\?\\") {
        // Solo `\\?\X:\…` (unidad con letra y ruta) o `\\?\UNC\servidor\recurso\…`.
        let ok = is_drive_prefix(rest) || rest.starts_with("unc\\");
        if !ok {
            return Err(reject(raw, "ruta extendida que no es de fichero"));
        }
    }
    // Tuberías y mailslots, también remotos: `\\srv\pipe\x`, `\\?\UNC\srv\pipe\x`.
    let unc_rest = lower
        .strip_prefix("\\\\?\\unc\\")
        .or_else(|| lower.strip_prefix("\\\\"));
    if let Some(rest) = unc_rest {
        let mut parts = rest.split('\\');
        let _server = parts.next();
        if matches!(parts.next(), Some("pipe" | "mailslot")) {
            return Err(reject(raw, "tubería o mailslot"));
        }
    }
    // Componentes: nombres reservados de dispositivo.
    for comp in s.split('\\').filter(|c| !c.is_empty()) {
        let name = comp.split(':').next().unwrap_or("");
        let stem = name
            .split('.')
            .next()
            .unwrap_or("")
            .trim_end_matches(' ')
            .to_lowercase();
        if RESERVED.contains(&stem.as_str()) {
            return Err(reject(raw, "nombre reservado de dispositivo"));
        }
    }
    Ok(())
}

/// Comprueba la forma canónica que devuelve el sistema tras resolver la ruta.
pub fn check_canonical(canon: &str) -> Result<()> {
    check_lexical(canon)?;
    let lower = canon.to_lowercase();
    let ok = match lower.strip_prefix("\\\\?\\") {
        Some(rest) => is_drive_prefix(rest) || rest.starts_with("unc\\"),
        None => cfg!(not(windows)) && canon.starts_with('/') && !canon.starts_with("/dev/"),
    };
    if ok {
        Ok(())
    } else {
        Err(reject(
            canon,
            "la ruta canónica no es un fichero de un volumen con letra o UNC",
        ))
    }
}

/// Recorre la cadena de enlaces del último componente sin abrir los destinos.
fn resolve_link_chain(path: &Path) -> Result<PathBuf> {
    let mut cur = path.to_path_buf();
    for _ in 0..MAX_LINK_HOPS {
        let md = match std::fs::symlink_metadata(&cur) {
            Ok(m) => m,
            Err(e) => return Err(IoError::Io(e)),
        };
        if !md.file_type().is_symlink() {
            return Ok(cur);
        }
        let target = std::fs::read_link(&cur)?;
        check_lexical(&target.to_string_lossy())?;
        cur = if target.is_absolute() {
            target
        } else {
            cur.parent().unwrap_or(Path::new(".")).join(target)
        };
    }
    Err(reject(
        &path.display().to_string(),
        "demasiados enlaces encadenados",
    ))
}

/// Admite una ruta: devuelve su forma canónica si designa (de momento) un fichero normal.
pub fn admit(path: &Path) -> Result<PathBuf> {
    check_lexical(&path.to_string_lossy())?;
    let resolved = resolve_link_chain(path)?;
    let canon = std::fs::canonicalize(&resolved)?;
    check_canonical(&canon.to_string_lossy())?;
    if !std::fs::metadata(&canon)?.is_file() {
        return Err(reject(
            &canon.display().to_string(),
            "no es un fichero normal",
        ));
    }
    Ok(canon)
}

/// Abre en solo lectura una ruta ya admitida y verifica el objeto abierto.
pub fn open_regular_file(canon: &Path) -> Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true).write(false);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        opts.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = opts.open(canon)?;
    let md = file.metadata()?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const DIRECTORY: u32 = 0x10;
        const DEVICE: u32 = 0x40;
        const REPARSE_POINT: u32 = 0x400;
        if md.file_attributes() & (DIRECTORY | DEVICE | REPARSE_POINT) != 0 {
            return Err(reject(
                &canon.display().to_string(),
                "el objeto abierto no es un fichero normal",
            ));
        }
    }
    if !md.is_file() {
        return Err(reject(
            &canon.display().to_string(),
            "no es un fichero normal",
        ));
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_rejects_every_device_form() {
        for p in [
            r"\\.\PhysicalDrive0",
            r"\\.\C:",
            r"//./PhysicalDrive1",
            r"\\?\C:",
            r"\\?\c:",
            r"\\?\PhysicalDrive0",
            r"\\?\HarddiskVolume1",
            r"\\?\Harddisk0Partition1",
            r"\\?\CdRom0",
            r"\\?\GLOBALROOT\Device\Harddisk0\Partition0",
            r"\\?\Volume{00000000-0000-0000-0000-000000000000}",
            r"\\?\Volume{00000000-0000-0000-0000-000000000000}\x.img",
            r"\??\C:\x.img",
            r"\Device\Harddisk0\DR0",
            r"\\.\pipe\algo",
            r"\\servidor\pipe\algo",
            r"\\?\UNC\servidor\PIPE\algo",
            "NUL",
            "con",
            r"C:\carpeta\nul",
            r"C:\carpeta\COM1.img",
            r"C:\carpeta\aux.txt:flujo",
            r"C:\carpeta\LPT9 .img",
            "",
            "a\0b",
        ] {
            assert!(
                matches!(check_lexical(p), Err(IoError::DevicePathRejected(_))),
                "no rechazó {p:?}"
            );
        }
    }

    #[test]
    fn lexical_accepts_normal_file_paths() {
        for p in [
            r"C:\imagenes\disco.img",
            r"\\?\C:\imagenes\disco.img",
            r"\\?\UNC\servidor\recurso\disco.img",
            r"\\servidor\recurso\disco.img",
            r"relativa\physicaldrive-copia.img",
            r"C:\a\consola.img",
            r"C:\a\com10.img",
            "disco.img",
        ] {
            assert!(check_lexical(p).is_ok(), "rechazó {p:?}");
        }
    }

    #[test]
    fn canonical_requires_drive_or_unc() {
        #[cfg(windows)]
        {
            assert!(check_canonical(r"\\?\C:\x\y.img").is_ok());
            assert!(check_canonical(r"\\?\UNC\srv\share\y.img").is_ok());
            assert!(check_canonical(r"C:\x\y.img").is_err());
            assert!(check_canonical(r"\\?\GLOBALROOT\Device\HarddiskVolume5\y.img").is_err());
            assert!(check_canonical(r"\\?\Volume{1}\y.img").is_err());
        }
    }

    #[test]
    fn admits_regular_file_and_rejects_directory() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.img");
        std::fs::write(&f, [0u8; 512]).unwrap();
        let canon = admit(&f).unwrap();
        assert!(open_regular_file(&canon).is_ok());
        assert!(matches!(
            admit(dir.path()),
            Err(IoError::DevicePathRejected(_))
        ));
        // Aunque se salte `admit`, la apertura tampoco acepta un directorio.
        assert!(open_regular_file(&std::fs::canonicalize(dir.path()).unwrap()).is_err());
    }

    /// Las uniones de directorio (que también usan los puntos de montaje) se crean sin privilegios.
    /// Una imagen alcanzada a través de una unión es un fichero normal y debe admitirse; la propia
    /// unión (directorio) debe rechazarse al abrir.
    #[cfg(windows)]
    #[test]
    fn junction_to_directory_is_followed_but_never_opened_as_image() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("d.img"), [1u8; 1024]).unwrap();
        let junc = dir.path().join("union");
        let st = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&junc)
            .arg(&real)
            .output()
            .unwrap();
        assert!(st.status.success(), "no se pudo crear la unión");
        let via = admit(&junc.join("d.img")).unwrap();
        assert!(via.to_string_lossy().contains("real"), "{}", via.display());
        assert!(open_regular_file(&via).is_ok());
        assert!(matches!(admit(&junc), Err(IoError::DevicePathRejected(_))));
        let _ = std::fs::remove_dir(&junc);
    }

    /// Los enlaces simbólicos de fichero requieren privilegio (o modo desarrollador). Si se pueden
    /// crear, se verifica el rechazo de un enlace a un dispositivo sin abrirlo.
    #[cfg(windows)]
    #[test]
    fn symlink_to_device_is_rejected_when_symlinks_are_available() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("trampa.img");
        match std::os::windows::fs::symlink_file(r"\\.\PhysicalDrive0", &link) {
            Ok(()) => assert!(matches!(admit(&link), Err(IoError::DevicePathRejected(_)))),
            Err(e) => eprintln!(
                "AVISO: no se pueden crear enlaces simbólicos ({e}); se cubre con pruebas léxicas"
            ),
        }
    }
}
