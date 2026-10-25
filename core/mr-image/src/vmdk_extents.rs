//! VMDK descrito por un **descriptor de texto separado** con una o varias extensiones
//! (`monolithicFlat`, `twoGbMaxExtentFlat`, `twoGbMaxExtentSparse` y un descriptor que cita un
//! único fichero SPARSE), en **solo lectura** y con apertura **explícita** de cada extensión
//! (ADR-018, addendum de la incidencia 11).
//!
//! Basado en la especificación pública de VMware «Virtual Disk Format 1.1». Cada línea de
//! extensión del descriptor es `ACCESO SECTORES TIPO ["FICHERO" [DESPLAZAMIENTO]]`; el disco
//! virtual es la concatenación de las extensiones en el orden del descriptor.
//!
//! Política:
//! - [`read_plan`] / [`plan_from_text`] interpretan el descriptor y devuelven las extensiones que
//!   hacen falta ([`ExtentPlan`]) **sin abrir ningún fichero** citado.
//! - [`VmdkExtentImage::open`] solo abre los ficheros que el llamador pasa en un
//!   [`ApprovedFiles`] (pares «nombre declarado → ruta elegida por el usuario»). La búsqueda es
//!   por coincidencia exacta del nombre declarado: nunca se recorre un directorio ni se resuelve
//!   una ruta del descriptor. Una extensión necesaria sin fichero aprobado da
//!   `ExternalReference`; un fichero aprobado que no corresponde a ninguna extensión necesaria,
//!   o que es el propio descriptor, es un error del llamador.
//! - Nombres declarados que no son un nombre de fichero simple (rutas absolutas o relativas con
//!   separadores, `..`, UNC, unidades, flujos alternativos, nombres de dispositivo DOS,
//!   caracteres no válidos en Windows, espacios o puntos finales, texto no UTF-8) se rechazan
//!   con `UnsupportedFeature`, aunque se aprueben.
//! - Tipos: FLAT (bytes en `[DESPLAZAMIENTO × 512, + SECTORES × 512)` del fichero, que debe
//!   cubrirlos), SPARSE (lector monolithicSparse/streamOptimized de [`VmdkImage`] por extensión,
//!   con capacidad igual a los sectores declarados) y ZERO (ceros, disperso). Las extensiones
//!   NOACCESS no se abren y se leen como **ilegibles** (`IoError::Read`, `Data` en `allocation`,
//!   `Unreadable` en el lector tolerante). VMFS, VMFSSPARSE, VMFSRDM, VMFSRAW y SESPARSE:
//!   `UnsupportedFeature`.
//! - El tamaño virtual es la suma de las extensiones; los nombres no pueden repetirse (sin
//!   distinguir mayúsculas, como en NTFS); un disco con padre (`parentFileNameHint` o
//!   `parentCID`) sigue siendo `ExternalReference` (fuera del alcance).

use crate::external::ApprovedFiles;
use crate::tolerant;
use crate::vmdk::{
    corrupt, descriptor_text, external, parse_descriptor, read_exact_at, sanitize, unsupported,
    ExtentAccess, ExtentType, VmdkImage, MAX_DESCRIPTOR_BYTES, TEXT_DESCRIPTOR_SIGNATURE,
};
use mr_io::{Allocation, AllocationRun, BlockDevice, IoError, Result};
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

const FORMAT: &str = "VMDK";
const SECTOR: u64 = 512;
/// Extensiones máximas de un descriptor (un disco de 62 TiB en trozos de 2 GiB tiene unas 31 744).
pub const MAX_EXTENTS: usize = 32_768;
/// Tipos de disco (`createType`) que se abren con este lector.
pub const SUPPORTED_CREATE_TYPES: [&str; 4] = [
    "monolithicFlat",
    "twoGbMaxExtentFlat",
    "twoGbMaxExtentSparse",
    "monolithicSparse",
];

/// Clase de problema que impide abrir el disco.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueKind {
    /// Estructura incoherente (`IoError::Corrupt`).
    Corrupt,
    /// Variante o nombre no admitido (`IoError::UnsupportedFeature`).
    Unsupported,
    /// Depende de un disco padre (`IoError::ExternalReference`).
    Parent,
}

/// Problema que impide abrir el disco, con un texto saneado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub kind: IssueKind,
    pub text: String,
}

impl Issue {
    fn corrupt(text: impl Into<String>) -> Self {
        Self {
            kind: IssueKind::Corrupt,
            text: text.into(),
        }
    }

    fn unsupported(text: impl Into<String>) -> Self {
        Self {
            kind: IssueKind::Unsupported,
            text: text.into(),
        }
    }

    fn to_error(&self) -> IoError {
        match self.kind {
            IssueKind::Corrupt => corrupt(self.text.clone()),
            IssueKind::Unsupported => unsupported(self.text.clone()),
            IssueKind::Parent => external(&self.text),
        }
    }
}

/// Una extensión del descriptor y lo que hace falta para leerla.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredExtent {
    /// Posición en el descriptor (desde 0).
    pub index: usize,
    pub access: ExtentAccess,
    pub kind: ExtentType,
    /// Tamaño en sectores de 512 bytes.
    pub sectors: u64,
    /// Desplazamiento en sectores dentro del fichero (FLAT; 0 si no se declara).
    pub offset: u64,
    /// Primer sector virtual que cubre.
    pub start_sector: u64,
    /// Nombre de fichero **exactamente** como lo declara el descriptor (solo texto; nunca se
    /// abre por sí mismo). `None` en ZERO.
    pub file_name: Option<String>,
    /// `true` si hay que aprobar un fichero para este nombre (FLAT o SPARSE con acceso).
    pub needs_file: bool,
    /// Motivo por el que esta extensión impide la apertura (nombre no admitido, tipo no
    /// soportado…), o `None`.
    pub issue: Option<Issue>,
}

