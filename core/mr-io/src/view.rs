//! Vista de un rango de sectores de otro dispositivo como dispositivo independiente
//! (p. ej. una partición o un volumen dentro de una imagen).
//!
//! La vista es de **solo lectura** y nunca permite leer fuera de su rango aunque el dispositivo
//! interior sea mayor: todas las traducciones de desplazamiento usan aritmética comprobada. Las
//! vistas pueden anidarse (una vista sobre otra vista).

use crate::{check_bounds, AllocationRun, BlockDevice, IoError, Result};
use std::sync::Arc;

/// Rango `[start_lba, start_lba + sector_count)` de otro dispositivo, expuesto como dispositivo.
pub struct RangeView {
    inner: Arc<dyn BlockDevice>,
    start_lba: u64,
    sector_count: u64,
    /// Desplazamiento en bytes del primer sector dentro del interior.
    start_byte: u64,
    /// Tamaño en bytes de la vista (`sector_count × sector_size`).
    size: u64,
}

impl RangeView {
    /// Crea una vista de `sector_count` sectores desde `start_lba` del dispositivo `inner`.
    ///
    /// Falla con [`IoError::InvalidRange`] si el rango está vacío, desborda o no cabe en los
    /// sectores completos del interior (los bytes finales que no completan un sector no se exponen).
    pub fn new(inner: Arc<dyn BlockDevice>, start_lba: u64, sector_count: u64) -> Result<Self> {
        let total = inner.sector_count();
        let invalid = || IoError::InvalidRange {
            lba: start_lba,
            count: sector_count,
            total,
        };
        let end = start_lba.checked_add(sector_count).ok_or_else(invalid)?;
        if sector_count == 0 || end > total {
            return Err(invalid());
        }
        let ss = u64::from(inner.sector_size());
        // end ≤ total y total × ss ≤ size ≤ u64::MAX; aun así se comprueba por si el interior
        // declarase un tamaño incoherente.
        let start_byte = start_lba.checked_mul(ss).ok_or_else(invalid)?;
        let size = sector_count.checked_mul(ss).ok_or_else(invalid)?;
        start_byte.checked_add(size).ok_or_else(invalid)?;
        Ok(Self {
            inner,
            start_lba,
            sector_count,
            start_byte,
            size,
        })
    }

    /// Primer sector de la vista en el dispositivo interior.
    pub fn start_lba(&self) -> u64 {
        self.start_lba
    }

    /// Número de sectores de la vista.
    pub fn sector_count(&self) -> u64 {
        self.sector_count
    }

    /// Dispositivo interior.
    pub fn inner(&self) -> &Arc<dyn BlockDevice> {
        &self.inner
    }
}

impl BlockDevice for RangeView {
    fn size(&self) -> u64 {
        self.size
    }

    fn sector_size(&self) -> u32 {
        self.inner.sector_size()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        // Primero contra los límites de la vista: nunca se lee fuera aunque el interior siga.
        check_bounds(offset, buf.len(), self.size)?;
        let inner_off = self
            .start_byte
            .checked_add(offset)
            .ok_or(IoError::OutOfBounds {
                offset,
                len: buf.len(),
                size: self.size,
            })?;
        self.inner.read_at(inner_off, buf)
    }

