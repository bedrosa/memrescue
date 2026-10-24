//! Piezas de VMDK **streamOptimized** (especificación pública de VMware «Virtual Disk Format 1.1»,
//! apartado «Stream-Optimized Compressed Sparse Extents»): marcadores, pie, granos comprimidos y
//! su caché. La apertura y la validación de las tablas viven en [`crate::vmdk`].
//!
//! Estructura de un fichero streamOptimized (todo alineado a sectores de 512 bytes):
//! - Cabecera `KDMV` (indicadores 16 «comprimido» y 17 «marcadores», `compressAlgorithm` = 1) y
//!   descriptor incrustado, como en monolithicSparse.
//! - Granos: marcador de grano `{lba: u64, size: u32}` seguido de `size` bytes de datos deflate en
//!   formato zlib (RFC 1950, con Adler-32 de los datos descomprimidos) y relleno hasta el sector.
//!   La entrada de GT apunta al sector del marcador.
//! - Metadatos con marcador propio `{numSectors: u64, size: u32 = 0, type: u32}` en un sector:
//!   GT (tipo 1), GD (tipo 2), pie (tipo 3); el fin de flujo (tipo 0) es un sector a cero.
//! - Si la cabecera trae `gdOffset` = `0xFFFF…FFFF` (GD al final), el GD real está en el **pie**:
//!   copia de la cabecera en el penúltimo sector, precedida de su marcador y seguida del fin de
//!   flujo (los últimos 1536 bytes del fichero).

use mr_io::{IoError, Result};
use std::fs::File;
use std::io;
use std::sync::{Arc, Mutex};

/// `compressAlgorithm` = deflate (único definido por la especificación).
pub(crate) const COMPRESSION_DEFLATE: u16 = 1;
pub(crate) const MARKER_EOS: u32 = 0;
pub(crate) const MARKER_FOOTER: u32 = 3;
/// Bytes del marcador de grano que preceden a los datos comprimidos: LBA (u64) + tamaño (u32).
pub(crate) const GRAIN_MARKER_LEN: u64 = 12;
/// Marcador del pie, pie y marcador de fin de flujo: un sector cada uno, al final del fichero.
pub(crate) const FOOTER_TAIL_BYTES: u64 = 1536;
/// Grano máximo admitido en streamOptimized (16 MiB; VMware y qemu usan 64 KiB): acota los
/// búferes de lectura y de descompresión de cada grano.
pub(crate) const MAX_STREAM_GRAIN_SECTORS: u64 = 1 << 15;
/// Memoria máxima de la caché de granos descomprimidos (como mínimo cabe un grano).
const CACHE_BYTES: u64 = 4 * 1024 * 1024;
/// Ventana de lectura de los marcadores de grano al abrir.
const MARKER_WINDOW: usize = 4096;

/// Tamaño comprimido máximo aceptado para un grano (como qemu: el doble del grano). El peor
/// caso real de deflate (bloques almacenados) apenas supera el tamaño del grano.
pub(crate) fn max_compressed(grain_bytes: u64) -> u64 {
    grain_bytes.saturating_mul(2)
}

fn le_u32(b: &[u8], at: usize) -> u32 {
    let mut a = [0u8; 4];
    a.copy_from_slice(&b[at..at + 4]);
    u32::from_le_bytes(a)
}

fn le_u64(b: &[u8], at: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(a)
}

/// Marcador de metadatos (`val`, `size`, `type`): `b` mide al menos 16 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Marker {
    pub val: u64,
    pub size: u32,
    pub kind: u32,
}

impl Marker {
    pub(crate) fn parse(b: &[u8]) -> Self {
        Self {
            val: le_u64(b, 0),
            size: le_u32(b, 8),
            kind: le_u32(b, 12),
        }
    }
}

/// `true` si `b` (≥ 16 bytes) es un marcador de fin de flujo.
pub(crate) fn is_eos(b: &[u8]) -> bool {
    Marker::parse(b)
        == Marker {
            val: 0,
            size: 0,
            kind: MARKER_EOS,
        }
}

/// Comprueba los marcadores que rodean al pie en los últimos [`FOOTER_TAIL_BYTES`] del fichero:
/// marcador de pie (`size` = 0, tipo 3) y fin de flujo. El pie son los 512 bytes centrales.
pub(crate) fn check_footer_markers(tail: &[u8]) -> std::result::Result<(), String> {
    let m = Marker::parse(&tail[..16]);
    if m.size != 0 || m.kind != MARKER_FOOTER {
        return Err(format!(
            "marcador del pie no válido (tamaño {}, tipo {}; se esperaba tamaño 0, tipo {MARKER_FOOTER})",
            m.size, m.kind
        ));
    }
    if !is_eos(&tail[1024..1040]) {
        return Err("falta el marcador de fin de flujo tras el pie".into());
    }
    Ok(())
}

/// Marcador de grano: `(lba, tamaño comprimido)`.
pub(crate) fn grain_marker(b: &[u8; GRAIN_MARKER_LEN as usize]) -> (u64, u32) {
    (le_u64(b, 0), le_u32(b, 8))
}