/// Resultado de interpretar un descriptor separado, sin abrir ninguna extensión.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtentPlan {
    /// `createType` declarado (saneado).
    pub create_type: Option<String>,
    /// Referencia al disco padre (saneada), si existe.
    pub parent: Option<String>,
    /// Tamaño virtual en bytes (suma de las extensiones; 0 si desborda).
    pub size: u64,
    pub extents: Vec<RequiredExtent>,
    /// Problemas del descriptor en su conjunto.
    pub issues: Vec<Issue>,
}

impl ExtentPlan {
    /// Primer problema que impide abrir el disco: los del descriptor y luego los de cada
    /// extensión, en orden.
    pub fn first_issue(&self) -> Option<&Issue> {
        self.issues
            .first()
            .or_else(|| self.extents.iter().find_map(|e| e.issue.as_ref()))
    }

    /// `true` si el descriptor se puede abrir una vez aprobados sus ficheros.
    pub fn is_openable(&self) -> bool {
        self.first_issue().is_none()
    }

    /// Nombres declarados para los que hay que aprobar un fichero, en orden.
    pub fn required_names(&self) -> impl Iterator<Item = &str> {
        self.extents
            .iter()
            .filter(|e| e.needs_file)
            .filter_map(|e| e.file_name.as_deref())
    }

    /// Resumen legible: tipo, tamaño y extensiones por tipo.
    pub fn summary(&self) -> String {
        let mut s = format!(
            "VMDK {} (descriptor separado), {} {}",
            self.create_type.as_deref().unwrap_or("sin createType"),
            self.extents.len(),
            if self.extents.len() == 1 {
                "extensión"
            } else {
                "extensiones"
            }
        );
        let count =
            |f: &dyn Fn(&RequiredExtent) -> bool| self.extents.iter().filter(|e| f(e)).count();
        let flat = count(&|e| e.kind == ExtentType::Flat && e.access != ExtentAccess::NoAccess);
        let sparse = count(&|e| e.kind == ExtentType::Sparse && e.access != ExtentAccess::NoAccess);
        let zero = count(&|e| e.kind == ExtentType::Zero && e.access != ExtentAccess::NoAccess);
        let noaccess = count(&|e| e.access == ExtentAccess::NoAccess);
        let mut parts = Vec::new();
        if flat > 0 {
            parts.push(format!("FLAT ×{flat}"));
        }
        if sparse > 0 {
            parts.push(format!("SPARSE ×{sparse}"));
        }
        if zero > 0 {
            parts.push(format!("ZERO ×{zero}"));
        }
        if noaccess > 0 {
            parts.push(format!("NOACCESS ×{noaccess} (ilegibles)"));
        }
        if !parts.is_empty() {
            s.push_str(&format!(" ({})", parts.join(", ")));
        }
        s
    }
}

/// Nombre de tipo tal como aparece en el descriptor.
pub fn extent_type_name(k: ExtentType) -> &'static str {
    match k {
        ExtentType::Sparse => "SPARSE",
        ExtentType::Flat => "FLAT",
        ExtentType::Zero => "ZERO",
        ExtentType::Vmfs => "VMFS",
        ExtentType::VmfsSparse => "VMFSSPARSE",
        ExtentType::VmfsRdm => "VMFSRDM",
        ExtentType::VmfsRaw => "VMFSRAW",
        ExtentType::SeSparse => "SESPARSE",
    }
}

/// Nombre de acceso tal como aparece en el descriptor.
pub fn extent_access_name(a: ExtentAccess) -> &'static str {
    match a {
        ExtentAccess::ReadWrite => "RW",
        ExtentAccess::ReadOnly => "RDONLY",
        ExtentAccess::NoAccess => "NOACCESS",
    }
}

/// Motivo por el que un nombre declarado no es un nombre de fichero simple, o `None` si lo es.
pub fn check_extent_name(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        return Some("nombre vacío");
    }
    if name.contains('\u{fffd}') {
        return Some("el nombre no es texto UTF-8 válido");
    }
    if name == "." || name == ".." || name.contains("..") {
        return Some("contiene «..»");
    }
    if name.starts_with("\\\\") || name.starts_with("//") {
        return Some("ruta UNC o de dispositivo");
    }
    if name.contains(['/', '\\']) {
        return Some("contiene separadores de ruta (ruta absoluta o con directorios)");
    }
    if name.contains(':') {
        return Some("contiene «:» (unidad, flujo alternativo o dispositivo)");
    }
    if name.contains(['<', '>', '"', '|', '?', '*']) || name.chars().any(char::is_control) {
        return Some("contiene caracteres no válidos en un nombre de fichero");
    }
    if name.trim() != name || name.ends_with('.') {
        return Some("empieza o termina con espacios o termina en punto");
    }
    if mr_io::path_policy::check_lexical(name).is_err() {
        return Some("nombre reservado de dispositivo");
    }
    None
}