    fn allocation(&self, lba: u64, count: u64) -> Result<Vec<AllocationRun>> {
        let total = self.sector_count;
        if count == 0 || lba.checked_add(count).is_none_or(|e| e > total) {
            return Err(IoError::InvalidRange { lba, count, total });
        }
        // lba + count ≤ sector_count y start_lba + sector_count no desborda (validado en `new`).
        let runs = self.inner.allocation(self.start_lba + lba, count)?;
        runs.into_iter()
            .map(|(l, c, a)| {
                let local = l
                    .checked_sub(self.start_lba)
                    .filter(|&x| x.checked_add(c).is_some_and(|e| e <= total))
                    .ok_or_else(|| IoError::Corrupt {
                        format: "vista de rango",
                        detail: format!(
                            "el dispositivo interior devolvió un tramo fuera de la vista \
                             (LBA {l}, {c} sectores)"
                        ),
                    })?;
                Ok((local, c, a))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Allocation, MemDevice};

    /// Dispositivo con el byte de cada posición = número de sector (mod 256).
    fn mem(sectors: usize, ss: u32) -> Arc<dyn BlockDevice> {
        Arc::new(
            MemDevice::new(
                (0..sectors * ss as usize)
                    .map(|i| (i / ss as usize) as u8)
                    .collect(),
                ss,
            )
            .unwrap(),
        )
    }

    #[test]
    fn exact_limits() {
        let d = mem(16, 512);
        let v = RangeView::new(d.clone(), 4, 8).unwrap();
        assert_eq!(v.size(), 8 * 512);
        assert_eq!(v.sector_size(), 512);
        assert_eq!(BlockDevice::sector_count(&v), 8);
        assert_eq!((v.start_lba(), RangeView::sector_count(&v)), (4, 8));
        assert!(Arc::ptr_eq(v.inner(), &d));
        let mut b = vec![0u8; 512];
        v.read_sectors(0, 1, &mut b).unwrap();
        assert!(b.iter().all(|&x| x == 4), "primer sector");
        v.read_sectors(7, 1, &mut b).unwrap();
        assert!(b.iter().all(|&x| x == 11), "último sector");
        // Uno más allá: el interior tiene el sector 12, pero la vista no lo expone.
        assert!(matches!(
            v.read_sectors(8, 1, &mut b),
            Err(IoError::InvalidRange { .. })
        ));
        assert!(matches!(
            v.read_at(8 * 512, &mut b),
            Err(IoError::OutOfBounds { .. })
        ));
        // Último byte exacto sí; uno más no.
        let mut one = [0u8; 1];
        v.read_at(8 * 512 - 1, &mut one).unwrap();
        assert_eq!(one[0], 11);
        assert!(v.read_at(8 * 512, &mut one).is_err());
        // Vista que acaba exactamente en el final del interior.
        assert!(RangeView::new(d.clone(), 0, 16).is_ok());
        assert!(RangeView::new(d.clone(), 15, 1).is_ok());
        assert!(matches!(
            RangeView::new(d.clone(), 15, 2),
            Err(IoError::InvalidRange { .. })
        ));
        assert!(matches!(
            RangeView::new(d.clone(), 16, 1),
            Err(IoError::InvalidRange { .. })
        ));
        assert!(matches!(
            RangeView::new(d, 3, 0),
            Err(IoError::InvalidRange { .. })
        ));
    }

    #[test]
    fn trailing_partial_sector_of_inner_is_not_exposed() {
        // 4 sectores completos + 100 bytes sueltos.
        let d: Arc<dyn BlockDevice> =
            Arc::new(MemDevice::new(vec![7u8; 4 * 512 + 100], 512).unwrap());
        assert!(RangeView::new(d.clone(), 0, 4).is_ok());
        assert!(RangeView::new(d, 0, 5).is_err());
    }

    #[test]
    fn read_crossing_end_fails_even_if_inner_has_data() {
        let d = mem(16, 512);
        let v = RangeView::new(d, 2, 4).unwrap();
        let mut b = vec![0u8; 1024];
        // Empieza en el último sector de la vista y sigue en el sector 6 del interior.
        assert!(matches!(
            v.read_at(3 * 512, &mut b),
            Err(IoError::OutOfBounds { .. })
        ));
        assert!(v.read_sectors(3, 2, &mut b).is_err());
        // El búfer no se ha tocado con datos de fuera del rango.
        assert!(b.iter().all(|&x| x == 0));
        // Lectura no alineada dentro de la vista.
        let mut c = [0u8; 4];
        v.read_at(510, &mut c).unwrap();
        assert_eq!(c, [2, 2, 3, 3]);
    }

    #[test]
    fn overflow_is_rejected() {
        let d = mem(8, 512);
        assert!(RangeView::new(d.clone(), u64::MAX, 1).is_err());
        assert!(RangeView::new(d.clone(), 1, u64::MAX).is_err());
        assert!(RangeView::new(d.clone(), u64::MAX, u64::MAX).is_err());
        let v = RangeView::new(d, 1, 4).unwrap();
        let mut b = [0u8; 16];
        assert!(matches!(
            v.read_at(u64::MAX, &mut b),
            Err(IoError::OutOfBounds { .. })
        ));
        assert!(v.read_at(u64::MAX - 8, &mut b).is_err());
        assert!(v.read_sectors(u64::MAX, 1, &mut b).is_err());
        assert!(v.allocation(u64::MAX, 2).is_err());
        assert!(v.allocation(1, u64::MAX).is_err());

        // Interior enorme: la vista cerca del final no debe desbordar al traducir.
        struct Huge;
        impl BlockDevice for Huge {
            fn size(&self) -> u64 {
                u64::MAX
            }
            fn sector_size(&self) -> u32 {
                4096
            }
            fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
                check_bounds(offset, buf.len(), self.size())?;
                buf.fill(0xAB);
                Ok(())
            }
        }
        let total = Huge.sector_count();
        let hv = RangeView::new(Arc::new(Huge), total - 2, 2).unwrap();
        let mut s = vec![0u8; 4096];
        hv.read_sectors(1, 1, &mut s).unwrap();
        assert!(s.iter().all(|&x| x == 0xAB));
        assert!(hv.read_sectors(2, 1, &mut s).is_err());
        assert!(hv.read_at(u64::MAX - 10, &mut s[..4]).is_err());
        assert!(RangeView::new(Arc::new(Huge), total - 1, 2).is_err());
    }

    #[test]
    fn nested_views() {
        let d = mem(32, 1024);
        let outer: Arc<dyn BlockDevice> = Arc::new(RangeView::new(d, 8, 16).unwrap());
        let inner = RangeView::new(outer.clone(), 4, 4).unwrap();
        assert_eq!(inner.size(), 4 * 1024);
        assert_eq!(inner.sector_size(), 1024);
        let mut b = vec![0u8; 4 * 1024];
        inner.read_sectors(0, 4, &mut b).unwrap();
        for (i, chunk) in b.chunks(1024).enumerate() {
            assert!(chunk.iter().all(|&x| x as usize == 12 + i));
        }
        assert!(inner.read_sectors(4, 1, &mut b[..1024]).is_err());
        // La vista anidada no puede exceder la exterior aunque el disco real sea mayor.
        assert!(RangeView::new(outer.clone(), 10, 7).is_err());
        assert!(RangeView::new(outer, 10, 6).is_ok());
    }

    /// Dispositivo de prueba con tramos dispersos: sectores [4, 8) y [12, 16) son `Sparse`.
    struct Patchy;
    impl BlockDevice for Patchy {
        fn size(&self) -> u64 {
            20 * 512
        }
        fn sector_size(&self) -> u32 {
            512
        }
        fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
            check_bounds(offset, buf.len(), self.size())?;
            buf.fill(0);
            Ok(())
        }
        fn allocation(&self, lba: u64, count: u64) -> Result<Vec<AllocationRun>> {
            let total = self.sector_count();
            if count == 0 || lba.checked_add(count).is_none_or(|e| e > total) {
                return Err(IoError::InvalidRange { lba, count, total });
            }
            let kind = |s: u64| {
                if (4..8).contains(&s) || (12..16).contains(&s) {
                    Allocation::Sparse
                } else {
                    Allocation::Data
                }
            };
            let mut runs: Vec<AllocationRun> = Vec::new();
            for s in lba..lba + count {
                match runs.last_mut() {
                    Some((_, c, a)) if *a == kind(s) => *c += 1,
                    _ => runs.push((s, 1, kind(s))),
                }
            }
            Ok(runs)
        }
    }

