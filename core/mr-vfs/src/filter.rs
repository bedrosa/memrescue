//! Filtros y consultas: capa pura sobre un [`Vfs`] terminado (no lee el origen ni cambia el árbol).
//!
//! **Criterios** ([`Filter`]):
//!
//! - Categorías ([`Category::bit`]) y entradas borradas: valen para ficheros y directorios.
//! - Criterios de fichero: estado de los datos ([`DataState::bit`]), reglas de nombre
//!   ([`NameRules`]), tamaño y fecha (rangos cerrados con cada extremo opcional). Solo se aplican a
//!   los ficheros.
//!
//! **Visibilidad.** Un fichero es visible si cumple todos los criterios. Un directorio (o carpeta
//! sintética) es visible si tiene algún descendiente visible o si no hay criterios de fichero y él
//! cumple los de categoría y borrado (con `files_only`, solo por sus descendientes). Así, filtrar
//! por borrados o por `*.txt` muestra los directorios que llevan hasta ellos.
//!
//! **Reglas de nombre** (INV-01 §7.5): lista ordenada separada por `;`, gana la primera que
//! coincide; `!` delante la hace de exclusión; comodines `*` y `?`; sin distinguir mayúsculas; una
//! regla con `/` se compara con la ruta relativa al nodo de partida de la consulta (componentes
//! escapados) y las demás con el nombre visible. Sin reglas, todo pasa; si ninguna coincide, pasa
//! solo cuando todas las reglas son de exclusión (`!*.tmp` excluye los `.tmp` y deja el resto).

use crate::arena::Vfs;
use crate::{Category, DataState, NodeId, NodeKind, Result, VfsError};

/// Qué fecha usa el filtro de fechas.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TimeField {
    Created,
    #[default]
    Modified,
    /// Cambio del registro (MFT).
    Changed,
    Accessed,
}

impl TimeField {
    pub fn code(self) -> &'static str {
        match self {
            TimeField::Created => "creacion",
            TimeField::Modified => "modificacion",
            TimeField::Changed => "cambio",
            TimeField::Accessed => "acceso",
        }
    }
}

/// Regla de nombre.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Rule {
    include: bool,
    /// Patrón en minúsculas.
    pattern: Vec<char>,
    /// Se compara con la ruta relativa.
    path: bool,
}

/// Lista ordenada de reglas de nombre (ver el módulo).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NameRules {
    rules: Vec<Rule>,
}

/// Máximo de reglas y de caracteres por regla.
pub const MAX_RULES: usize = 256;
pub const MAX_RULE_CHARS: usize = 1024;

fn lower(s: &str) -> Vec<char> {
    s.chars().flat_map(char::to_lowercase).collect()
}