/// Interpreta el texto de un descriptor separado. Errores solo si el texto no se puede
/// interpretar (`Corrupt`); los problemas semánticos quedan en [`ExtentPlan::issues`] y en cada
/// extensión, para poder mostrarlos.
pub fn plan_from_text(text: &str) -> Result<ExtentPlan> {
    let d = parse_descriptor(text)?;
    let mut issues = Vec::new();
    let parent = d.parent_reference().map(|p| sanitize(&p));
    if let Some(p) = &parent {
        issues.push(Issue {
            kind: IssueKind::Parent,
            text: p.clone(),
        });
    }
    let create_type = d.create_type.as_deref().map(sanitize);
    match d.create_type.as_deref() {
        None => issues.push(Issue::corrupt("descriptor sin createType")),
        Some(t)
            if !SUPPORTED_CREATE_TYPES
                .iter()
                .any(|s| s.eq_ignore_ascii_case(t)) =>
        {
            issues.push(Issue::unsupported(format!(
                "createType «{}» (se admiten {})",
                sanitize(t),
                SUPPORTED_CREATE_TYPES.join(", ")
            )))
        }
        _ => {}
    }
    if d.extents.is_empty() {
        issues.push(Issue::corrupt("descriptor sin extensiones"));
    }
    if d.extents.len() > MAX_EXTENTS {
        issues.push(Issue::unsupported(format!(
            "{} extensiones (máximo {MAX_EXTENTS})",
            d.extents.len()
        )));
    }
    let mut extents: Vec<RequiredExtent> = Vec::with_capacity(d.extents.len().min(MAX_EXTENTS));
    let mut start: Option<u64> = Some(0);
    for (index, e) in d.extents.iter().enumerate().take(MAX_EXTENTS) {
        let n = index + 1;
        let start_sector = start.unwrap_or(0);
        start = start
            .and_then(|s| s.checked_add(e.sectors))
            .filter(|&s| s <= u64::MAX / SECTOR);
        let offset = e.offset.unwrap_or(0);
        let readable = e.access != ExtentAccess::NoAccess;
        let needs_file = readable && matches!(e.kind, ExtentType::Flat | ExtentType::Sparse);
        let name = e.file_name.as_deref();
        let issue = if e.sectors == 0 {
            Some(Issue::corrupt(format!("la extensión {n} mide 0 sectores")))
        } else if !readable {
            // No se abre: ni el nombre ni el tipo importan.
            None
        } else if !matches!(
            e.kind,
            ExtentType::Flat | ExtentType::Sparse | ExtentType::Zero
        ) {
            Some(Issue::unsupported(format!(
                "extensión {n} de tipo {} (se admiten FLAT, SPARSE y ZERO)",
                extent_type_name(e.kind)
            )))
        } else if let Some(why) = name.and_then(check_extent_name) {
            Some(Issue::unsupported(format!(
                "la extensión {n} declara «{}», que no es un nombre de fichero simple ({why}); \
                 por seguridad no se admite",
                sanitize(name.unwrap_or(""))
            )))
        } else if e.kind == ExtentType::Sparse && offset != 0 {
            Some(Issue::unsupported(format!(
                "la extensión SPARSE {n} declara un desplazamiento ({offset})"
            )))
        } else if e.kind == ExtentType::Flat
            && offset
                .checked_add(e.sectors)
                .and_then(|s| s.checked_mul(SECTOR))
                .is_none()
        {
            Some(Issue::corrupt(format!(
                "la extensión {n} desborda (desplazamiento {offset} + {} sectores)",
                e.sectors
            )))
        } else {
            None
        };
        extents.push(RequiredExtent {
            index,
            access: e.access,
            kind: e.kind,
            sectors: e.sectors,
            offset,
            start_sector,
            file_name: e.file_name.clone(),
            needs_file,
            issue,
        });
    }
    let size = match start {
        Some(s) => s * SECTOR,
        None => {
            issues.push(Issue::corrupt(
                "la suma de las extensiones desborda el tamaño máximo",
            ));
            0
        }
    };
    // Nombres repetidos (sin distinguir mayúsculas: el mismo fichero en NTFS).
    let mut names: Vec<(String, usize)> = extents
        .iter()
        .filter_map(|e| e.file_name.as_deref().map(|n| (n.to_lowercase(), e.index)))
        .collect();
    names.sort();
    for w in names.windows(2) {
        if w[0].0 == w[1].0 {
            issues.push(Issue::corrupt(format!(
                "las extensiones {} y {} declaran el mismo fichero «{}»",
                w[0].1 + 1,
                w[1].1 + 1,
                sanitize(&w[1].0)
            )));
            break;
        }
    }
    Ok(ExtentPlan {
        create_type,
        parent,
        size,
        extents,
        issues,
    })
}

/// Lee e interpreta un descriptor separado ya admitido y abierto en solo lectura. No abre nada
/// más. Un fichero que no es un descriptor de texto (o mide más de 1 MiB) es `UnsupportedFeature`.
pub fn read_plan(file: &File) -> Result<ExtentPlan> {
    let len = file.metadata()?.len();
    if len > MAX_DESCRIPTOR_BYTES as u64 {
        return Err(unsupported(format!(
            "el descriptor mide {len} bytes (máximo {MAX_DESCRIPTOR_BYTES})"
        )));
    }
    let mut raw = vec![0u8; len as usize];
    read_exact_at(file, 0, &mut raw)?;
    let body = raw.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&raw);
    let n = TEXT_DESCRIPTOR_SIGNATURE.len();
    if body.len() < n || !body[..n].eq_ignore_ascii_case(TEXT_DESCRIPTOR_SIGNATURE) {
        return Err(unsupported(
            "el fichero no es un descriptor VMDK de texto separado («# Disk DescriptorFile»)",
        ));
    }
    plan_from_text(&descriptor_text(&raw))
}

/// Contenido de una extensión abierta.
enum Backing {
    /// Fichero plano y desplazamiento en bytes de la extensión dentro de él.
    Flat {
        file: File,
        base: u64,
    },
    Sparse(Box<VmdkImage>),
    Zero,
    NoAccess,
}

struct Part {
    /// Primer sector virtual.
    start: u64,
    sectors: u64,
    backing: Backing,
}

/// Añade a un error de la extensión `n` («nombre») el contexto de qué extensión es.
fn in_extent(e: IoError, n: usize, name: &str) -> IoError {
    let ctx = |d: String| format!("extensión {n} («{}»): {d}", sanitize(name));
    match e {
        IoError::Corrupt { format, detail } => IoError::Corrupt {
            format,
            detail: ctx(detail),
        },
        IoError::UnsupportedFeature { format, detail } => IoError::UnsupportedFeature {
            format,
            detail: ctx(detail),
        },
        other => other,
    }
}

fn invalid_input(detail: String) -> IoError {
    IoError::Io(io::Error::new(io::ErrorKind::InvalidInput, detail))
}

