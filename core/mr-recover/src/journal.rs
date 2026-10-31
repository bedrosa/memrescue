//! Diario de la recuperación: JSON Lines UTF-8, una línea por acontecimiento, escrita entera de
//! una vez y vaciada al momento (si la ejecución se corta, todo lo escrito es válido). Formato
//! estable y versionado en `docs/formatos/diario-recuperacion.md` ([`JOURNAL_VERSION`]); los
//! informes TXT/CSV/HTML (#27) se derivan de él.
//!
//! El fichero se crea en exclusiva (nunca se sustituye uno existente) y nunca en el origen.

use std::io::Write;
use std::path::{Path, PathBuf};

/// Versión del esquema del diario.
pub const JOURNAL_VERSION: u64 = 1;
/// Carpeta del diario dentro del destino por defecto.
pub const JOURNAL_FOLDER: &str = "MemRescue-registro";

/// Diario abierto.
pub struct Journal {
    file: std::fs::File,
    path: PathBuf,
    lines: u64,
}

impl Journal {
    /// Crea el diario en `path` (en exclusiva; el directorio padre se crea si falta).
    pub fn create(path: &Path) -> std::io::Result<Journal> {
        if let Some(p) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(p)?;
        }
        let file = std::fs::OpenOptions::new()
            .append(true)
            .create_new(true)
            .open(path)?;
        Ok(Journal {
            file,
            path: path.to_path_buf(),
            lines: 0,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn lines(&self) -> u64 {
        self.lines
    }

    /// Añade una línea (un objeto JSON sin saltos de línea) y la vacía al sistema.
    pub fn line(&mut self, json: &str) -> std::io::Result<()> {
        debug_assert!(!json.contains('\n'));
        let mut b = Vec::with_capacity(json.len() + 1);
        b.extend_from_slice(json.as_bytes());
        b.push(b'\n');
        self.file.write_all(&b)?;
        self.file.flush()?;
        self.lines += 1;
        Ok(())
    }

    /// Sincroniza con el disco (al final y en la cancelación).
    pub fn sync(&mut self) -> std::io::Result<()> {
        self.file.sync_all()
    }
}
