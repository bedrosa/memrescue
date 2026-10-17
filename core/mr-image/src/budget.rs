//! Presupuesto de memoria de trabajo de la apertura de un contenedor (ADR-020, addendum de la
//! incidencia 38).
//!
//! Cada tabla de asignación (BAT, directorio y tablas de granos) está acotada por
//! [`crate::MAX_TABLE_BYTES`]. Además, toda la memoria proporcional a las tablas que reserva una
//! apertura (tabla decodificada, lista de candidatos de la detección de solapes, registro de
//! daños…) se pide a un [`Budget`], que:
//! - no deja pasar de [`crate::open_memory_budget`] de los bytes de tablas leídos del fichero;
//! - reserva con `try_reserve_exact`, de modo que un fallo de asignación se informa como
//!   `IoError::UnsupportedFeature` en lugar de abortar el proceso.
//!
//! Las reservas pequeñas y de tamaño fijo (búfer de lectura por trozos, cabeceras, descriptor)
//! no pasan por el presupuesto.

use mr_io::{IoError, Result};

/// Trozo de lectura de las tablas: nunca se guarda la tabla en bruto entera.
pub(crate) const TABLE_CHUNK: usize = 1024 * 1024;

/// Contabilidad de la memoria de trabajo de una apertura.
#[derive(Debug)]
pub(crate) struct Budget {
    format: &'static str,
    limit: u64,
    used: u64,
}

impl Budget {
    /// Presupuesto vacío: hay que declarar las tablas con [`Self::add_tables`].
    pub(crate) fn new(format: &'static str) -> Self {
        Self {
            format,
            limit: 0,
            used: 0,
        }
    }

    /// Amplía el límite con el presupuesto de `table_bytes` bytes de tablas leídas del fichero.
    pub(crate) fn add_tables(&mut self, table_bytes: u64) {
        self.limit = self
            .limit
            .saturating_add(crate::open_memory_budget(table_bytes));
    }

    fn unsupported(&self, detail: String) -> IoError {
        IoError::UnsupportedFeature {
            format: self.format,
            detail,
        }
    }

    /// Vector vacío con capacidad exacta para `n` elementos, cargado al presupuesto.
    pub(crate) fn vec<T>(&mut self, n: u64, what: &str) -> Result<Vec<T>> {
        let bytes = n.saturating_mul(std::mem::size_of::<T>() as u64);
        let used = self.used.saturating_add(bytes);
        if used > self.limit {
            return Err(self.unsupported(format!(
                "{what}: {bytes} bytes superan el presupuesto de memoria de la apertura ({} bytes, {} ya reservados)",
                self.limit, self.used
            )));
        }
        let count = usize::try_from(n).map_err(|_| {
            self.unsupported(format!(
                "{what}: {n} elementos no caben en memoria en esta plataforma"
            ))
        })?;
        let mut v = Vec::new();
        v.try_reserve_exact(count).map_err(|e| {
            self.unsupported(format!("no hay memoria para {what} ({bytes} bytes): {e}"))
        })?;
        self.used = used;
        Ok(v)
    }

    /// Libera un vector obtenido con [`Self::vec`] y descuenta su reserva.
    pub(crate) fn release<T>(&mut self, v: Vec<T>) {
        let bytes = (v.capacity() as u64).saturating_mul(std::mem::size_of::<T>() as u64);
        self.used = self.used.saturating_sub(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reservations_are_bounded_and_released() {
        let mut b = Budget::new("PRUEBA");
        assert!(matches!(
            b.vec::<u8>(1, "nada"),
            Err(IoError::UnsupportedFeature {
                format: "PRUEBA",
                ..
            })
        ));
        b.add_tables(800);
        let limit = crate::open_memory_budget(800);
        assert_eq!(limit, 1700 + crate::OPEN_MEMORY_FIXED_BYTES);
        assert_eq!(b.limit, limit);
        let a: Vec<u32> = b.vec(200, "a").unwrap();
        assert_eq!(a.capacity(), 200);
        let c: Vec<u64> = b.vec(100, "c").unwrap();
        // Quedan `limit - 1600` bytes: uno más no cabe.
        match b.vec::<u8>(limit - 1600 + 1, "d") {
            Err(IoError::UnsupportedFeature { detail, .. }) => {
                assert!(detail.contains("presupuesto"), "{detail}")
            }
            r => panic!("{r:?}"),
        }
        b.release(a);
        b.release(c);
        assert_eq!(b.used, 0);
        assert!(b.vec::<u8>(limit, "e").is_ok());
    }

    #[test]
    fn allocation_failure_is_an_error_not_an_abort() {
        let mut b = Budget::new("PRUEBA");
        b.limit = u64::MAX;
        // Ningún sistema puede reservar casi todo el espacio de direcciones.
        let r = b.vec::<u8>(u64::MAX / 2, "tabla enorme");
        assert!(
            matches!(r, Err(IoError::UnsupportedFeature { .. })),
            "{r:?}"
        );
        assert_eq!(b.used, 0);
    }
}