/// Disco VMDK de varias extensiones (o de una extensión en otro fichero) abierto en solo
/// lectura a partir de su descriptor separado y de los ficheros aprobados por el usuario.
pub struct VmdkExtentImage {
    parts: Vec<Part>,
    size: u64,
    plan: ExtentPlan,
    /// Ficheros abiertos, en el orden de las extensiones.
    paths: Vec<PathBuf>,
    tolerant: bool,
    discarded: u64,
}

impl std::fmt::Debug for VmdkExtentImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VmdkExtentImage")
            .field("size", &self.size)
            .field("extents", &self.parts.len())
            .finish()
    }
}

impl VmdkExtentImage {
    /// Abre el disco que describe `descriptor` (ya admitido y abierto en solo lectura; su ruta
    /// canónica es `descriptor_path`) usando **solo** los ficheros de `approved`. `tolerant`
    /// se aplica a las extensiones SPARSE (ADR-020).
    ///
    /// Errores: problema del descriptor ([`ExtentPlan::first_issue`]); extensión necesaria sin
    /// fichero aprobado (`ExternalReference` con los nombres que faltan); fichero aprobado que no
    /// corresponde a ninguna extensión necesaria o que es el propio descriptor (`IoError::Io` con
    /// `InvalidInput`); fichero FLAT que no cubre su extensión o extensión SPARSE no válida
    /// (`Corrupt`).
    pub fn open(
        descriptor: &File,
        descriptor_path: &Path,
        approved: &ApprovedFiles,
        tolerant: bool,
    ) -> Result<Self> {
        let plan = read_plan(descriptor)?;
        if let Some(issue) = plan.first_issue() {
            return Err(issue.to_error());
        }
        let required: Vec<&str> = plan.required_names().collect();
        for a in approved.iter() {
            if a.path() == descriptor_path {
                return Err(invalid_input(format!(
                    "«{}» es el propio descriptor, no una extensión",
                    sanitize(a.role())
                )));
            }
            if !required.contains(&a.role()) {
                return Err(invalid_input(format!(
                    "el fichero aprobado para «{}» no corresponde a ninguna extensión que haya que \
                     abrir (los nombres deben coincidir exactamente con el descriptor)",
                    sanitize(a.role())
                )));
            }
        }
        let missing: Vec<String> = required
            .iter()
            .filter(|n| approved.get(n).is_none())
            .map(|n| sanitize(n))
            .collect();
        if !missing.is_empty() {
            return Err(IoError::ExternalReference {
                format: FORMAT,
                reference: missing.join("», «"),
            });
        }

        let mut parts = Vec::with_capacity(plan.extents.len());
        let mut paths = Vec::new();
        let mut discarded = 0u64;
        for e in &plan.extents {
            let n = e.index + 1;
            let backing = if e.access == ExtentAccess::NoAccess {
                Backing::NoAccess
            } else if e.kind == ExtentType::Zero {
                Backing::Zero
            } else {
                let name = e.file_name.as_deref().unwrap_or("");
                let a = approved.get(name).ok_or_else(|| external(name))?;
                let file = a.try_clone_file()?;
                paths.push(a.path().to_path_buf());
                match e.kind {
                    ExtentType::Flat => {
                        // Comprobado en el plan: no desborda.
                        let base = e.offset * SECTOR;
                        let end = (e.offset + e.sectors) * SECTOR;
                        let len = file.metadata()?.len();
                        if len < end {
                            return Err(corrupt(format!(
                                "extensión {n} («{}»): el fichero mide {len} bytes y la extensión \
                                 FLAT necesita {end} (desplazamiento {} + {} sectores)",
                                sanitize(name),
                                e.offset,
                                e.sectors
                            )));
                        }
                        Backing::Flat { file, base }
                    }
                    _ => {
                        let img = VmdkImage::open_extent(file, tolerant, e.sectors)
                            .map_err(|err| in_extent(err, n, name))?;
                        discarded += img.discarded_blocks();
                        Backing::Sparse(Box::new(img))
                    }
                }
            };
            parts.push(Part {
                start: e.start_sector,
                sectors: e.sectors,
                backing,
            });
        }
        Ok(Self {
            parts,
            size: plan.size,
            plan,
            paths,
            tolerant,
            discarded,
        })
    }

    /// Plan del descriptor con el que se abrió.
    pub fn plan(&self) -> &ExtentPlan {
        &self.plan
    }

    /// Rutas canónicas de los ficheros de extensión abiertos, en orden.
    pub fn extent_paths(&self) -> &[PathBuf] {
        &self.paths
    }

    /// Granos descartados en modo tolerante (suma de las extensiones SPARSE).
    pub fn discarded_blocks(&self) -> u64 {
        self.discarded
    }

    pub fn is_tolerant(&self) -> bool {
        self.tolerant
    }

    /// Variante legible.
    pub fn describe(&self) -> String {
        let mut s = self.plan.summary();
        if self.tolerant {
            s.push_str(&tolerant::describe_suffix(
                self.discarded,
                "grano",
                "granos",
            ));
        }
        s
    }

    /// Índice de la extensión que contiene el sector virtual `s` (`s` < total).
    fn part_of(&self, s: u64) -> usize {
        self.parts.partition_point(|p| p.start + p.sectors <= s)
    }
}

fn noaccess_read(n: usize, offset: u64, len: usize) -> IoError {
    IoError::Read {
        offset,
        len,
        source: io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{FORMAT}: la extensión {n} es NOACCESS: el descriptor prohíbe leerla"),
        ),
    }
}

impl BlockDevice for VmdkExtentImage {
    fn size(&self) -> u64 {
        self.size
    }