/// Validación de un marcador de grano frente a la GT que lo referencia: su LBA debe ser el primer
/// sector del grano `g` y su tamaño comprimido, no nulo, acotado y dentro del fichero.
pub(crate) fn check_grain_marker(
    g: u64,
    lba: u64,
    size: u32,
    grain_sectors: u64,
    offset: u64,
    file_len: u64,
) -> std::result::Result<(), String> {
    let expected = g.saturating_mul(grain_sectors);
    if lba != expected {
        return Err(format!(
            "el marcador del grano {g} (byte {offset}) declara el LBA {lba}; la GT lo sitúa en el {expected}"
        ));
    }
    let max = max_compressed(grain_sectors * 512);
    if size == 0 || u64::from(size) > max {
        return Err(format!(
            "el grano {g} declara {size} bytes comprimidos (entre 1 y {max})"
        ));
    }
    let end = offset
        .checked_add(GRAIN_MARKER_LEN + u64::from(size))
        .filter(|&e| e <= file_len);
    if end.is_none() {
        return Err(format!(
            "el grano {g} comprimido ({size} bytes desde el byte {offset}) sale del fichero ({file_len} bytes)"
        ));
    }
    Ok(())
}

/// Error de lectura de un grano comprimido dañado, con el rango **virtual** pedido (como los
/// granos descartados del modo tolerante): el lector resiliente lo marca `Unreadable`.
pub(crate) fn damaged_read(g: u64, offset: u64, len: usize, why: &str) -> IoError {
    IoError::Read {
        offset,
        len,
        source: io::Error::new(
            io::ErrorKind::InvalidData,
            format!("VMDK: grano comprimido {g} dañado: {why}"),
        ),
    }
}

/// Descomprime un grano (zlib con Adler-32) y comprueba su tamaño: el grano entero o, en el último
/// grano, la parte que cubre el disco (`tail`). La salida nunca pasa de `grain_bytes` + 1 bytes.
pub(crate) fn inflate_grain(
    compressed: &[u8],
    grain_bytes: usize,
    tail: usize,
) -> std::result::Result<Vec<u8>, String> {
    let out = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(compressed, grain_bytes + 1)
        .map_err(|e| format!("flujo zlib no válido ({:?})", e.status))?;
    if out.len() != grain_bytes && out.len() != tail {
        return Err(if tail == grain_bytes {
            format!(
                "se descomprime a {} bytes (se esperaban {grain_bytes})",
                out.len()
            )
        } else {
            format!(
                "se descomprime a {} bytes (se esperaban {grain_bytes} o {tail})",
                out.len()
            )
        });
    }
    Ok(out)
}

fn read_exact_at(file: &File, offset: u64, buf: &mut [u8]) -> Result<()> {
    mr_io::read_exact_at(file, offset, buf).map_err(|source| IoError::Read {
        offset,
        len: buf.len(),
        source,
    })
}

/// Lector de marcadores de grano al abrir: los granos suelen estar seguidos en el fichero, así que
/// una ventana pequeña evita una lectura por grano cuando se comprimen mucho.
pub(crate) struct MarkerReader {
    buf: Vec<u8>,
    start: u64,
    len: usize,
}

impl MarkerReader {
    pub(crate) fn new() -> Self {
        Self {
            buf: vec![0u8; MARKER_WINDOW],
            start: 0,
            len: 0,
        }
    }

    /// Los 12 bytes del marcador en `offset`; quien llama comprueba antes que caben en el fichero.
    pub(crate) fn read(
        &mut self,
        file: &File,
        offset: u64,
        file_len: u64,
    ) -> Result<[u8; GRAIN_MARKER_LEN as usize]> {
        let need = GRAIN_MARKER_LEN;
        let cached = offset >= self.start && offset + need <= self.start + self.len as u64;
        if !cached {
            let len = (file_len - offset).min(MARKER_WINDOW as u64) as usize;
            read_exact_at(file, offset, &mut self.buf[..len])?;
            self.start = offset;
            self.len = len;
        }
        let i = (offset - self.start) as usize;
        let mut m = [0u8; GRAIN_MARKER_LEN as usize];
        m.copy_from_slice(&self.buf[i..i + need as usize]);
        Ok(m)
    }
}

/// Caché LRU acotada de granos descompresos (como mucho [`CACHE_BYTES`], al menos un grano).
#[derive(Debug)]
pub(crate) struct GrainCache {
    slots: usize,
    state: Mutex<CacheState>,
}

#[derive(Debug, Default)]
struct CacheState {
    tick: u64,
    /// `(grano, último uso, datos)`.
    entries: Vec<(u64, u64, Arc<Vec<u8>>)>,
}