/// Comodines `*` (cualquier secuencia) y `?` (un carácter), con retroceso lineal.
fn glob(p: &[char], s: &[char]) -> bool {
    let (mut pi, mut si) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while si < s.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, si));
            pi += 1;
        } else if let Some((sp, ss)) = star {
            pi = sp + 1;
            si = ss + 1;
            star = Some((sp, ss + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

impl NameRules {
    /// Reglas de una lista `regla;regla;!exclusión` (los espacios alrededor de cada regla se
    /// ignoran; las vacías también).
    pub fn parse(s: &str) -> Result<Self> {
        let mut rules = Vec::new();
        for raw in s.split(';').map(str::trim).filter(|r| !r.is_empty()) {
            let (include, pat) = match raw.strip_prefix('!') {
                Some(p) => (false, p.trim()),
                None => (true, raw),
            };
            if pat.is_empty() {
                return Err(VfsError::InvalidFilter(format!("regla vacía: «{raw}»")));
            }
            if pat.chars().count() > MAX_RULE_CHARS {
                return Err(VfsError::InvalidFilter(format!(
                    "regla de más de {MAX_RULE_CHARS} caracteres"
                )));
            }
            let path = pat.contains('/');
            rules.push(Rule {
                include,
                pattern: lower(pat.trim_start_matches('/')),
                path,
            });
            if rules.len() > MAX_RULES {
                return Err(VfsError::InvalidFilter(format!(
                    "más de {MAX_RULES} reglas"
                )));
            }
        }
        Ok(NameRules { rules })
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// `true` si alguna regla compara rutas.
    pub fn uses_paths(&self) -> bool {
        self.rules.iter().any(|r| r.path)
    }

    /// ¿Pasa un nombre (y su ruta relativa, si alguna regla la usa)?
    pub fn matches(&self, name: &str, rel_path: &str) -> bool {
        if self.rules.is_empty() {
            return true;
        }
        let n = lower(name);
        let p = if self.uses_paths() {
            lower(rel_path)
        } else {
            Vec::new()
        };
        for r in &self.rules {
            let subject = if r.path { &p } else { &n };
            if glob(&r.pattern, subject) {
                return r.include;
            }
        }
        self.rules.iter().all(|r| !r.include)
    }
}

/// Criterios de una consulta (ver el módulo). `Filter::default()` lo deja pasar todo.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    /// Máscara de [`Category::bit`] (0 = todas).
    pub categories: u32,
    /// Máscara de [`DataState::bit`] (0 = todos). Solo ficheros.
    pub data_states: u32,
    /// Solo entradas borradas ([`crate::flags::DELETED`]).
    pub deleted_only: bool,
    /// Los directorios solo se muestran si llevan a un fichero visible.
    pub files_only: bool,
    pub names: NameRules,
    pub size_min: Option<u64>,
    pub size_max: Option<u64>,
    pub time_field: TimeField,
    /// FILETIME mínimo y máximo (incluidos).
    pub time_min: Option<u64>,
    pub time_max: Option<u64>,
}

impl Filter {
    /// Filtro de las categorías dadas.
    pub fn categories(cs: &[Category]) -> Self {
        Filter {
            categories: cs.iter().fold(0, |m, c| m | c.bit()),
            ..Filter::default()
        }
    }

    /// Hay criterios que solo se aplican a ficheros.
    pub fn has_file_criteria(&self) -> bool {
        self.data_states != 0
            || !self.names.is_empty()
            || self.size_min.is_some()
            || self.size_max.is_some()
            || self.time_min.is_some()
            || self.time_max.is_some()
    }

    /// Comprueba la coherencia de los rangos.
    pub fn validate(&self) -> Result<()> {
        if let (Some(a), Some(b)) = (self.size_min, self.size_max) {
            if a > b {
                return Err(VfsError::InvalidFilter(format!(
                    "tamaño mínimo {a} mayor que el máximo {b}"
                )));
            }
        }
        if let (Some(a), Some(b)) = (self.time_min, self.time_max) {
            if a > b {
                return Err(VfsError::InvalidFilter(
                    "fecha mínima posterior a la máxima".into(),
                ));
            }
        }
        if self.categories >> 7 != 0 || self.data_states >> 8 != 0 {
            return Err(VfsError::InvalidFilter(
                "máscara de categorías o de estados no válida".into(),
            ));
        }
        Ok(())
    }

    fn common(&self, vfs: &Vfs, id: NodeId) -> bool {
        let cat_ok = self.categories == 0
            || vfs
                .category(id)
                .is_some_and(|c| self.categories & c.bit() != 0);
        cat_ok && (!self.deleted_only || vfs.flags(id) & crate::flags::DELETED != 0)
    }

    /// ¿Cumple el nodo los criterios por sí mismo? (`rel_path`: ruta relativa al nodo de partida,
    /// solo necesaria si alguna regla de nombre la usa.)
    pub fn matches_self(&self, vfs: &Vfs, id: NodeId, rel_path: &str) -> bool {
        let Some(n) = vfs.node(id) else {
            return false;
        };
        if !self.common(vfs, id) {
            return false;
        }
        if n.kind != NodeKind::File {
            return !self.files_only && !self.has_file_criteria();
        }
        if self.data_states != 0 && !n.data.is_some_and(|d| self.data_states & d.bit() != 0) {
            return false;
        }
        if self.size_min.is_some_and(|m| n.size < m) || self.size_max.is_some_and(|m| n.size > m) {
            return false;
        }
        let t = match self.time_field {
            TimeField::Created => n.times.created,
            TimeField::Modified => n.times.modified,
            TimeField::Changed => n.times.changed,
            TimeField::Accessed => n.times.accessed,
        };
        if self.time_min.is_some_and(|m| t < m) || self.time_max.is_some_and(|m| t > m) {
            return false;
        }
        self.names.matches(&vfs.name(id), rel_path)
    }
}

/// Nodos visibles bajo `start` (sin incluirlo) en orden determinista: con `recursive`, todo el
/// subárbol en profundidad (cada directorio antes que su contenido); sin él, los hijos directos.
/// Memoria lineal en el tamaño del subárbol (un recorrido y un bit por nodo).
pub fn query(vfs: &Vfs, start: NodeId, recursive: bool, f: &Filter) -> Result<Vec<NodeId>> {
    f.validate()?;
    if !vfs.contains(start) {
        return Err(VfsError::NoSuchNode(start.0));
    }
    let order = vfs.preorder(start);
    let mut visible = vec![false; vfs.len()];
    let paths = f.names.uses_paths();
    // Orden previo invertido: cada hijo antes que su padre.
    for &id in order.iter().rev() {
        if id == start {
            continue;
        }
        let rel = if paths {
            vfs.relative_path(start, id).unwrap_or_default()
        } else {
            String::new()
        };
        if visible[id.index()] || f.matches_self(vfs, id, &rel) {
            visible[id.index()] = true;
            if let Some(p) = vfs.parent(id) {
                visible[p.index()] = true;
            }
        }
    }
    Ok(if recursive {
        order
            .into_iter()
            .filter(|id| *id != start && visible[id.index()])
            .collect()
    } else {
        vfs.children(start)
            .iter()
            .copied()
            .filter(|id| visible[id.index()])
            .collect()
    })
}

/// Categorías de una lista `vivo,borrado,...` (códigos de [`Category::code`]).
pub fn parse_categories(s: &str) -> Result<u32> {
    s.split(',')
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .try_fold(0u32, |m, c| {
            Category::from_code(c)
                .map(|c| m | c.bit())
                .ok_or_else(|| VfsError::InvalidFilter(format!("categoría desconocida: {c}")))
        })
}

/// Estados de una lista `integro,parcial,...` (códigos de [`DataState::code`]).
pub fn parse_data_states(s: &str) -> Result<u32> {
    s.split(',')
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .try_fold(0u32, |m, c| {
            DataState::from_code(c)
                .map(|d| m | d.bit())
                .ok_or_else(|| VfsError::InvalidFilter(format!("estado desconocido: {c}")))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matches_wildcards() {
        let g = |p: &str, s: &str| glob(&lower(p), &lower(s));
        assert!(g("*.txt", "a.TXT"));
        assert!(g("*", ""));
        assert!(g("a?c", "abc"));
        assert!(!g("a?c", "ac"));
        assert!(g("*a*b*", "xxaxxbxx"));
        assert!(!g("*a*b", "xxaxxbxxc"));
        assert!(g("ñ*", "Ñandú"));
        assert!(!g("", "a"));
    }

    #[test]
    fn rules_first_match_wins_with_exclusions() {
        let r = NameRules::parse(" !tmp*; *.txt ;*.doc").unwrap();
        assert_eq!(r.len(), 3);
        assert!(r.matches("a.txt", ""));
        assert!(!r.matches("tmp.txt", ""));
        assert!(r.matches("X.DOC", ""));
        assert!(!r.matches("a.bin", ""));
        // Solo exclusiones: lo demás pasa.
        let r = NameRules::parse("!*.tmp").unwrap();
        assert!(r.matches("a.bin", ""));
        assert!(!r.matches("a.TMP", ""));
        // Reglas de ruta.
        let r = NameRules::parse("docs/*.txt").unwrap();
        assert!(r.uses_paths());
        assert!(r.matches("a.txt", "docs/a.txt"));
        assert!(!r.matches("a.txt", "otros/a.txt"));
        assert!(NameRules::parse("").unwrap().matches("x", ""));
        assert!(NameRules::parse("!").is_err());
        assert!(NameRules::parse(&"a;".repeat(MAX_RULES + 1)).is_err());
    }

    #[test]
    fn category_and_state_lists() {
        assert_eq!(
            parse_categories("vivo, borrado").unwrap(),
            Category::Live.bit() | Category::Deleted.bit()
        );
        assert!(parse_categories("otra").is_err());
        assert_eq!(
            parse_data_states("integro,parcial").unwrap(),
            DataState::Integro.bit() | DataState::Parcial.bit()
        );
        assert!(parse_data_states("x").is_err());
    }
}