    fn sector_size(&self) -> u32 {
        SECTOR as u32
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let end = offset
            .checked_add(buf.len() as u64)
            .filter(|&e| e <= self.size)
            .ok_or(IoError::OutOfBounds {
                offset,
                len: buf.len(),
                size: self.size,
            })?;
        let mut pos = offset;
        let mut done = 0usize;
        while pos < end {
            let i = self.part_of(pos / SECTOR);
            let p = self
                .parts
                .get(i)
                .ok_or_else(|| corrupt(format!("desplazamiento {pos} fuera de las extensiones")))?;
            let part_start = p.start * SECTOR;
            let within = pos - part_start;
            let n = (p.sectors * SECTOR - within).min(end - pos) as usize;
            let chunk = &mut buf[done..done + n];
            match &p.backing {
                Backing::Zero => chunk.fill(0),
                Backing::NoAccess => return Err(noaccess_read(i + 1, pos, n)),
                Backing::Flat { file, base } => {
                    read_exact_at(file, base + within, chunk).map_err(|e| match e {
                        // El rango virtual, no el del fichero.
                        IoError::Read { source, .. } => IoError::Read {
                            offset: pos,
                            len: n,
                            source,
                        },
                        other => other,
                    })?
                }
                Backing::Sparse(img) => img.read_at(within, chunk).map_err(|e| match e {
                    IoError::Read {
                        offset,
                        len,
                        source,
                    } => IoError::Read {
                        offset: offset.saturating_add(part_start),
                        len,
                        source,
                    },
                    other => other,
                })?,
            }
            pos += n as u64;
            done += n;
        }
        Ok(())
    }

    fn allocation(&self, lba: u64, count: u64) -> Result<Vec<AllocationRun>> {
        let total = self.sector_count();
        let end = match lba.checked_add(count) {
            Some(e) if count > 0 && e <= total => e,
            _ => return Err(IoError::InvalidRange { lba, count, total }),
        };
        let mut runs: Vec<AllocationRun> = Vec::new();
        let mut push = |s: u64, n: u64, k: Allocation| match runs.last_mut() {
            Some((s0, n0, k0)) if *k0 == k && *s0 + *n0 == s => *n0 += n,
            _ => runs.push((s, n, k)),
        };
        let mut s = lba;
        while s < end {
            let p = self
                .parts
                .get(self.part_of(s))
                .ok_or_else(|| corrupt(format!("sector {s} fuera de las extensiones")))?;
            let e = (p.start + p.sectors).min(end);
            match &p.backing {
                Backing::Zero => push(s, e - s, Allocation::Sparse),
                // Ilegible: «datos» para que el lector tolerante lo intente y lo marque.
                Backing::NoAccess | Backing::Flat { .. } => push(s, e - s, Allocation::Data),
                Backing::Sparse(img) => {
                    for (l, n, k) in img.allocation(s - p.start, e - s)? {
                        push(l + p.start, n, k);
                    }
                }
            }
            s = e;
        }
        Ok(runs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vmdk::tests::sparse_extent_bytes;

    fn plan(text: &str) -> ExtentPlan {
        plan_from_text(text).unwrap()
    }

    fn desc(create: &str, lines: &[&str]) -> String {
        format!(
            "# Disk DescriptorFile\nversion=1\nCID=1\nparentCID=ffffffff\ncreateType=\"{create}\"\n\n{}\n\nddb.adapterType = \"ide\"\n",
            lines.join("\n")
        )
    }

    struct Fixture {
        dir: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                dir: tempfile::tempdir().unwrap(),
            }
        }
        fn path(&self, n: &str) -> PathBuf {
            self.dir.path().join(n)
        }
        fn write(&self, n: &str, b: &[u8]) -> PathBuf {
            let p = self.path(n);
            std::fs::write(&p, b).unwrap();
            p
        }
        fn open(
            &self,
            desc_name: &str,
            approved: &[(&str, &str)],
            tolerant: bool,
        ) -> Result<VmdkExtentImage> {
            let dp = mr_io::path_policy::admit(&self.path(desc_name)).unwrap();
            let f = mr_io::path_policy::open_regular_file(&dp).unwrap();
            let ap = ApprovedFiles::new(approved.iter().map(|(r, p)| (*r, self.path(p))))?;
            VmdkExtentImage::open(&f, &dp, &ap, tolerant)
        }
    }

    fn pattern(len: usize, seed: u8) -> Vec<u8> {
        (0..len)
            .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
            .collect()
    }

    fn read_all(d: &dyn BlockDevice) -> Vec<u8> {
        let mut v = vec![0u8; d.size() as usize];
        d.read_at(0, &mut v).unwrap();
        v
    }

    #[test]
    fn plan_lists_extents_without_opening() {
        let p = plan(&desc(
            "twoGbMaxExtentFlat",
            &[
                "RW 8 FLAT \"a-f001.vmdk\" 0",
                "RDONLY 4 FLAT \"a-f002.vmdk\" 2",
                "RW 16 ZERO",
                "NOACCESS 8 FLAT \"../../Windows/x\" 0",
                "RW 32 SPARSE \"a-s003.vmdk\"",
            ],
        ));
        assert!(p.is_openable(), "{:?}", p.first_issue());
        assert_eq!(p.size, (8 + 4 + 16 + 8 + 32) * 512);
        assert_eq!(
            p.required_names().collect::<Vec<_>>(),
            ["a-f001.vmdk", "a-f002.vmdk", "a-s003.vmdk"]
        );
        assert_eq!(p.extents[1].offset, 2);
        assert_eq!(p.extents[2].start_sector, 12);
        assert!(!p.extents[3].needs_file);
        assert!(p.summary().contains("NOACCESS ×1"), "{}", p.summary());
        assert_eq!(p.create_type.as_deref(), Some("twoGbMaxExtentFlat"));
    }

