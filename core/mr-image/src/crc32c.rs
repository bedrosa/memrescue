//! CRC-32C (Castagnoli), necesario para validar cabeceras y tablas de regiones de VHDX.
//!
//! Implementación propia por tabla de 256 entradas, generada en tiempo de compilación, con el
//! polinomio reflejado `0x82F63B78` (forma normal `0x1EDC6F41`), valor inicial `0xFFFFFFFF` y
//! complemento final, tal como lo definen RFC 3720 (iSCSI) y [MS-VHDX].

/// Polinomio de Castagnoli en forma reflejada.
const POLINOMIO_REFLEJADO: u32 = 0x82F6_3B78;

/// Tabla de 256 entradas calculada en tiempo de compilación.
const TABLA: [u32; 256] = generar_tabla();

const fn generar_tabla() -> [u32; 256] {
    let mut tabla = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut bit = 0;
        while bit < 8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ POLINOMIO_REFLEJADO
            } else {
                c >> 1
            };
            bit += 1;
        }
        tabla[i] = c;
        i += 1;
    }
    tabla
}

/// Cálculo incremental de CRC-32C: `Crc32c::new()`, uno o más `update` y `finish`.
#[derive(Debug, Clone, Copy)]
pub struct Crc32c {
    estado: u32,
}

impl Default for Crc32c {
    fn default() -> Self {
        Self::new()
    }
}

impl Crc32c {
    pub const fn new() -> Self {
        Self {
            estado: 0xFFFF_FFFF,
        }
    }

    /// Añade `data` al cálculo.
    pub fn update(&mut self, data: &[u8]) {
        let mut c = self.estado;
        for &b in data {
            c = TABLA[((c ^ u32::from(b)) & 0xFF) as usize] ^ (c >> 8);
        }
        self.estado = c;
    }

    /// Valor final del CRC (no consume el estado: se puede seguir actualizando).
    pub fn finish(&self) -> u32 {
        !self.estado
    }
}

/// CRC-32C de `data` de una sola vez.
pub fn crc32c(data: &[u8]) -> u32 {
    let mut c = Crc32c::new();
    c.update(data);
    c.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vectores_publicos() {
        // Valor de comprobación del catálogo de CRC (CRC-32/ISCSI).
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
        // RFC 3720, apéndice B.4.
        assert_eq!(crc32c(&[0u8; 32]), 0x8A91_36AA);
        assert_eq!(crc32c(&[0xFFu8; 32]), 0x62A8_AB43);
        let creciente: Vec<u8> = (0u8..32).collect();
        assert_eq!(crc32c(&creciente), 0x46DD_794E);
        let decreciente: Vec<u8> = (0u8..32).rev().collect();
        assert_eq!(crc32c(&decreciente), 0x113F_DB5C);
        assert_eq!(crc32c(&[]), 0);
    }

    #[test]
    fn incremental_equivale_a_una_pasada() {
        let datos: Vec<u8> = (0..10_000u32).map(|i| (i * 131 % 251) as u8).collect();
        let total = crc32c(&datos);
        for corte in [0, 1, 7, 4096, 9_999, 10_000] {
            let mut c = Crc32c::new();
            c.update(&datos[..corte]);
            c.update(&datos[corte..]);
            assert_eq!(c.finish(), total);
        }
    }
}
