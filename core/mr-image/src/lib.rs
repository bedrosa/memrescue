//! Contenedores de imagen de disco como dispositivos de bloques virtuales de **solo lectura**.
//!
//! Contrato común (todas las implementaciones):
//! - Constructor `open(file: std::fs::File) -> mr_io::Result<Self>` que recibe un fichero **ya admitido**
//!   por `mr_io::path_policy` y abierto en solo lectura. Nunca abre otros ficheros por su cuenta.
//! - Implementa `mr_io::BlockDevice` (`size` = tamaño virtual, `sector_size` = sector lógico del formato,
//!   `read_at`, y `allocation` con las regiones dispersas como `Allocation::Sparse`).
//! - `fn describe(&self) -> String`: variante legible (p. ej. «VHD dinámico, bloques de 2 MiB»).
//! - Validación estricta de cabeceras, sumas de control, desplazamientos y tamaños contra el tamaño real
//!   del fichero; memoria acotada (tablas ≤ 256 MiB); errores `IoError::Corrupt`,
//!   `IoError::UnsupportedFeature` o `IoError::ExternalReference` (padres, extensiones externas), nunca pánicos.
//! - Modo estricto por defecto; modo tolerante solo por petición explícita
//!   ([`OpenOptions::tolerant`], ADR-020): las entradas de asignación inválidas o ambiguas descartan
//!   solo su bloque o grano, que se lee con error (`IoError::Read`) y se informa como `Data` en
//!   `allocation`, para que el lector tolerante lo marque `Unreadable` con el patrón de relleno.
//! - Basado en especificaciones públicas; probado contra `qemu-img` como referencia independiente.

pub mod crc32c;
pub mod open;
pub mod split;
mod tolerant;
pub mod vhd;
pub mod vhdx;
pub mod vmdk;

pub use open::{open_image, open_split, OpenOptions, OpenedImage, Reblocked};

/// Límite de memoria para tablas de asignación (BAT, directorios de grano…).
pub const MAX_TABLE_BYTES: u64 = 256 * 1024 * 1024;
