//! Resultado de validación con nivel de confianza.
//!
//! Los validadores de estructuras no devuelven un booleano: distinguen entre una estructura
//! válida, una válida tras correcciones, una sospechosa y una inválida. Así el mismo parser sirve
//! para volúmenes sanos y para reconstrucción de volúmenes dañados.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confidence {
    Valid,
    ValidWithFixes(Vec<String>),
    Suspicious(Vec<String>),
    Invalid(String),
}

impl Confidence {
    /// `true` si la estructura se puede usar (válida, corregida o sospechosa).
    pub fn is_usable(&self) -> bool {
        !matches!(self, Confidence::Invalid(_))
    }

    /// Orden de calidad: mayor es mejor.
    pub fn rank(&self) -> u8 {
        match self {
            Confidence::Valid => 3,
            Confidence::ValidWithFixes(_) => 2,
            Confidence::Suspicious(_) => 1,
            Confidence::Invalid(_) => 0,
        }
    }
}

impl fmt::Display for Confidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Confidence::Valid => write!(f, "válida"),
            Confidence::ValidWithFixes(v) => write!(f, "válida con correcciones: {}", v.join("; ")),
            Confidence::Suspicious(v) => write!(f, "sospechosa: {}", v.join("; ")),
            Confidence::Invalid(m) => write!(f, "inválida: {m}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rank_orders_quality() {
        assert!(Confidence::Valid.rank() > Confidence::ValidWithFixes(vec![]).rank());
        assert!(Confidence::Suspicious(vec![]).rank() > Confidence::Invalid(String::new()).rank());
        assert!(!Confidence::Invalid("x".into()).is_usable());
        assert!(Confidence::Suspicious(vec!["y".into()]).is_usable());
    }
}
