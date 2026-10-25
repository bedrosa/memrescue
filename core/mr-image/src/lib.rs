//! Contenedores de imagen de disco como dispositivos de bloques virtuales de **solo lectura**.
//!
//! Contrato común (todas las implementaciones):
//! - Constructor `open(file: std::fs::File) -> mr_io::Result<Self>` que recibe un fichero **ya admitido**
//!   por `mr_io::path_policy` y abierto en solo lectura. Nunca abre otros ficheros por su cuenta.
//! - Implementa `mr_io::BlockDevice` (`size` = tamaño virtual, `sector_size` = sector lógico del formato,
//!   `read_at`, y `allocation` con las regiones dispersas como `Allocation::Sparse`).
//! - `fn describe(&self) -> String`: variante legible (p. ej. «VHD dinámico, bloques de 2 MiB»).
//! - Validación estricta de cabeceras, sumas de control, desplazamientos y tamaños contra el tamaño real
//!   del fichero; memoria acotada (tablas ≤ 256 MiB y memoria de trabajo de la apertura dentro de
//!   [`open_memory_budget`]); errores `IoError::Corrupt`,
//!   `IoError::UnsupportedFeature` o `IoError::ExternalReference` (padres, extensiones externas), nunca pánicos.
//! - Modo estricto por defecto; modo tolerante solo por petición explícita
//!   ([`OpenOptions::tolerant`], ADR-020): las entradas de asignación inválidas o ambiguas descartan
//!   solo su bloque o grano, que se lee con error (`IoError::Read`) y se informa como `Data` en
//!   `allocation`, para que el lector tolerante lo marque `Unreadable` con el patrón de relleno.
//! - Basado en especificaciones públicas; probado contra `qemu-img` como referencia independiente.

mod budget;
pub mod crc32c;
#[cfg(test)]
mod diff_builder;
pub mod differencing;
#[cfg(test)]
mod differencing_tests;
pub mod external;
pub mod open;
pub mod split;
mod tolerant;
pub mod vhd;
pub mod vhdx;
mod vhdx_log;
pub mod vmdk;
pub mod vmdk_extents;
mod vmdk_stream;

pub use differencing::parent_role;
pub use external::{ApprovedFiles, ExternalHint};
pub use open::{
    external_hints, list_vmdk_extents, open_image, open_image_with_parents, open_split,
    open_vmdk_extents, OpenOptions, OpenedImage, Reblocked,
};

/// Límite de memoria para tablas de asignación (BAT, directorios de grano…).
pub const MAX_TABLE_BYTES: u64 = 256 * 1024 * 1024;

/// Parte fija del presupuesto de cada tabla (redondeos del registro de daños, zonas fijas).
pub const OPEN_MEMORY_FIXED_BYTES: u64 = 64 * 1024;

/// Presupuesto de memoria de trabajo de una apertura (ADR-020, addendum) para `table_bytes` bytes
/// de tablas de asignación leídos del fichero: 17/8 (2,125 veces) esa cantidad más
/// [`OPEN_MEMORY_FIXED_BYTES`].
///
/// Cubre la tabla decodificada que conserva la imagen (como máximo 1 vez los bytes leídos), la
/// lista de entradas candidatas que se ordena para detectar solapes (como máximo 1 vez), el
/// registro de daños del modo tolerante (1 bit por entrada, como máximo 1/32) y estructuras
/// auxiliares menores. Las tablas en bruto se leen por trozos y nunca se guardan enteras. Con
/// [`MAX_TABLE_BYTES`] = 256 MiB por tabla, el presupuesto no pasa de unos 544 MiB en VHD y
/// VHDX ni de unos 1088 MiB en VMDK (directorio y tablas de granos al límite a la vez).
pub fn open_memory_budget(table_bytes: u64) -> u64 {
    (table_bytes.saturating_mul(17) / 8).saturating_add(OPEN_MEMORY_FIXED_BYTES)
}
