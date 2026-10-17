//! Cálculo incremental de hashes para informes de recuperación e integridad de imágenes.

use md5::Md5;
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HashAlgorithm {
    Crc32,
    Md5,
    Sha1,
    Sha256,
}

impl HashAlgorithm {
    pub const ALL: [HashAlgorithm; 4] = [
        HashAlgorithm::Crc32,
        HashAlgorithm::Md5,
        HashAlgorithm::Sha1,
        HashAlgorithm::Sha256,
    ];

    pub fn name(self) -> &'static str {
        match self {
            HashAlgorithm::Crc32 => "crc32",
            HashAlgorithm::Md5 => "md5",
            HashAlgorithm::Sha1 => "sha1",
            HashAlgorithm::Sha256 => "sha256",
        }
    }

    /// Identificador numérico estable usado en la interfaz C.
    pub fn code(self) -> u32 {
        match self {
            HashAlgorithm::Crc32 => 1,
            HashAlgorithm::Md5 => 2,
            HashAlgorithm::Sha1 => 3,
            HashAlgorithm::Sha256 => 4,
        }
    }

    pub fn from_code(code: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.code() == code)
    }
}

impl fmt::Display for HashAlgorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for HashAlgorithm {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|a| a.name().eq_ignore_ascii_case(s))
            .ok_or_else(|| format!("algoritmo de hash desconocido: {s}"))
    }
}

enum State {
    Crc32(crc32fast::Hasher),
    Md5(Md5),
    Sha1(Sha1),
    Sha256(Sha256),
}

/// Calcula varios hashes a la vez sobre el mismo flujo de datos.
pub struct MultiHasher {
    states: Vec<(HashAlgorithm, State)>,
}

impl MultiHasher {
    pub fn new(algorithms: &[HashAlgorithm]) -> Self {
        let mut states = Vec::new();
        for &a in algorithms {
            if states.iter().any(|(x, _)| *x == a) {
                continue;
            }
            let s = match a {
                HashAlgorithm::Crc32 => State::Crc32(crc32fast::Hasher::new()),
                HashAlgorithm::Md5 => State::Md5(Md5::new()),
                HashAlgorithm::Sha1 => State::Sha1(Sha1::new()),
                HashAlgorithm::Sha256 => State::Sha256(Sha256::new()),
            };
            states.push((a, s));
        }
        Self { states }
    }

    pub fn update(&mut self, data: &[u8]) {
        for (_, s) in &mut self.states {
            match s {
                State::Crc32(h) => h.update(data),
                State::Md5(h) => h.update(data),
                State::Sha1(h) => h.update(data),
                State::Sha256(h) => h.update(data),
            }
        }
    }

    /// Devuelve cada algoritmo con su resumen en hexadecimal en minúsculas.
    pub fn finish(self) -> Vec<(HashAlgorithm, String)> {
        self.states
            .into_iter()
            .map(|(a, s)| {
                let hex = match s {
                    State::Crc32(h) => format!("{:08x}", h.finalize()),
                    State::Md5(h) => hex::encode(h.finalize()),
                    State::Sha1(h) => hex::encode(h.finalize()),
                    State::Sha256(h) => hex::encode(h.finalize()),
                };
                (a, hex)
            })
            .collect()
    }
}

/// CRC-32 (IEEE 802.3, polinomio reflejado 0xEDB88320) de un búfer.
pub fn crc32(data: &[u8]) -> u32 {
    crc32fast::hash(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Vectores de prueba publicados para la entrada ASCII "abc".
    #[test]
    fn known_vectors_abc() {
        let mut h = MultiHasher::new(&HashAlgorithm::ALL);
        h.update(b"abc");
        let r = h.finish();
        let get = |a| r.iter().find(|(x, _)| *x == a).unwrap().1.clone();
        assert_eq!(get(HashAlgorithm::Crc32), "352441c2");
        assert_eq!(get(HashAlgorithm::Md5), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            get(HashAlgorithm::Sha1),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            get(HashAlgorithm::Sha256),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn incremental_equals_single_shot() {
        let data: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        let mut a = MultiHasher::new(&[HashAlgorithm::Sha256]);
        a.update(&data);
        let mut b = MultiHasher::new(&[HashAlgorithm::Sha256]);
        for chunk in data.chunks(777) {
            b.update(chunk);
        }
        assert_eq!(a.finish(), b.finish());
    }

    #[test]
    fn codes_round_trip() {
        for a in HashAlgorithm::ALL {
            assert_eq!(HashAlgorithm::from_code(a.code()), Some(a));
            assert_eq!(a.name().parse::<HashAlgorithm>().unwrap(), a);
        }
        assert!(HashAlgorithm::from_code(99).is_none());
    }
}