    #[test]
    fn allocation_is_shifted() {
        let v = RangeView::new(Arc::new(Patchy), 6, 10).unwrap();
        assert_eq!(
            v.allocation(0, 10).unwrap(),
            vec![
                (0, 2, Allocation::Sparse),
                (2, 4, Allocation::Data),
                (6, 4, Allocation::Sparse),
            ]
        );
        assert_eq!(
            v.allocation(3, 4).unwrap(),
            vec![(3, 3, Allocation::Data), (6, 1, Allocation::Sparse)]
        );
        assert!(matches!(
            v.allocation(5, 6),
            Err(IoError::InvalidRange { .. })
        ));
        assert!(v.allocation(0, 0).is_err());
        // Anidada: se desplaza dos veces.
        let outer: Arc<dyn BlockDevice> = Arc::new(v);
        let nested = RangeView::new(outer, 1, 6).unwrap();
        assert_eq!(
            nested.allocation(0, 6).unwrap(),
            vec![
                (0, 1, Allocation::Sparse),
                (1, 4, Allocation::Data),
                (5, 1, Allocation::Sparse),
            ]
        );
    }

    /// Interior defectuoso que devuelve tramos con LBA anteriores al pedido.
    struct Liar;
    impl BlockDevice for Liar {
        fn size(&self) -> u64 {
            16 * 512
        }
        fn sector_size(&self) -> u32 {
            512
        }
        fn read_at(&self, _: u64, _: &mut [u8]) -> Result<()> {
            Ok(())
        }
        fn allocation(&self, _: u64, count: u64) -> Result<Vec<AllocationRun>> {
            Ok(vec![(0, count, Allocation::Data)])
        }
    }

    #[test]
    fn incoherent_inner_allocation_is_reported() {
        let v = RangeView::new(Arc::new(Liar), 4, 4).unwrap();
        assert!(matches!(v.allocation(0, 4), Err(IoError::Corrupt { .. })));
    }

    #[test]
    fn allocation_of_plain_inner_is_data() {
        let v = RangeView::new(mem(8, 4096), 2, 3).unwrap();
        assert_eq!(v.allocation(0, 3).unwrap(), vec![(0, 3, Allocation::Data)]);
    }
}