    #[test]
    fn malicious_names_are_rejected() {
        for name in [
            "..\\..\\Windows\\System32\\config\\SAM",
            "../etc/passwd",
            "..",
            "a..b",
            "C:\\x.vmdk",
            "C:x.vmdk",
            "/etc/passwd",
            "\\\\servidor\\recurso\\x.vmdk",
            "\\\\?\\GLOBALROOT\\Device\\Harddisk0\\DR0",
            "\\\\.\\PhysicalDrive0",
            "sub/x.vmdk",
            "x.vmdk:flujo",
            "NUL",
            "con.vmdk",
            "COM1",
            " x.vmdk",
            "x.vmdk ",
            "x.vmdk.",
            "x?.vmdk",
            "x\u{fffd}.vmdk",
        ] {
            let p = plan(&desc(
                "monolithicFlat",
                &[&format!("RW 8 FLAT \"{name}\" 0")],
            ));
            let i = p.first_issue().unwrap_or_else(|| panic!("aceptó {name:?}"));
            assert_eq!(i.kind, IssueKind::Unsupported, "{name:?}");
            assert!(matches!(i.to_error(), IoError::UnsupportedFeature { .. }));
        }
        assert!(check_extent_name("disco-flat.vmdk").is_none());
        assert!(check_extent_name("disco con espacios ñ.vmdk").is_none());
    }

    #[test]
    fn semantic_problems() {
        let cases: &[(&str, &[&str], IssueKind)] = &[
            (
                "monolithicFlat",
                &["RW 8 FLAT \"a.vmdk\" 0", "RW 8 FLAT \"A.VMDK\" 8"],
                IssueKind::Corrupt,
            ),
            (
                "monolithicFlat",
                &["RW 0 FLAT \"a.vmdk\" 0"],
                IssueKind::Corrupt,
            ),
            ("monolithicFlat", &[], IssueKind::Corrupt),
            ("vmfs", &["RW 8 VMFS \"a.vmdk\""], IssueKind::Unsupported),
            (
                "monolithicFlat",
                &["RW 8 VMFS \"a.vmdk\""],
                IssueKind::Unsupported,
            ),
            (
                "monolithicFlat",
                &["RW 8 SESPARSE \"a.vmdk\""],
                IssueKind::Unsupported,
            ),
            (
                "twoGbMaxExtentSparse",
                &["RW 8 SPARSE \"a.vmdk\" 4"],
                IssueKind::Unsupported,
            ),
            (
                "monolithicFlat",
                &["RW 18446744073709551615 FLAT \"a.vmdk\" 1"],
                IssueKind::Corrupt,
            ),
            (
                "monolithicFlat",
                &[
                    "RW 18446744073709551615 ZERO",
                    "RW 18446744073709551615 ZERO",
                ],
                IssueKind::Corrupt,
            ),
        ];
        for (ct, lines, kind) in cases {
            let p = plan(&desc(ct, lines));
            assert_eq!(
                p.first_issue().map(|i| i.kind),
                Some(*kind),
                "{ct} {lines:?}"
            );
        }
        // Padre: ExternalReference (fuera del alcance).
        let t = desc("monolithicFlat", &["RW 8 FLAT \"a.vmdk\" 0"]).replace(
            "parentCID=ffffffff",
            "parentCID=12345678\nparentFileNameHint=\"..\\\\base.vmdk\"",
        );
        let p = plan(&t);
        assert_eq!(p.first_issue().unwrap().kind, IssueKind::Parent);
        assert!(matches!(
            p.first_issue().unwrap().to_error(),
            IoError::ExternalReference { .. }
        ));
        let p = plan("# Disk DescriptorFile\nRW 8 FLAT \"a.vmdk\" 0\n");
        assert_eq!(p.first_issue().unwrap().kind, IssueKind::Corrupt);
        // Texto que no se puede interpretar.
        assert!(plan_from_text("# Disk DescriptorFile\nRW x FLAT \"a\"\n").is_err());
        assert!(plan_from_text("# Disk DescriptorFile\nRW 8 FLAT a.vmdk\n").is_err());
    }

