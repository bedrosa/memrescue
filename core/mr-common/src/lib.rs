//! Tipos compartidos por todos los crates del motor de MemRescue.

pub mod confidence;
pub mod hash;
pub mod range;

pub use confidence::Confidence;
pub use hash::{HashAlgorithm, MultiHasher};
pub use range::{ByteRange, Lba};

/// Versión del motor (la del workspace).
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");
