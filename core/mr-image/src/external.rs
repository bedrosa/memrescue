//! Ficheros externos aprobados explícitamente por el usuario (ADR-018).
//!
//! Algunos contenedores dependen de otros ficheros (extensiones de un VMDK descrito por un
//! descriptor separado, discos padre de un diferencial…). El motor **nunca** los busca ni los
//! abre por su cuenta: el llamador pasa una lista explícita de pares `(papel, ruta)` que el
//! usuario ha elegido o confirmado uno a uno. El «papel» es el nombre que el contenedor declara
//! (p. ej. el nombre de fichero de una línea de extensión del descriptor) y la ruta, el fichero
//! que el usuario asigna a ese nombre.
//!
//! Garantías de [`ApprovedFiles`]:
//! - Cada ruta pasa la política de admisión de `mr_io::path_policy` (fichero normal, nunca un
//!   dispositivo, un directorio ni una tubería) y se abre en **solo lectura** al construir la
//!   lista; se guarda su ruta canónica.
//! - Los papeles no pueden estar vacíos, repetirse ni contener caracteres de control; dos papeles
//!   no pueden apuntar al mismo fichero (ruta canónica).
//! - [`ApprovedFiles::get`] solo devuelve una entrada cuyo papel coincide **exactamente** (byte a
//!   byte) con el nombre declarado. No recorre directorios, no interpreta `..` ni rutas relativas
//!   o absolutas y no toca el sistema de archivos: un nombre declarado que no se aprobó no se
//!   encuentra nunca.
//!
//! Cada formato decide además qué nombres declarados acepta (el VMDK rechaza rutas absolutas,
//! UNC, de dispositivo o con separadores antes de consultar la lista).

use mr_io::path_policy::{admit, open_regular_file};
use mr_io::{IoError, Result};
use std::fs::File;
use std::path::{Path, PathBuf};

/// Número máximo de ficheros aprobados en una apertura.
pub const MAX_APPROVED_FILES: usize = 32_768;
/// Longitud máxima (bytes) de un papel.
pub const MAX_ROLE_BYTES: usize = 4096;

fn invalid(detail: String) -> IoError {
    IoError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        detail,
    ))
}

/// Un fichero aprobado: papel (nombre declarado), ruta canónica y fichero abierto en solo lectura.
#[derive(Debug)]
pub struct ApprovedFile {
    role: String,
    path: PathBuf,
    file: File,
}

impl ApprovedFile {
    /// Nombre declarado al que el usuario asignó este fichero.
    pub fn role(&self) -> &str {
        &self.role
    }

    /// Ruta canónica admitida.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Fichero abierto en solo lectura.
    pub fn file(&self) -> &File {
        &self.file
    }

    /// Duplica el descriptor del fichero (sigue siendo de solo lectura).
    pub fn try_clone_file(&self) -> Result<File> {
        Ok(self.file.try_clone()?)
    }
}

/// Lista ordenada de ficheros externos aprobados por el usuario.
#[derive(Debug, Default)]
pub struct ApprovedFiles {
    entries: Vec<ApprovedFile>,
}