impl GrainCache {
    pub(crate) fn new(grain_bytes: u64) -> Self {
        Self {
            slots: (CACHE_BYTES / grain_bytes.max(1)).max(1) as usize,
            state: Mutex::new(CacheState::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CacheState> {
        // Un pánico con el cerrojo tomado no deja la caché incoherente: cada operación es atómica.
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn get(&self, g: u64) -> Option<Arc<Vec<u8>>> {
        let mut s = self.lock();
        s.tick += 1;
        let tick = s.tick;
        s.entries.iter_mut().find(|e| e.0 == g).map(|e| {
            e.1 = tick;
            Arc::clone(&e.2)
        })
    }

    pub(crate) fn insert(&self, g: u64, data: Vec<u8>) -> Arc<Vec<u8>> {
        let data = Arc::new(data);
        let mut s = self.lock();
        s.tick += 1;
        let tick = s.tick;
        if let Some(e) = s.entries.iter_mut().find(|e| e.0 == g) {
            e.1 = tick;
            e.2 = Arc::clone(&data);
            return data;
        }
        if s.entries.len() >= self.slots {
            if let Some(oldest) = s
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| e.1)
                .map(|(i, _)| i)
            {
                s.entries.swap_remove(oldest);
            }
        }
        s.entries.push((g, tick, Arc::clone(&data)));
        data
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.lock().entries.len()
    }

    #[cfg(test)]
    pub(crate) fn slots(&self) -> usize {
        self.slots
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::vmdk::VmdkImage;
    use mr_io::{Allocation, BlockDevice};
    use std::io::Write;

    pub(crate) const GRAIN: u64 = 16; // sectores (8 KiB)
    /// Cambio sobre el generador de un caso de prueba.
    type Edit = Box<dyn Fn(&mut StreamBuilder)>;
    const SECTOR: usize = 512;
    const FLAGS: u32 = 1 | (1 << 16) | (1 << 17);

    fn descriptor(capacity: u64, create_type: &str) -> String {
        format!(
            "# Disk DescriptorFile\nversion=1\nCID=12345678\nparentCID=ffffffff\n\
             createType=\"{create_type}\"\n\n# Extent description\n\
             RW {capacity} SPARSE \"flujo.vmdk\"\n\n# The Disk Data Base\n#DDB\n\
             ddb.virtualHWVersion = \"4\"\n"
        )
    }

    /// Contenido virtual determinista (casi nunca cero) del byte `vo` en el grano de semilla `s`.
    fn pattern(vo: u64, s: u8) -> u8 {
        let x = vo.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (vo >> 7);
        ((x >> 56) as u8 ^ s) | 1
    }

    fn put(f: &mut Vec<u8>, at: usize, b: &[u8]) {
        if f.len() < at + b.len() {
            f.resize(at + b.len(), 0);
        }
        f[at..at + b.len()].copy_from_slice(b);
    }

    fn pad(f: &mut Vec<u8>) {
        f.resize(f.len().div_ceil(SECTOR) * SECTOR, 0);
    }

    fn metadata_marker(f: &mut Vec<u8>, sectors: u64, kind: u32) {
        let mut m = [0u8; SECTOR];
        m[..8].copy_from_slice(&sectors.to_le_bytes());
        m[12..16].copy_from_slice(&kind.to_le_bytes());
        f.extend_from_slice(&m);
    }

    /// Generador de imágenes streamOptimized sintéticas al estilo de VMware: cabecera, descriptor,
    /// granos con marcador, GT con marcador, GD con marcador, pie y fin de flujo.
    pub(crate) struct StreamBuilder {
        pub version: u32,
        pub flags: u32,
        pub algorithm: u16,
        pub capacity: u64,
        pub grain: u64,
        /// `true`: cabecera con `gdOffset` = GD_AT_END y GD en el pie; `false`: en la cabecera.
        pub gd_at_end: bool,
        pub descriptor: String,
        /// Granos con datos (índice, semilla del patrón), en el orden del fichero.
        pub grains: Vec<(u64, u8)>,
        /// Último grano descomprimido a su parte útil (`true`) o al grano entero.
        pub short_tail: bool,
        /// Sustituciones: LBA del marcador, tamaño declarado, datos comprimidos.
        pub marker_lba: Vec<(u64, u64)>,
        pub marker_size: Vec<(u64, u32)>,
        pub payload: Vec<(u64, Vec<u8>)>,
        /// Entradas de GT forzadas (grano, valor) y de GD (índice, valor).
        pub raw_gte: Vec<(u64, u32)>,
        pub raw_gde: Vec<(usize, u32)>,
        /// Ajustes del pie: función sobre los 1536 bytes finales.
        pub footer_edit: Option<fn(&mut [u8])>,
    }

    /// Ubicación de las estructuras de una imagen construida.
    #[derive(Debug, Clone)]
    pub(crate) struct Layout {
        /// (grano, desplazamiento del marcador, bytes comprimidos).
        pub grains: Vec<(u64, u64, u32)>,
        pub gd: u64,
        pub gts: Vec<u64>,
    }

    impl StreamBuilder {
        pub(crate) fn new(capacity: u64) -> Self {
            Self {
                version: 3,
                flags: FLAGS,
                algorithm: COMPRESSION_DEFLATE,
                capacity,
                grain: GRAIN,
                gd_at_end: true,
                descriptor: descriptor(capacity, "streamOptimized"),
                grains: Vec::new(),
                short_tail: true,
                marker_lba: Vec::new(),
                marker_size: Vec::new(),
                payload: Vec::new(),
                raw_gte: Vec::new(),
                raw_gde: Vec::new(),
                footer_edit: None,
            }
        }

        fn grain_bytes(&self) -> u64 {
            self.grain * 512
        }

        /// Contenido virtual del grano `g` (lo que se comprime).
        pub(crate) fn grain_content(&self, g: u64, seed: u8) -> Vec<u8> {
            let gb = self.grain_bytes();
            let size = self.capacity * 512;
            let len = if self.short_tail {
                gb.min(size - g * gb)
            } else {
                gb
            };
            (0..len)
                .map(|k| {
                    let vo = g * gb + k;
                    if vo < size {
                        pattern(vo, seed)
                    } else {
                        0
                    }
                })
                .collect()
        }

        /// Disco virtual esperado.
        pub(crate) fn expected(&self) -> Vec<u8> {
            let size = (self.capacity * 512) as usize;
            let mut v = vec![0u8; size];
            for &(g, seed) in &self.grains {
                let c = self.grain_content(g, seed);
                let at = (g * self.grain_bytes()) as usize;
                let n = c.len().min(size - at);
                v[at..at + n].copy_from_slice(&c[..n]);
            }
            v
        }

        fn header(&self, gd_offset: u64) -> [u8; SECTOR] {
            let mut h = [0u8; SECTOR];
            h[0..4].copy_from_slice(b"KDMV");
            h[4..8].copy_from_slice(&self.version.to_le_bytes());
            h[8..12].copy_from_slice(&self.flags.to_le_bytes());
            h[12..20].copy_from_slice(&self.capacity.to_le_bytes());
            h[20..28].copy_from_slice(&self.grain.to_le_bytes());
            h[28..36].copy_from_slice(&1u64.to_le_bytes());
            h[36..44].copy_from_slice(&2u64.to_le_bytes());
            h[44..48].copy_from_slice(&512u32.to_le_bytes());
            h[56..64].copy_from_slice(&gd_offset.to_le_bytes());
            h[64..72].copy_from_slice(&3u64.to_le_bytes());
            h[73..77].copy_from_slice(b"\n \r\n");
            h[77..79].copy_from_slice(&self.algorithm.to_le_bytes());
            h
        }

        pub(crate) fn build(&self) -> (Vec<u8>, Layout) {
            let grains = self.capacity.div_ceil(self.grain);
            let gd_entries = grains.div_ceil(512).max(1) as usize;
            let mut f = vec![0u8; 3 * SECTOR];
            let d = self.descriptor.as_bytes();
            put(&mut f, SECTOR, &d[..d.len().min(2 * SECTOR)]);
            let mut gt = vec![0u32; gd_entries * 512];
            let mut layout = Layout {
                grains: Vec::new(),
                gd: 0,
                gts: Vec::new(),
            };
            for &(g, seed) in &self.grains {
                let content = self.grain_content(g, seed);
                let mut z = miniz_oxide::deflate::compress_to_vec_zlib(&content, 6);
                if let Some((_, p)) = self.payload.iter().find(|p| p.0 == g) {
                    z = p.clone();
                }
                let at = f.len();
                gt[g as usize] = (at / SECTOR) as u32;
                let lba = self
                    .marker_lba
                    .iter()
                    .find(|m| m.0 == g)
                    .map_or(g * self.grain, |m| m.1);
                let size = self
                    .marker_size
                    .iter()
                    .find(|m| m.0 == g)
                    .map_or(z.len() as u32, |m| m.1);
                f.extend_from_slice(&lba.to_le_bytes());
                f.extend_from_slice(&size.to_le_bytes());
                f.extend_from_slice(&z);
                pad(&mut f);
                layout.grains.push((g, at as u64, z.len() as u32));
            }
            for &(g, v) in &self.raw_gte {
                gt[g as usize] = v;
            }
            let mut gd = vec![0u32; gd_entries];
            for (i, e) in gd.iter_mut().enumerate() {
                metadata_marker(&mut f, 4, 1);
                *e = (f.len() / SECTOR) as u32;
                layout.gts.push(f.len() as u64);
                for v in &gt[i * 512..(i + 1) * 512] {
                    f.extend_from_slice(&v.to_le_bytes());
                }
            }
            for &(i, v) in &self.raw_gde {
                gd[i] = v;
            }
            let gd_sectors = (gd_entries * 4).div_ceil(SECTOR) as u64;
            metadata_marker(&mut f, gd_sectors, 2);
            let gd_sector = (f.len() / SECTOR) as u64;
            layout.gd = f.len() as u64;
            for v in &gd {
                f.extend_from_slice(&v.to_le_bytes());
            }
            pad(&mut f);
            let head_gd = if self.gd_at_end { u64::MAX } else { gd_sector };
            f[..SECTOR].copy_from_slice(&self.header(head_gd));
            metadata_marker(&mut f, 1, MARKER_FOOTER);
            f.extend_from_slice(&self.header(gd_sector));
            f.extend_from_slice(&[0u8; SECTOR]);
            if let Some(edit) = self.footer_edit {
                let n = f.len();
                edit(&mut f[n - FOOTER_TAIL_BYTES as usize..]);
            }
            (f, layout)
        }
    }

    pub(crate) fn temp_with(bytes: &[u8]) -> tempfile::NamedTempFile {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(bytes).unwrap();
        tmp.flush().unwrap();
        tmp
    }

    pub(crate) fn open_mode(
        bytes: &[u8],
        tolerant: bool,
    ) -> (tempfile::NamedTempFile, Result<VmdkImage>) {
        let tmp = temp_with(bytes);
        let r = VmdkImage::open_with(File::open(tmp.path()).unwrap(), tolerant);
        (tmp, r)
    }

    fn is_corrupt<T: std::fmt::Debug>(r: &Result<T>) -> bool {
        matches!(r, Err(IoError::Corrupt { format: "VMDK", .. }))
    }

    fn is_unsupported<T: std::fmt::Debug>(r: &Result<T>) -> bool {
        matches!(r, Err(IoError::UnsupportedFeature { format: "VMDK", .. }))
    }

    /// 601 granos (2 GT), capacidad no múltiplo del grano y granos en orden no monótono.
    pub(crate) fn sample() -> StreamBuilder {
        let mut b = StreamBuilder::new(600 * GRAIN + 5);
        b.grains = vec![(4, 0x44), (0, 0x11), (1, 0x22), (513, 0x55), (600, 0x66)];
        b
    }

    #[test]
    fn markers_and_footer() {
        let mut m = [0u8; 16];
        m[..8].copy_from_slice(&7u64.to_le_bytes());
        m[8..12].copy_from_slice(&0u32.to_le_bytes());
        m[12..16].copy_from_slice(&2u32.to_le_bytes());
        assert_eq!(
            Marker::parse(&m),
            Marker {
                val: 7,
                size: 0,
                kind: 2
            }
        );
        assert!(!is_eos(&m));
        assert!(is_eos(&[0u8; 16]));
        let mut g = [0u8; 12];
        g[..8].copy_from_slice(&256u64.to_le_bytes());
        g[8..].copy_from_slice(&999u32.to_le_bytes());
        assert_eq!(grain_marker(&g), (256, 999));

        let (f, _) = sample().build();
        let tail = &f[f.len() - FOOTER_TAIL_BYTES as usize..];
        check_footer_markers(tail).unwrap();
        assert_eq!(&tail[512..516], b"KDMV");
        let mut bad = tail.to_vec();
        bad[12] = 2; // tipo GD
        assert!(check_footer_markers(&bad).unwrap_err().contains("tipo 2"));
        let mut bad = tail.to_vec();
        bad[8] = 1; // tamaño ≠ 0
        assert!(check_footer_markers(&bad).is_err());
        let mut bad = tail.to_vec();
        bad[1024] = 1; // fin de flujo alterado
        assert!(check_footer_markers(&bad)
            .unwrap_err()
            .contains("fin de flujo"));
    }

    #[test]
    fn grain_marker_bounds() {
        let ok = |lba, size, off, len| check_grain_marker(3, lba, size, 128, off, len);
        assert!(ok(384, 100, 0, 112).is_ok());
        assert!(ok(384, 100, 0, 111)
            .unwrap_err()
            .contains("sale del fichero"));
        assert!(ok(385, 100, 0, 1 << 20).unwrap_err().contains("LBA 385"));
        assert!(ok(384, 0, 0, 1 << 20).is_err());
        assert!(ok(384, 2 * 65536, 0, 1 << 20).is_ok());
        assert!(ok(384, 2 * 65536 + 1, 0, 1 << 20).is_err());
        assert!(ok(384, u32::MAX, u64::MAX - 5, u64::MAX).is_err());
        // Desbordamiento del LBA esperado: nunca coincide.
        assert!(check_grain_marker(u64::MAX, 0, 1, 128, 0, 100).is_err());
    }

    #[test]
    fn inflate_checks_size_and_checksum() {
        let data: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8).collect();
        let z = miniz_oxide::deflate::compress_to_vec_zlib(&data, 6);
        assert_eq!(inflate_grain(&z, 8192, 8192).unwrap(), data);
        // Tamaño distinto del grano y de la cola.
        assert!(inflate_grain(&z, 16384, 16384).is_err());
        assert!(inflate_grain(&z, 16384, 512).is_err());
        assert_eq!(inflate_grain(&z, 16384, 8192).unwrap(), data);
        // Más datos que el grano: se corta en el límite y es error.
        assert!(inflate_grain(&z, 4096, 4096).is_err());
        // Adler-32 alterado (los 4 últimos bytes): error aunque el deflate sea válido.
        let mut bad = z.clone();
        let n = bad.len();
        bad[n - 1] ^= 1;
        assert!(inflate_grain(&bad, 8192, 8192)
            .unwrap_err()
            .contains("Adler32"));
        // Truncado, vacío, basura y deflate sin cabecera zlib.
        assert!(inflate_grain(&z[..z.len() / 2], 8192, 8192).is_err());
        assert!(inflate_grain(&[], 8192, 8192).is_err());
        assert!(inflate_grain(&[0xff; 64], 8192, 8192).is_err());
        let raw = miniz_oxide::deflate::compress_to_vec(&data, 6);
        assert!(inflate_grain(&raw, 8192, 8192).is_err());
    }

    #[test]
    fn cache_is_bounded_lru() {
        let c = GrainCache::new(1024 * 1024);
        assert_eq!(c.slots(), 4);
        for g in 0..4 {
            c.insert(g, vec![g as u8]);
        }
        assert!(c.get(0).is_some()); // 0 pasa a ser el más reciente
        c.insert(9, vec![9]);
        assert_eq!(c.len(), 4);
        assert!(c.get(1).is_none(), "se expulsa el menos usado");
        assert_eq!(c.get(0).unwrap()[0], 0);
        c.insert(9, vec![10]);
        assert_eq!(c.get(9).unwrap()[0], 10);
        assert_eq!(c.len(), 4);
        // Granos enormes: al menos un hueco.
        assert_eq!(GrainCache::new(64 * 1024 * 1024).slots(), 1);
    }

    #[test]
    fn reads_stream_with_gd_in_footer_and_in_header() {
        for gd_at_end in [true, false] {
            for short_tail in [true, false] {
                let mut b = sample();
                b.gd_at_end = gd_at_end;
                b.short_tail = short_tail;
                let expect = b.expected();
                let (bytes, _) = b.build();
                let (t, img) = open_mode(&bytes, false);
                let before = crate::tolerant::check::Snapshot::of(t.path());
                let img = img.unwrap();
                let what = format!("GD al final {gd_at_end}, cola corta {short_tail}");
                assert_eq!(img.size(), (600 * GRAIN + 5) * 512, "{what}");
                let d = img.describe();
                assert!(
                    d.contains("streamOptimized") && d.contains("deflate"),
                    "{d}"
                );
                assert_eq!(d.contains("pie"), gd_at_end, "{d}");
                let mut all = vec![0xccu8; expect.len()];
                img.read_at(0, &mut all).unwrap();
                assert!(all == expect, "{what}");
                // Lecturas desalineadas que cruzan granos comprimidos y dispersos.
                let gb = (GRAIN * 512) as usize;
                for &(off, len) in &[
                    (gb - 3, 7),
                    (gb * 2 - 1, gb + 2),
                    (gb * 4 + 100, 10),
                    (gb * 513 - 10, 30),
                    (expect.len() - 5, 5),
                ] {
                    let mut buf = vec![0xeeu8; len];
                    img.read_at(off as u64, &mut buf).unwrap();
                    assert_eq!(&buf[..], &expect[off..off + len], "{what}");
                }
                let cap = 600 * GRAIN + 5;
                assert_eq!(
                    img.allocation(0, cap).unwrap(),
                    vec![
                        (0, 2 * GRAIN, Allocation::Data),
                        (2 * GRAIN, 2 * GRAIN, Allocation::Sparse),
                        (4 * GRAIN, GRAIN, Allocation::Data),
                        (5 * GRAIN, 508 * GRAIN, Allocation::Sparse),
                        (513 * GRAIN, GRAIN, Allocation::Data),
                        (514 * GRAIN, 86 * GRAIN, Allocation::Sparse),
                        (600 * GRAIN, 5, Allocation::Data),
                    ]
                );
                crate::tolerant::check::tolerant_read_matches(&img, &expect, &[], &what);
                drop(img);
                assert_eq!(crate::tolerant::check::Snapshot::of(t.path()), before);
            }
        }
    }

    #[test]
    fn unsupported_stream_variants() {
        let cases: Vec<(&str, Edit)> = vec![
            (
                "comprimido sin marcadores",
                Box::new(|b| b.flags &= !(1 << 17)),
            ),
            (
                "marcadores sin compresión",
                Box::new(|b| b.flags &= !(1 << 16)),
            ),
            ("algoritmo 2", Box::new(|b| b.algorithm = 2)),
            ("algoritmo 0 con indicadores", Box::new(|b| b.algorithm = 0)),
            (
                "GD al final sin compresión",
                Box::new(|b| {
                    b.flags = 1;
                    b.algorithm = 0;
                }),
            ),
            (
                "grano de 32 MiB",
                Box::new(|b| {
                    b.grain = 1 << 16;
                    b.grains = vec![(0, 1)];
                }),
            ),
        ];
        for (what, edit) in cases {
            let mut b = sample();
            edit(&mut b);
            let (bytes, _) = b.build();
            for tolerant in [false, true] {
                let (_t, r) = open_mode(&bytes, tolerant);
                assert!(is_unsupported(&r), "{what}: {r:?}");
            }
        }
    }

    #[test]
    fn footer_and_descriptor_are_critical_in_both_modes() {
        let cases: Vec<(&str, Edit)> = vec![
            (
                "firma del pie",
                Box::new(|b| b.footer_edit = Some(|t| t[512] = b'X')),
            ),
            (
                "tipo del marcador del pie",
                Box::new(|b| b.footer_edit = Some(|t| t[12] = 1)),
            ),
            (
                "fin de flujo",
                Box::new(|b| b.footer_edit = Some(|t| t[1024 + 12] = 3)),
            ),
            (
                "pie con GD al final",
                Box::new(|b| b.footer_edit = Some(|t| t[512 + 56..512 + 64].fill(0xff))),
            ),
            (
                "pie con GD a cero",
                Box::new(|b| b.footer_edit = Some(|t| t[512 + 56..512 + 64].fill(0))),
            ),
            (
                "pie con otra capacidad",
                Box::new(|b| b.footer_edit = Some(|t| t[512 + 12] ^= 1)),
            ),
            (
                "pie con otro grano",
                Box::new(|b| b.footer_edit = Some(|t| t[512 + 20] = 32)),
            ),
            (
                "pie con GD fuera del fichero",
                Box::new(|b| b.footer_edit = Some(|t| t[512 + 62] = 1)),
            ),
            (
                "createType monolithicSparse",
                Box::new(|b| b.descriptor = descriptor(600 * GRAIN + 5, "monolithicSparse")),
            ),
        ];
        for (what, edit) in cases {
            let mut b = sample();
            edit(&mut b);
            let (bytes, _) = b.build();
            for tolerant in [false, true] {
                let (t, r) = open_mode(&bytes, tolerant);
                assert!(is_corrupt(&r), "{what} (tolerante {tolerant}): {r:?}");
                assert!(std::fs::read(t.path()).unwrap() == bytes);
            }
        }
        // Fichero demasiado corto para el pie.
        let (bytes, _) = sample().build();
        let mut short = bytes[..3 * 512].to_vec();
        short[56..64].fill(0xff);
        assert!(is_corrupt(&open_mode(&short, false).1));
    }

    #[test]
    fn end_of_stream_is_optional_with_gd_in_header() {
        // qemu-img no siempre escribe el marcador de fin de flujo: con el GD en la cabecera no hace
        // falta para leer. Sin el pie y el fin de flujo, o con longitud no múltiplo del sector, se
        // abre en ambos modos sin descartes y se lee entero.
        let mut b = sample();
        b.gd_at_end = false;
        let expect = b.expected();
        let (bytes, _) = b.build();
        for cut in [bytes.len() - 1536, bytes.len() - 1] {
            for tolerant in [false, true] {
                let (_t, img) = open_mode(&bytes[..cut], tolerant);
                let img = img.unwrap();
                assert_eq!(img.discarded_blocks(), 0);
                crate::tolerant::check::tolerant_read_matches(
                    &img,
                    &expect,
                    &[],
                    "sin fin de flujo",
                );
            }
        }
    }

    #[test]
    fn invalid_markers_are_rejected_or_discarded() {
        let base = sample();
        let expect = base.expected();
        let (raw, layout) = base.build();
        let cap = 600 * GRAIN + 5;
        let at =
            |g: u64| -> u32 { (layout.grains.iter().find(|x| x.0 == g).unwrap().1 / 512) as u32 };
        let (g0, g1, gt1) = (at(0), at(1), (layout.gts[1] / 512) as u32);
        let len_of = |g: u64| layout.grains.iter().find(|x| x.0 == g).unwrap().2;
        let (len1, len600) = (len_of(1), len_of(600));
        let file_sectors = (raw.len() / 512) as u32;
        // (caso, cambio, tramos de sectores dañados, granos descartados).
        type Case = (&'static str, Edit, Vec<(u64, u64)>, u64);
        let cases: Vec<Case> = vec![
            (
                "LBA del marcador distinto",
                Box::new(|b| b.marker_lba = vec![(1, 0)]),
                vec![(GRAIN, GRAIN)],
                1,
            ),
            (
                "tamaño comprimido 0",
                Box::new(|b| b.marker_size = vec![(4, 0)]),
                vec![(4 * GRAIN, GRAIN)],
                1,
            ),
            (
                "tamaño comprimido enorme",
                Box::new(|b| b.marker_size = vec![(4, u32::MAX)]),
                vec![(4 * GRAIN, GRAIN)],
                1,
            ),
            (
                "grano comprimido fuera del fichero",
                Box::new(move |b| b.raw_gte = vec![(4, file_sectors + 10)]),
                vec![(4 * GRAIN, GRAIN)],
                1,
            ),
            (
                // Mismo marcador para dos granos: su LBA solo encaja con el grano 0.
                "grano duplicado",
                Box::new(move |b| b.raw_gte = vec![(1, g0)]),
                vec![(GRAIN, GRAIN)],
                1,
            ),
            (
                // El marcador del grano 1 declara bytes que invaden el marcador del grano 513.
                "grano que invade otro",
                Box::new(move |b| b.marker_size = vec![(1, len1 + 600)]),
                vec![(GRAIN, GRAIN), (513 * GRAIN, GRAIN)],
                2,
            ),
            (
                // El grano 600 (el último del fichero) invade el marcador y la GT 0: se descartan
                // el grano y la GT con sus 512 granos.
                "grano que invade una GT",
                Box::new(move |b| b.marker_size = vec![(600, len600 + 1024)]),
                vec![(0, 512 * GRAIN), (600 * GRAIN, 5)],
                513,
            ),
            (
                // La GT 1 apunta al grano 1: se solapan y se descartan los dos (la GT, entera).
                "GT sobre un grano",
                Box::new(move |b| b.raw_gde = vec![(1, g1)]),
                vec![(GRAIN, GRAIN), (512 * GRAIN, cap - 512 * GRAIN)],
                90,
            ),
            (
                // Una entrada de GT que apunta a otra GT: lo que hay allí no es un marcador válido.
                "grano sobre una GT",
                Box::new(move |b| b.raw_gte = vec![(4, gt1)]),
                vec![(4 * GRAIN, GRAIN)],
                1,
            ),
        ];
        for (what, edit, bad, n) in cases {
            let mut b = sample();
            edit(&mut b);
            let (bytes, _) = b.build();
            let (_t, strict) = open_mode(&bytes, false);
            assert!(is_corrupt(&strict), "{what}: {strict:?}");
            let (t, img) = open_mode(&bytes, true);
            let before = crate::tolerant::check::Snapshot::of(t.path());
            let img = img.unwrap_or_else(|e| panic!("{what}: {e}"));
            assert_eq!(img.discarded_blocks(), n, "{what}");
            crate::tolerant::check::tolerant_read_matches(&img, &expect, &bad, what);
            drop(img);
            assert_eq!(
                crate::tolerant::check::Snapshot::of(t.path()),
                before,
                "{what}"
            );
        }
    }

    #[test]
    fn damaged_compressed_data_is_unreadable_never_wrong() {
        let base = sample();
        let expect = base.expected();
        let gb = (GRAIN * 512) as usize;
        let z4 = miniz_oxide::deflate::compress_to_vec_zlib(&base.grain_content(4, 0x44), 6);
        let other = miniz_oxide::deflate::compress_to_vec_zlib(&vec![7u8; gb], 6);
        let half = miniz_oxide::deflate::compress_to_vec_zlib(&vec![7u8; gb / 2], 6);
        let mut flipped = z4.clone();
        flipped[z4.len() / 2] ^= 0x10;
        let mut adler = z4.clone();
        let n = adler.len();
        adler[n - 2] ^= 0x01;
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("byte alterado en medio", flipped),
            ("Adler-32 alterado", adler),
            ("flujo truncado", z4[..z4.len() - 10].to_vec()),
            ("medio grano", half),
            ("basura", vec![0x5a; 300]),
        ];
        for (what, payload) in cases {
            let mut b = sample();
            b.payload = vec![(4, payload)];
            let (bytes, _) = b.build();
            for tolerant in [false, true] {
                // Los marcadores son válidos: se abre en ambos modos y el grano 4 es ilegible.
                let (t, img) = open_mode(&bytes, tolerant);
                let before = crate::tolerant::check::Snapshot::of(t.path());
                let img = img.unwrap_or_else(|e| panic!("{what}: {e}"));
                assert_eq!(img.discarded_blocks(), 0);
                let mut buf = vec![0u8; 512];
                match img.read_at(4 * gb as u64 + 512, &mut buf) {
                    Err(IoError::Read {
                        offset,
                        len,
                        source,
                    }) => {
                        assert_eq!((offset, len), (4 * gb as u64 + 512, 512));
                        assert!(
                            source.to_string().contains("grano comprimido 4"),
                            "{source}"
                        );
                    }
                    r => panic!("{what}: {r:?}"),
                }
                crate::tolerant::check::tolerant_read_matches(
                    &img,
                    &expect,
                    &[(4 * GRAIN, GRAIN)],
                    what,
                );
                drop(img);
                assert_eq!(crate::tolerant::check::Snapshot::of(t.path()), before);
            }
        }
        // Un flujo zlib válido con otro contenido del tamaño correcto no se puede detectar: VMDK no
        // guarda ninguna suma de los datos aparte del Adler-32 del propio flujo.
        let mut b = sample();
        b.payload = vec![(4, other)];
        let (bytes, _) = b.build();
        let (_t, img) = open_mode(&bytes, false);
        let mut buf = vec![0u8; gb];
        img.unwrap().read_at(4 * gb as u64, &mut buf).unwrap();
        assert!(buf.iter().all(|&x| x == 7));
    }

    #[test]
    fn shrunk_after_open_gives_errors_and_cache_keeps_verified_data() {
        let mut b = sample();
        b.gd_at_end = false;
        let expect = b.expected();
        let (bytes, layout) = b.build();
        let gb = GRAIN * 512;
        let tmp = temp_with(&bytes);
        let img = VmdkImage::open(File::open(tmp.path()).unwrap()).unwrap();
        // El grano 4 (el primero del fichero) se lee y queda en la caché.
        let mut first = vec![0u8; gb as usize];
        img.read_at(4 * gb, &mut first).unwrap();
        // Se corta a mitad del grano 1.
        let (_, off1, len1) = *layout.grains.iter().find(|x| x.0 == 1).unwrap();
        let cut = off1 + 12 + u64::from(len1) / 2;
        std::fs::OpenOptions::new()
            .write(true)
            .open(tmp.path())
            .unwrap()
            .set_len(cut)
            .unwrap();
        for &(g, off, len) in &layout.grains {
            let mut buf = vec![0u8; 512];
            let r = img.read_at(g * gb, &mut buf);
            if g != 4 && off + 12 + u64::from(len) > cut {
                assert!(r.is_err(), "grano {g}");
            } else {
                r.unwrap();
                let v = (g * gb) as usize;
                assert_eq!(&buf[..], &expect[v..v + 512], "grano {g}");
            }
        }
    }

    #[test]
    fn many_grains_keep_cache_bounded() {
        // 1100 granos de 8 KiB con datos: la caché no pasa de 4 MiB (512 granos).
        let mut b = StreamBuilder::new(1100 * GRAIN);
        b.grains = (0..1100).map(|g| (g, (g % 200) as u8)).collect();
        let expect = b.expected();
        let (bytes, _) = b.build();
        let (_t, img) = open_mode(&bytes, false);
        let img = img.unwrap();
        let mut all = vec![0u8; expect.len()];
        img.read_at(0, &mut all).unwrap();
        assert!(all == expect);
        let stream_cache = img.stream_cache().unwrap();
        assert_eq!(stream_cache.slots(), 512);
        assert_eq!(stream_cache.len(), 512);
    }
}