    #[test]
    fn opens_flat_zero_noaccess_and_sparse_extents() {
        let fx = Fixture::new();
        let a = pattern(8 * 512, 1);
        let mut b_file = vec![0xEEu8; 2 * 512];
        let b = pattern(4 * 512, 2);
        b_file.extend_from_slice(&b);
        b_file.extend_from_slice(&[0xEE; 512]);
        fx.write("d-f001.vmdk", &a);
        fx.write("d-f002.vmdk", &b_file);
        // Extensión SPARSE de 64 sectores (granos de 16): granos 0 y 2 con datos.
        let sp = sparse_extent_bytes(64, &[(0, 0x41), (2, 0x42)]);
        fx.write("d-s003.vmdk", &sp);
        let text = desc(
            "twoGbMaxExtentFlat",
            &[
                "RW 8 FLAT \"d-f001.vmdk\" 0",
                "RDONLY 4 FLAT \"d-f002.vmdk\" 2",
                "RW 16 ZERO",
                "NOACCESS 8 FLAT \"no-se-abre.vmdk\" 0",
                "RW 64 SPARSE \"d-s003.vmdk\"",
            ],
        );
        fx.write("d.vmdk", text.as_bytes());
        let before: Vec<Vec<u8>> = ["d-f001.vmdk", "d-f002.vmdk", "d-s003.vmdk", "d.vmdk"]
            .iter()
            .map(|n| std::fs::read(fx.path(n)).unwrap())
            .collect();
        let img = fx
            .open(
                "d.vmdk",
                &[
                    ("d-f001.vmdk", "d-f001.vmdk"),
                    ("d-f002.vmdk", "d-f002.vmdk"),
                    ("d-s003.vmdk", "d-s003.vmdk"),
                ],
                false,
            )
            .unwrap();
        assert_eq!(img.size(), (8 + 4 + 16 + 8 + 64) * 512);
        assert_eq!(img.extent_paths().len(), 3);
        assert!(
            img.describe().contains("twoGbMaxExtentFlat"),
            "{}",
            img.describe()
        );
        let mut buf = vec![0u8; 12 * 512];
        img.read_at(0, &mut buf).unwrap();
        assert_eq!(&buf[..8 * 512], &a[..]);
        assert_eq!(&buf[8 * 512..], &b[..]);
        // Lectura que cruza FLAT → ZERO.
        let mut x = vec![1u8; 1024];
        img.read_at(11 * 512, &mut x).unwrap();
        assert_eq!(&x[..512], &b[3 * 512..]);
        assert!(x[512..].iter().all(|&v| v == 0));
        // NOACCESS: ilegible, con el rango virtual.
        let mut y = vec![0u8; 512];
        match img.read_at(28 * 512, &mut y) {
            Err(IoError::Read { offset, len, .. }) => assert_eq!((offset, len), (28 * 512, 512)),
            other => panic!("{other:?}"),
        }
        assert!(img.read_at(27 * 512, &mut vec![0u8; 1024]).is_err());
        // SPARSE: granos 0 y 2 con datos, 1 y 3 dispersos.
        let mut z = vec![0u8; 64 * 512];
        img.read_at(36 * 512, &mut z).unwrap();
        let g = 16 * 512;
        assert!(z[..g].iter().all(|&v| v == 0x41));
        assert!(z[g..2 * g].iter().all(|&v| v == 0));
        assert!(z[2 * g..3 * g].iter().all(|&v| v == 0x42));
        assert!(z[3 * g..].iter().all(|&v| v == 0));
        assert_eq!(
            img.allocation(0, img.sector_count()).unwrap(),
            vec![
                (0, 12, Allocation::Data),
                (12, 16, Allocation::Sparse),
                (28, 8 + 16, Allocation::Data),
                (52, 16, Allocation::Sparse),
                (68, 16, Allocation::Data),
                (84, 16, Allocation::Sparse),
            ]
        );
        assert!(img.allocation(0, img.sector_count() + 1).is_err());
        assert!(img.read_at(img.size() - 1, &mut [0u8; 2]).is_err());
        // Lector tolerante: NOACCESS ilegible, el resto idéntico.
        let report = mr_io::resilient::read_range(
            &img,
            0,
            img.sector_count(),
            &mr_io::resilient::ReadPolicy {
                on_error: mr_io::resilient::OnError::FillAndContinue,
                ..mr_io::resilient::ReadPolicy::default()
            },
            |_, _, _| Ok(()),
            |_, _| true,
        )
        .unwrap();
        use mr_io::SectorState;
        assert_eq!(report.map.count(SectorState::Unreadable), 8);
        assert_eq!(report.map.count(SectorState::Sparse), 16 + 32);
        assert!(report
            .map
            .runs()
            .iter()
            .any(|&(s, n, st)| (s, n, st) == (28, 8, SectorState::Unreadable)));
        drop(img);
        for (n, b) in ["d-f001.vmdk", "d-f002.vmdk", "d-s003.vmdk", "d.vmdk"]
            .iter()
            .zip(before)
        {
            assert_eq!(std::fs::read(fx.path(n)).unwrap(), b, "{n}");
        }
        assert!(!fx.path("no-se-abre.vmdk").exists());
    }

    #[test]
    fn only_approved_files_are_opened() {
        let fx = Fixture::new();
        fx.write("e-f001.vmdk", &[7u8; 8 * 512]);
        fx.write("e-f002.vmdk", &[8u8; 8 * 512]);
        fx.write("otro.vmdk", &[9u8; 8 * 512]);
        let text = desc(
            "twoGbMaxExtentFlat",
            &["RW 8 FLAT \"e-f001.vmdk\" 0", "RW 8 FLAT \"e-f002.vmdk\" 0"],
        );
        fx.write("e.vmdk", text.as_bytes());
        // Sin aprobar nada: ExternalReference con los dos nombres, aunque existan al lado.
        match fx.open("e.vmdk", &[], false) {
            Err(IoError::ExternalReference { reference, .. }) => {
                assert!(reference.contains("e-f001.vmdk") && reference.contains("e-f002.vmdk"))
            }
            other => panic!("{other:?}"),
        }
        // Falta una.
        match fx.open("e.vmdk", &[("e-f001.vmdk", "e-f001.vmdk")], false) {
            Err(IoError::ExternalReference { reference, .. }) => {
                assert_eq!(reference, "e-f002.vmdk")
            }
            other => panic!("{other:?}"),
        }
        let invalid = |r: Result<VmdkExtentImage>| matches!(r, Err(IoError::Io(e)) if e.kind() == io::ErrorKind::InvalidInput);
        // Nombre que no coincide exactamente (mayúsculas) o que sobra.
        assert!(invalid(fx.open(
            "e.vmdk",
            &[
                ("E-F001.vmdk", "e-f001.vmdk"),
                ("e-f002.vmdk", "e-f002.vmdk")
            ],
            false
        )));
        assert!(invalid(fx.open(
            "e.vmdk",
            &[
                ("e-f001.vmdk", "e-f001.vmdk"),
                ("e-f002.vmdk", "e-f002.vmdk"),
                ("extra.vmdk", "otro.vmdk")
            ],
            false
        )));
        // El propio descriptor como extensión.
        assert!(invalid(fx.open(
            "e.vmdk",
            &[("e-f001.vmdk", "e.vmdk"), ("e-f002.vmdk", "e-f002.vmdk")],
            false
        )));
        // El usuario puede asignar otro fichero a un nombre: se usa ese, no el del descriptor.
        let img = fx
            .open(
                "e.vmdk",
                &[("e-f001.vmdk", "otro.vmdk"), ("e-f002.vmdk", "e-f002.vmdk")],
                false,
            )
            .unwrap();
        let all = read_all(&img);
        assert!(all[..8 * 512].iter().all(|&v| v == 9));
        assert!(all[8 * 512..].iter().all(|&v| v == 8));
    }