impl ApprovedFiles {
    /// Lista vacía: ningún fichero externo autorizado.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Valida y abre (solo lectura) cada `(papel, ruta)`. Errores: papel vacío, repetido, con
    /// caracteres de control o demasiado largo, o dos papeles con el mismo fichero
    /// (`IoError::Io` con `InvalidInput`); ruta rechazada por la política
    /// (`DevicePathRejected`) o inexistente (`Io`).
    pub fn new<R, P>(list: impl IntoIterator<Item = (R, P)>) -> Result<Self>
    where
        R: Into<String>,
        P: AsRef<Path>,
    {
        let mut entries: Vec<ApprovedFile> = Vec::new();
        for (role, path) in list {
            let role: String = role.into();
            if entries.len() == MAX_APPROVED_FILES {
                return Err(invalid(format!(
                    "demasiados ficheros aprobados (máximo {MAX_APPROVED_FILES})"
                )));
            }
            if role.is_empty() {
                return Err(invalid("fichero aprobado sin nombre declarado".into()));
            }
            if role.len() > MAX_ROLE_BYTES || role.chars().any(char::is_control) {
                return Err(invalid(
                    "nombre declarado demasiado largo o con caracteres de control".into(),
                ));
            }
            if entries.iter().any(|e| e.role == role) {
                return Err(invalid(format!(
                    "el nombre declarado «{role}» se ha aprobado más de una vez"
                )));
            }
            let canon = admit(path.as_ref())?;
            if let Some(other) = entries.iter().find(|e| e.path == canon) {
                return Err(invalid(format!(
                    "«{role}» y «{}» apuntan al mismo fichero ({})",
                    other.role,
                    canon.display()
                )));
            }
            let file = open_regular_file(&canon)?;
            entries.push(ApprovedFile {
                role,
                path: canon,
                file,
            });
        }
        Ok(Self { entries })
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Entradas en el orden en que se aprobaron.
    pub fn iter(&self) -> impl Iterator<Item = &ApprovedFile> {
        self.entries.iter()
    }

    /// Entrada aprobada para el nombre declarado `declared` (coincidencia exacta). Nunca toca el
    /// sistema de archivos.
    pub fn get(&self, declared: &str) -> Option<&ApprovedFile> {
        self.entries.iter().find(|e| e.role == declared)
    }

    /// `true` si alguna entrada es el fichero de ruta canónica `canon`.
    pub fn contains_path(&self, canon: &Path) -> bool {
        self.entries.iter().any(|e| e.path == canon)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_invalid<T: std::fmt::Debug>(r: Result<T>) -> bool {
        matches!(r, Err(IoError::Io(e)) if e.kind() == std::io::ErrorKind::InvalidInput)
    }

    #[test]
    fn exact_lookup_only() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a-f001.vmdk");
        std::fs::write(&a, [1u8; 512]).unwrap();
        let ap = ApprovedFiles::new([("a-f001.vmdk", &a)]).unwrap();
        assert_eq!(ap.len(), 1);
        assert!(ap.get("a-f001.vmdk").is_some());
        for other in [
            "A-F001.vmdk",
            "./a-f001.vmdk",
            r".\a-f001.vmdk",
            "a-f001.vmdk ",
            "..\\a-f001.vmdk",
            "a-f002.vmdk",
            "",
        ] {
            assert!(ap.get(other).is_none(), "{other}");
        }
        let e = ap.get("a-f001.vmdk").unwrap();
        assert_eq!(e.role(), "a-f001.vmdk");
        assert!(e.path().is_absolute());
        assert!(e.try_clone_file().is_ok());
        assert!(ap.contains_path(e.path()));
        assert!(ApprovedFiles::empty().get("a-f001.vmdk").is_none());
    }

    #[test]
    fn rejects_bad_lists() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.vmdk");
        let b = dir.path().join("b.vmdk");
        std::fs::write(&a, [1u8; 512]).unwrap();
        std::fs::write(&b, [2u8; 512]).unwrap();
        assert!(is_invalid(ApprovedFiles::new([("", &a)])));
        assert!(is_invalid(ApprovedFiles::new([("x\ny", &a)])));
        assert!(is_invalid(ApprovedFiles::new([("x", &a), ("x", &b)])));
        assert!(is_invalid(ApprovedFiles::new([("x", &a), ("y", &a)])));
        let long = "x".repeat(MAX_ROLE_BYTES + 1);
        assert!(is_invalid(ApprovedFiles::new([(long, &a)])));
        assert!(matches!(
            ApprovedFiles::new([("x", Path::new(r"\\.\PhysicalDrive0"))]),
            Err(IoError::DevicePathRejected(_))
        ));
        assert!(matches!(
            ApprovedFiles::new([("x", dir.path())]),
            Err(IoError::DevicePathRejected(_))
        ));
        assert!(ApprovedFiles::new([("x", dir.path().join("no-existe.vmdk"))]).is_err());
        // Los ficheros no cambian.
        assert_eq!(std::fs::read(&a).unwrap(), vec![1u8; 512]);
    }
}