    #[test]
    fn approved_but_unsafe_names_are_still_rejected() {
        let fx = Fixture::new();
        fx.write("x.vmdk", &[1u8; 4096]);
        let text = desc("monolithicFlat", &["RW 8 FLAT \"..\\x.vmdk\" 0"]);
        fx.write("m.vmdk", text.as_bytes());
        assert!(matches!(
            fx.open("m.vmdk", &[("..\\x.vmdk", "x.vmdk")], false),
            Err(IoError::UnsupportedFeature { .. })
        ));
    }

    #[test]
    fn size_checks() {
        let fx = Fixture::new();
        fx.write("s-flat.vmdk", &[1u8; 8 * 512 - 1]);
        fx.write(
            "m.vmdk",
            desc("monolithicFlat", &["RW 8 FLAT \"s-flat.vmdk\" 0"]).as_bytes(),
        );
        assert!(matches!(
            fx.open("m.vmdk", &[("s-flat.vmdk", "s-flat.vmdk")], false),
            Err(IoError::Corrupt { .. })
        ));
        // Con desplazamiento: el fichero debe cubrir desplazamiento + tamaño.
        fx.write("o-flat.vmdk", &[1u8; 8 * 512]);
        fx.write(
            "o.vmdk",
            desc("monolithicFlat", &["RW 8 FLAT \"o-flat.vmdk\" 1"]).as_bytes(),
        );
        assert!(matches!(
            fx.open("o.vmdk", &[("o-flat.vmdk", "o-flat.vmdk")], false),
            Err(IoError::Corrupt { .. })
        ));
        // SPARSE con capacidad distinta de la declarada.
        fx.write("s1.vmdk", &sparse_extent_bytes(64, &[(0, 1)]));
        fx.write(
            "sp.vmdk",
            desc("twoGbMaxExtentSparse", &["RW 48 SPARSE \"s1.vmdk\""]).as_bytes(),
        );
        assert!(matches!(
            fx.open("sp.vmdk", &[("s1.vmdk", "s1.vmdk")], false),
            Err(IoError::Corrupt { .. })
        ));
        // SPARSE que no es una extensión dispersa.
        fx.write(
            "sp2.vmdk",
            desc("twoGbMaxExtentSparse", &["RW 8 SPARSE \"o-flat.vmdk\""]).as_bytes(),
        );
        assert!(matches!(
            fx.open("sp2.vmdk", &[("o-flat.vmdk", "o-flat.vmdk")], false),
            Err(IoError::Corrupt { .. })
        ));
        // Fichero acortado tras abrir: error, nunca datos.
        fx.write("t-flat.vmdk", &[3u8; 8 * 512]);
        fx.write(
            "t.vmdk",
            desc("monolithicFlat", &["RW 8 FLAT \"t-flat.vmdk\" 0"]).as_bytes(),
        );
        let img = fx
            .open("t.vmdk", &[("t-flat.vmdk", "t-flat.vmdk")], false)
            .unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(fx.path("t-flat.vmdk"))
            .unwrap()
            .set_len(512)
            .unwrap();
        assert!(img.read_at(1024, &mut [0u8; 512]).is_err());
    }

    #[test]
    fn not_a_text_descriptor() {
        let fx = Fixture::new();
        fx.write("k.vmdk", &sparse_extent_bytes(64, &[]));
        assert!(matches!(
            fx.open("k.vmdk", &[], false),
            Err(IoError::UnsupportedFeature { .. })
        ));
        let mut big = b"# Disk DescriptorFile\n".to_vec();
        big.resize(MAX_DESCRIPTOR_BYTES + 1, b'#');
        fx.write("big.vmdk", &big);
        assert!(fx.open("big.vmdk", &[], false).is_err());
    }

    /// Descriptores alterados al azar: nunca un pánico, y nunca se abre un fichero no aprobado.
    #[test]
    fn fuzzed_descriptors_never_panic() {
        let fx = Fixture::new();
        fx.write("f-f001.vmdk", &[5u8; 16 * 512]);
        fx.write("f-s002.vmdk", &sparse_extent_bytes(32, &[(1, 9)]));
        let base = desc(
            "twoGbMaxExtentFlat",
            &[
                "RW 16 FLAT \"f-f001.vmdk\" 0",
                "RW 32 SPARSE \"f-s002.vmdk\"",
                "RW 8 ZERO",
                "NOACCESS 8 FLAT \"nada.vmdk\"",
            ],
        );
        let alphabet = b" \"\\/.:\n\r\t0123456789RWFLATSPRZEOND=#\x00\xff";
        let mut seed = 0x1234_5678_9abc_def0u64;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut opened = 0;
        for i in 0..1500 {
            let mut b = base.as_bytes().to_vec();
            for _ in 0..1 + rnd() % 4 {
                let at = (rnd() as usize) % b.len();
                match rnd() % 3 {
                    0 => b[at] = alphabet[(rnd() as usize) % alphabet.len()],
                    1 => {
                        b.remove(at);
                    }
                    _ => b.insert(at, alphabet[(rnd() as usize) % alphabet.len()]),
                }
            }
            let name = format!("fz{i}.vmdk");
            fx.write(&name, &b);
            if let Ok(t) = std::str::from_utf8(&b) {
                let _ = plan_from_text(t);
            }
            let r = fx.open(
                &name,
                &[
                    ("f-f001.vmdk", "f-f001.vmdk"),
                    ("f-s002.vmdk", "f-s002.vmdk"),
                ],
                i % 2 == 0,
            );
            if let Ok(img) = r {
                opened += 1;
                let n = img.size().min(64 * 512) as usize;
                let _ = img.read_at(0, &mut vec![0u8; n]);
                if img.sector_count() > 0 {
                    let _ = img.allocation(0, img.sector_count());
                }
            }
            std::fs::remove_file(fx.path(&name)).unwrap();
        }
        assert!(opened > 0);
        assert!(!fx.path("nada.vmdk").exists());
    }
}
