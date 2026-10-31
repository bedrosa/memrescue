//! Ejecución de una recuperación (ver `docs/formatos/recuperacion.md`).
//!
//! **Orden.** Recorrido en profundidad en el orden estable del árbol virtual: cada directorio se
//! decide antes que su contenido y se crea cuando hace falta (al escribir el primer fichero que
//! lleva dentro, salvo [`RecoverOptions::create_empty_dirs`]); sus fechas se aplican al salir de
//! él (postorden), porque crear un hijo las cambia.
//!
//! **Un fichero.** (1) Estado de los datos frente a [`RecoverOptions::write_states`]; (2) nombre
//! de destino y conflictos **antes** de leer datos; (3) directorios pendientes; (4) salida con
//! `create_output` (temporal), copia del flujo principal por trozos de 1 MiB con hashes y tramos,
//! flujos alternativos (`temporal:flujo` o, sin flujos con nombre, ficheros hermanos tras
//! publicar); (5) publicación sin sustitución (si el nombre apareció entretanto se vuelve a
//! resolver el conflicto); (6) fechas y, al final, solo lectura; (7) `.mrbad` si algún tramo no es
//! fiable; (8) línea del diario.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use mr_common::{HashAlgorithm, MultiHasher};
use mr_vfs::filter::{query, NameRules};
use mr_vfs::{flags, Category, DataState, NodeId, NodeInfo, NodeKind, Vfs};

use crate::dest::{self, Capabilities, CreateError, OutputSink, PendingFile, Root};
use crate::journal::{Journal, JOURNAL_FOLDER, JOURNAL_VERSION};
use crate::json::{array, filetime_iso, iso_utc, string, Obj};
use crate::names::{self, DestName};
use crate::source::{
    CancelFlag, ContentSource, RangeState, SourceError, StreamDesc, StreamReader, COPY_CHUNK,
};
use crate::{
    ConflictAnswer, ConflictDecision, ConflictPolicy, ConflictQuestion, Progress, RecoverError,
    RecoverObserver, RecoverOptions, RecoverSummary, RunStatus, Selection, SourceInfo, SplitPolicy,
    Totals,
};

/// Máximo de tramos no fiables por flujo en el diario (los recuentos son exactos).
pub const MAX_RANGES: usize = mr_fs_ntfs::stream::MAX_REPORTED_RANGES;
/// Reintentos de un fichero cuyo nombre apareció entre la creación y la publicación.
const MAX_PUBLISH_RETRIES: u32 = 3;
const ALGS: [HashAlgorithm; 4] = [
    HashAlgorithm::Crc32,
    HashAlgorithm::Md5,
    HashAlgorithm::Sha1,
    HashAlgorithm::Sha256,
];

/// Fallo durante la copia de un flujo.
#[derive(Debug)]
enum Fail {
    Source(SourceError),
    Dest(String, &'static str),
    Exists(PathBuf),
    Cancelled,
    /// El destino se quedó sin espacio: la recuperación se detiene.
    DiskFull(String),
}

fn is_disk_full(e: &std::io::Error) -> bool {
    // ERROR_HANDLE_DISK_FULL, ERROR_DISK_FULL, ENOSPC.
    matches!(e.raw_os_error(), Some(39 | 112 | 28)) || e.kind() == std::io::ErrorKind::StorageFull
}

fn dest_io(e: std::io::Error, code: &'static str) -> Fail {
    if is_disk_full(&e) {
        Fail::DiskFull(e.to_string())
    } else {
        Fail::Dest(e.to_string(), code)
    }
}

fn create_fail(e: CreateError) -> Fail {
    match e {
        CreateError::Exists(p) => Fail::Exists(p),
        CreateError::Rejected(m) => Fail::Dest(m, "destination_rejected"),
        CreateError::Io(e) => dest_io(e, "destination_io"),
    }
}

/// Hashes de un flujo o de una parte.
type Hashes = Vec<(HashAlgorithm, String)>;
/// Parte de un fichero dividido en el diario: ruta relativa, desplazamiento, longitud y hashes.
type PartOut = (String, u64, u64, Hashes);
/// Parte publicada: ruta, ruta relativa, desplazamiento, longitud y hashes.
type PartDone = (PathBuf, String, u64, u64, Hashes);

/// Tramo no fiable escrito.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Untrusted {
    offset: u64,
    length: u64,
    state: RangeState,
    /// Se escribió el patrón (si no, los bytes del disco).
    filled: bool,
    lba: Option<u64>,
    sectors: Option<u64>,
}

/// Resultado de copiar un flujo.
#[derive(Debug, Default)]
struct StreamOut {
    name: String,
    /// Ruta relativa del destino (fichero, `fichero:flujo` o hermano).
    dest: Option<String>,
    /// `principal`, `flujo` o `fichero_hermano`.
    form: &'static str,
    size: u64,
    written: u64,
    counts: [u64; 8],
    untrusted: Vec<Untrusted>,
    truncated: bool,
    filled: u64,
    hashes: Vec<(HashAlgorithm, String)>,
    /// Partes (división): ruta relativa, desplazamiento, longitud y hashes.
    parts: Vec<PartOut>,
    error: Option<(String, &'static str, String)>,
}

impl StreamOut {
    fn untrusted_bytes(&self) -> u64 {
        RangeState::ALL
            .iter()
            .enumerate()
            .filter(|(_, s)| !s.is_trustworthy())
            .map(|(i, _)| self.counts[i])
            .sum()
    }

    fn add_untrusted(&mut self, u: Untrusted) {
        if let Some(l) = self.untrusted.last_mut() {
            let lba_ok = match (l.lba, l.sectors, u.lba) {
                (None, _, None) => true,
                (Some(a), Some(n), Some(b)) => a + n == b,
                _ => false,
            };
            if l.state == u.state
                && l.filled == u.filled
                && l.offset + l.length == u.offset
                && lba_ok
            {
                l.length += u.length;
                if let (Some(n), Some(m)) = (l.sectors, u.sectors) {
                    l.sectors = Some(n + m);
                }
                return;
            }
        }
        if self.untrusted.len() >= MAX_RANGES {
            self.truncated = true;
        } else {
            self.untrusted.push(u);
        }
    }
}

fn state_index(s: RangeState) -> usize {
    RangeState::ALL.iter().position(|x| *x == s).unwrap_or(0)
}

/// Destino de los bytes de un flujo: un fichero o una serie de partes.
trait ChunkSink {
    fn put(&mut self, data: &[u8]) -> Result<(), Fail>;
}

impl<W: Write + ?Sized> ChunkSink for W {
    fn put(&mut self, data: &[u8]) -> Result<(), Fail> {
        self.write_all(data)
            .map_err(|e| dest_io(e, "destination_write"))
    }
}

/// Partes de un fichero dividido: cada una atómica y con sus hashes.
struct Parts<'s> {
    sink: &'s dyn OutputSink,
    dir: PathBuf,
    rel_dir: String,
    base: String,
    part_size: u64,
    overwrite: bool,
    current: Option<(Box<dyn PendingFile>, MultiHasher, u64, u64)>,
    /// Partes publicadas: ruta, relativa, desplazamiento, longitud, hashes.
    done: Vec<PartDone>,
    offset: u64,
}

impl Parts<'_> {
    fn part_name(base: &str, i: usize) -> String {
        format!("{base}.{:03}", i + 1)
    }

    fn rotate(&mut self) -> Result<(), Fail> {
        if let Some((f, h, start, len)) = self.current.take() {
            let p = f.commit().map_err(create_fail)?;
            let i = self.done.len();
            let rel = join_rel(&self.rel_dir, &Self::part_name(&self.base, i));
            self.done.push((p, rel, start, len, h.finish()));
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<Vec<PartDone>, Fail> {
        if self.current.is_none() && self.done.is_empty() {
            // Flujo vacío: una parte vacía.
            self.open()?;
        }
        self.rotate()?;
        Ok(std::mem::take(&mut self.done))
    }

    fn open(&mut self) -> Result<(), Fail> {
        let name = Self::part_name(&self.base, self.done.len());
        let f = self
            .sink
            .create(&self.dir.join(&name), self.overwrite)
            .map_err(create_fail)?;
        self.current = Some((f, MultiHasher::new(&ALGS), self.offset, 0));
        Ok(())
    }

    /// Borra las partes ya publicadas (cancelación o error: no queda un conjunto incompleto).
    fn discard(&mut self) {
        self.current = None;
        for (p, ..) in self.done.drain(..) {
            let _ = std::fs::remove_file(p);
        }
    }
}

impl ChunkSink for Parts<'_> {
    fn put(&mut self, mut data: &[u8]) -> Result<(), Fail> {
        while !data.is_empty() {
            if self.current.is_none() {
                self.open()?;
            }
            let (f, h, _, len) = self.current.as_mut().expect("parte abierta");
            let room = (self.part_size - *len) as usize;
            let take = room.min(data.len());
            f.write_all(&data[..take])
                .map_err(|e| dest_io(e, "destination_write"))?;
            h.update(&data[..take]);
            *len += take as u64;
            self.offset += take as u64;
            data = &data[take..];
            if *len == self.part_size {
                self.rotate()?;
            }
        }
        Ok(())
    }
}

fn join_rel(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// Estado de un directorio del recorrido.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DirState {
    /// Decidido, aún no creado.
    Pending,
    /// Decidido reutilizar uno existente (fusión), aún no comprobado.
    PendingMerge,
    Ready,
    /// Omitido por conflicto: su contenido se omite.
    Skipped,
    Failed,
}

struct Frame {
    node: Option<NodeId>,
    path: PathBuf,
    rel: String,
    state: DirState,
    names: HashSet<Vec<u16>>,
    /// Línea del diario pendiente (se escribe al crearlo o al salir sin crearlo).
    info: Option<NodeInfo>,
    dest: Option<DestName>,
    conflict: Option<Obj>,
    emitted: bool,
    action: &'static str,
    error: Option<(String, &'static str, String)>,
    streams: Vec<StreamOut>,
}

/// Conflicto resuelto.
enum Target {
    Write {
        name: String,
        overwrite: bool,
        conflict: Option<Obj>,
    },
    Skip(Obj),
    Identical(Obj, PathBuf),
    Cancel,
}

enum DirTarget {
    Create(String, Option<Obj>),
    Merge(String, Option<Obj>),
    Skip(String, Obj),
    Cancel,
}

struct Cancel<'o>(&'o dyn RecoverObserver);
impl CancelFlag for Cancel<'_> {
    fn cancelled(&self) -> bool {
        self.0.cancelled()
    }
}

struct Run<'a> {
    vfs: &'a Vfs,
    content: &'a dyn ContentSource,
    sink: &'a dyn OutputSink,
    source: &'a SourceInfo,
    opts: &'a RecoverOptions,
    obs: &'a dyn RecoverObserver,
    root: Root,
    caps: Capabilities,
    journal: Journal,
    stack: Vec<Frame>,
    totals: Totals,
    progress: Progress,
    file_policy: ConflictPolicy,
    dir_policy: ConflictPolicy,
    stop: Option<RunStatus>,
    reason: Option<String>,
}

fn hashes_obj(scope: &str, h: &[(HashAlgorithm, String)]) -> Obj {
    let mut o = Obj::new().str("ambito", scope);
    for (a, v) in h {
        let k = match a {
            HashAlgorithm::Crc32 => "crc32",
            HashAlgorithm::Md5 => "md5",
            HashAlgorithm::Sha1 => "sha1",
            HashAlgorithm::Sha256 => "sha256",
        };
        o = o.str(k, v);
    }
    o
}

fn times_obj(t: &mr_vfs::Times) -> Obj {
    let f = |o: Obj, k: &str, v: u64| {
        o.num(&format!("{k}_filetime"), v)
            .opt_str(k, filetime_iso(v).as_deref())
    };
    let o = Obj::new();
    let o = f(o, "creacion", t.created);
    let o = f(o, "modificacion", t.modified);
    let o = f(o, "cambio", t.changed);
    f(o, "acceso", t.accessed)
}

fn error_obj(class: &str, code: &str, msg: &str) -> Obj {
    Obj::new()
        .str("clase", class)
        .str("codigo", code)
        .str("mensaje", msg)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn ascii_stamp(secs: u64) -> String {
    let iso = iso_utc(secs);
    // AAAAMMDD-HHMMSS
    format!(
        "{}{}{}-{}{}{}",
        &iso[0..4],
        &iso[5..7],
        &iso[8..10],
        &iso[11..13],
        &iso[14..16],
        &iso[17..19]
    )
}

fn validate(opts: &RecoverOptions, sel: &Selection, vfs: &Vfs) -> Result<(), RecoverError> {
    let f = opts.fill;
    if f.iter().all(|b| *b == f[0]) {
        return Err(RecoverError::InvalidOptions(
            "el patrón de relleno no puede tener sus cuatro bytes iguales (ADR-021)".into(),
        ));
    }
    if opts.conflict == ConflictPolicy::Overwrite && !opts.overwrite_confirmed {
        return Err(RecoverError::InvalidOptions(
            "sustituir ficheros existentes exige una confirmación explícita".into(),
        ));
    }
    if opts.write_states >> 8 != 0 || opts.write_states & 1 != 0 {
        return Err(RecoverError::InvalidOptions(
            "máscara de estados no válida".into(),
        ));
    }
    if let SplitPolicy::Size(n) = opts.split {
        if n < 4096 {
            return Err(RecoverError::InvalidOptions(
                "el tamaño de las partes debe ser de al menos 4096 bytes".into(),
            ));
        }
    }
    if sel.nodes.is_empty() {
        return Err(RecoverError::InvalidOptions(
            "no hay nada seleccionado".into(),
        ));
    }
    for n in &sel.nodes {
        if !vfs.contains(*n) {
            return Err(RecoverError::Vfs(mr_vfs::VfsError::NoSuchNode(n.0)));
        }
    }
    sel.filter.validate()?;
    Ok(())
}

/// Recupera la selección del árbol virtual en el destino (ver el módulo y la documentación del
/// crate). Valida opciones y destino antes de leer nada; un error por fichero (del origen o del
/// destino) se anota en el diario y la recuperación sigue; la cancelación descarta el fichero en
/// curso y cierra el diario con estado `cancelada`.
pub fn recover(
    vfs: &Vfs,
    content: &dyn ContentSource,
    sink: &dyn OutputSink,
    source: &SourceInfo,
    selection: &Selection,
    opts: &RecoverOptions,
    obs: &dyn RecoverObserver,
) -> Result<RecoverSummary, RecoverError> {
    validate(opts, selection, vfs)?;
    let mut filter = selection.filter.clone();
    filter.names = NameRules::parse(&selection.masks)?;
    let root = dest::prepare_root(&opts.destination, sink)?;
    let mut caps = opts.capabilities.unwrap_or_else(|| dest::probe(&root));

    // ---- Plan: nodos de cada selección en orden, sin solapes ----
    let mut seen: HashSet<NodeId> = HashSet::new();
    let mut bases: Vec<NodeId> = Vec::new();
    let mut sorted = selection.nodes.clone();
    sorted.sort_unstable();
    sorted.dedup();
    for n in &sorted {
        let covered = vfs
            .ancestry(*n)
            .iter()
            .any(|a| *a != *n && seen.contains(a))
            || (*n != NodeId::ROOT && seen.contains(&NodeId::ROOT));
        if !covered {
            seen.insert(*n);
            bases.push(*n);
        }
    }
    // Orden estable: el de un recorrido del árbol (los nodos de menor ruta primero).
    let order: std::collections::HashMap<NodeId, usize> = {
        let mut m = std::collections::HashMap::new();
        for b in &bases {
            m.insert(*b, 0usize);
        }
        if bases.len() > 1 {
            for (i, id) in vfs.preorder(NodeId::ROOT).into_iter().enumerate() {
                if let Some(v) = m.get_mut(&id) {
                    *v = i;
                }
            }
        }
        m
    };
    bases.sort_by_key(|b| order[b]);
    let mut plans: Vec<(NodeId, Vec<NodeId>)> = Vec::new();
    let (mut files, mut dirs, mut bytes) = (0u64, 0u64, 0u64);
    for b in &bases {
        let mut items = Vec::new();
        let kind = vfs.kind(*b).unwrap_or(NodeKind::File);
        if *b != NodeId::ROOT {
            let name = vfs.name(*b);
            if kind == NodeKind::File && !filter.matches_self(vfs, *b, &name) {
                continue;
            }
            items.push(*b);
        }
        if kind.is_container() {
            items.extend(query(vfs, *b, true, &filter)?);
        }
        for i in &items {
            match vfs.node(*i) {
                Some(n) if n.kind == NodeKind::File => {
                    files += 1;
                    if n.data.is_some_and(|d| opts.write_states & d.bit() != 0) {
                        bytes += n.size;
                    }
                }
                Some(_) => dirs += 1,
                None => {}
            }
        }
        plans.push((*b, items));
    }

    // ---- Diario ----
    let started = now_secs();
    static RUNS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let id = format!(
        "{:08x}{:04x}{:04x}",
        (started as u32) ^ std::process::id().rotate_left(16),
        std::process::id() as u16,
        RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) as u16
    );
    let jpath = match &opts.journal {
        Some(p) => {
            mr_io::path_policy::check_lexical(&p.to_string_lossy())
                .map_err(|e| RecoverError::Journal(e.to_string()))?;
            if sink.is_source(p) {
                return Err(RecoverError::Journal(format!(
                    "el diario {} sería el origen: nunca se escribe en él",
                    p.display()
                )));
            }
            p.clone()
        }
        None => {
            let dir = root.canonical.join(JOURNAL_FOLDER);
            let stamp = ascii_stamp(started);
            let mut p = dir.join(format!("recuperacion-{stamp}-{id}.jsonl"));
            let mut k = 2;
            while p.exists() && k < 1000 {
                p = dir.join(format!("recuperacion-{stamp}-{id}-{k}.jsonl"));
                k += 1;
            }
            p
        }
    };
    if jpath.exists() {
        return Err(RecoverError::Journal(format!(
            "el diario {} ya existe: nunca se sustituye",
            jpath.display()
        )));
    }
    let journal = Journal::create(&jpath)
        .map_err(|e| RecoverError::Journal(format!("{}: {e}", jpath.display())))?;
    let jcanon = std::fs::canonicalize(&jpath).unwrap_or_else(|_| jpath.clone());
    if sink.is_source(&jcanon) {
        drop(journal);
        let _ = std::fs::remove_file(&jpath);
        return Err(RecoverError::Journal(
            "el diario sería el origen: nunca se escribe en él".into(),
        ));
    }
    // Primer componente del diario dentro de la raíz: reservado (nada recuperado lo sustituye).
    let journal_rel = jcanon
        .strip_prefix(&root.canonical)
        .ok()
        .and_then(|r| r.components().next())
        .map(|c| c.as_os_str().to_string_lossy().into_owned());

    let mut root_names = HashSet::new();
    if let Some(j) = &journal_rel {
        root_names.insert(names::fold_key(j, caps.case_sensitive));
    }
    // Límite de 4 GiB: solo se sondea si hace falta.
    if opts.capabilities.is_none()
        && opts.split == SplitPolicy::Auto
        && !caps.named_streams
        && plans.iter().any(|(_, items)| {
            items
                .iter()
                .any(|i| vfs.node(*i).is_some_and(|n| n.size > dest::FAT32_MAX_FILE))
        })
    {
        caps.max_file_size = dest::probe_max_file_size(&root);
    }

    let mut run = Run {
        vfs,
        content,
        sink,
        source,
        opts,
        obs,
        root,
        caps,
        journal,
        stack: Vec::new(),
        totals: Totals {
            files_planned: files,
            dirs_planned: dirs,
            ..Totals::default()
        },
        progress: Progress {
            files_total: files,
            bytes_total: bytes,
            ..Progress::default()
        },
        file_policy: opts.conflict,
        dir_policy: opts.conflict,
        stop: None,
        reason: None,
    };
    run.header(started, &id, &bases, &filter, &selection.masks)?;
    obs.progress(&run.progress);

    run.stack.push(Frame {
        node: None,
        path: run.root.canonical.clone(),
        rel: String::new(),
        state: DirState::Ready,
        names: root_names,
        info: None,
        dest: None,
        conflict: None,
        emitted: true,
        action: "raiz",
        error: None,
        streams: Vec::new(),
    });
    'outer: for (base, items) in &plans {
        // El marco raíz representa al padre de la selección.
        run.stack[0].node = if *base == NodeId::ROOT {
            Some(NodeId::ROOT)
        } else {
            vfs.parent(*base)
        };
        for &item in items {
            if run.stop.is_some() {
                break 'outer;
            }
            if obs.cancelled() {
                run.stop = Some(RunStatus::Cancelled);
                break 'outer;
            }
            let parent = vfs.parent(item);
            while run.stack.len() > 1 && run.stack.last().and_then(|f| f.node) != parent {
                run.pop()?;
            }
            let Some(info) = vfs.node(item) else { continue };
            if info.kind.is_container() {
                run.enter_dir(info)?;
            } else {
                run.file(info)?;
            }
        }
        while run.stack.len() > 1 {
            run.pop()?;
        }
    }
    while run.stack.len() > 1 {
        run.pop()?;
    }
    let status = run.stop.unwrap_or(RunStatus::Complete);
    run.finish(status)
}

impl Run<'_> {
    fn line(&mut self, j: String) -> Result<(), RecoverError> {
        self.journal
            .line(&j)
            .map_err(|e| RecoverError::Journal(e.to_string()))
    }

    fn header(
        &mut self,
        started: u64,
        id: &str,
        bases: &[NodeId],
        filter: &mr_vfs::filter::Filter,
        masks: &str,
    ) -> Result<(), RecoverError> {
        let o = self.opts;
        let states: Vec<String> = DataState::ALL
            .iter()
            .filter(|d| o.write_states & d.bit() != 0)
            .map(|d| string(d.code()))
            .collect();
        let cats: Vec<String> = Category::ALL
            .iter()
            .filter(|c| filter.categories & c.bit() != 0)
            .map(|c| string(c.code()))
            .collect();
        let fstates: Vec<String> = DataState::ALL
            .iter()
            .filter(|d| filter.data_states & d.bit() != 0)
            .map(|d| string(d.code()))
            .collect();
        let split = match o.split {
            SplitPolicy::Auto => string("auto"),
            SplitPolicy::Never => string("nunca"),
            SplitPolicy::Size(n) => n.to_string(),
        };
        let sel: Vec<String> = bases
            .iter()
            .map(|b| {
                Obj::new()
                    .num("nodo", u64::from(b.0))
                    .str("ruta", &self.vfs.path(*b))
                    .finish()
            })
            .collect();
        let fill: String = o.fill.iter().map(|b| format!("{b:02X}")).collect();
        let j = Obj::new()
            .str("tipo", "cabecera")
            .num("version", JOURNAL_VERSION)
            .str("motor", mr_common::ENGINE_VERSION)
            .str("id", id)
            .str("inicio", &iso_utc(started))
            .obj(
                "origen",
                Obj::new()
                    .str("ruta", &self.source.path)
                    .str("descripcion", &self.source.description)
                    .num("tamano", self.source.size)
                    .num("tamano_sector", u64::from(self.source.sector_size))
                    .opt_num("lba_volumen", self.source.volume_start_lba)
                    .str("sistema_archivos", self.content.fs_name())
                    .num("tamano_cluster", u64::from(self.content.cluster_size())),
            )
            .obj(
                "destino",
                Obj::new()
                    .str("raiz", &self.root.display)
                    .bool("flujos_con_nombre", self.caps.named_streams)
                    .bool("distingue_mayusculas", self.caps.case_sensitive)
                    .opt_num("tamano_maximo_fichero", self.caps.max_file_size)
                    .str("capacidades", self.caps.source),
            )
            .obj(
                "opciones",
                Obj::new()
                    .str("conflictos", o.conflict.code())
                    .raw("estados_escritos", &array(states))
                    .bool("incluir_bytes_reasignados", o.include_reallocated_bytes)
                    .str("relleno", &fill)
                    .bool("flujos_alternativos", o.alternate_streams)
                    .bool("fechas", o.times)
                    .bool("atributos", o.attributes)
                    .bool("marcar_incompletos", o.mark_incomplete)
                    .raw("division", &split)
                    .num("limite_renombrados", u64::from(o.rename_limit))
                    .bool("crear_directorios_vacios", o.create_empty_dirs),
            )
            .raw("seleccion", &array(sel))
            .obj(
                "filtro",
                Obj::new()
                    .raw("categorias", &array(cats))
                    .raw("estados", &array(fstates))
                    .bool("solo_borrados", filter.deleted_only)
                    .str("reglas", masks)
                    .opt_num("tamano_min", filter.size_min)
                    .opt_num("tamano_max", filter.size_max)
                    .str("campo_fecha", filter.time_field.code())
                    .opt_num("fecha_min", filter.time_min)
                    .opt_num("fecha_max", filter.time_max),
            )
            .obj(
                "plan",
                Obj::new()
                    .num("ficheros", self.totals.files_planned)
                    .num("directorios", self.totals.dirs_planned)
                    .num("bytes", self.progress.bytes_total),
            )
            .finish();
        self.line(j)
    }

    fn finish(mut self, status: RunStatus) -> Result<RecoverSummary, RecoverError> {
        let t = &self.totals;
        let by_state = DataState::ALL
            .iter()
            .fold(Obj::new(), |o, d| o.num(d.code(), t.written(*d)));
        let j = Obj::new()
            .str("tipo", "fin")
            .str("estado", status.code())
            .str("fin", &iso_utc(now_secs()))
            .opt_str("motivo", self.reason.as_deref())
            .obj(
                "totales",
                Obj::new()
                    .num("ficheros_planificados", t.files_planned)
                    .num("directorios_planificados", t.dirs_planned)
                    .num("ficheros_escritos", t.files_written)
                    .num("directorios_creados", t.dirs_created)
                    .num("directorios_fusionados", t.dirs_merged)
                    .num("bytes_escritos", t.bytes_written)
                    .num("bytes_no_fiables", t.untrusted_bytes)
                    .num("bytes_relleno", t.filled_bytes)
                    .obj("escritos_por_estado", by_state)
                    .num("omitidos_por_estado", t.skipped_state)
                    .num("omitidos_por_conflicto", t.skipped_conflict)
                    .num("omitidos_identicos", t.skipped_identical)
                    .num("no_soportados", t.unsupported)
                    .num("errores", t.errors)
                    .num("renombrados", t.renamed)
                    .num("rutas_largas", t.long_paths)
                    .num("marcados_incompletos", t.marked_incomplete)
                    .num("cancelados", t.cancelled_files),
            )
            .num("lineas", self.journal.lines() + 1)
            .finish();
        self.line(j)?;
        self.journal
            .sync()
            .map_err(|e| RecoverError::Journal(e.to_string()))?;
        self.progress.current.clear();
        self.obs.progress(&self.progress);
        Ok(RecoverSummary {
            status,
            reason: self.reason,
            journal: PathBuf::from(dest::display_path(self.journal.path())),
            destination: self.root.display.clone(),
            capabilities: self.caps,
            totals: self.totals,
        })
    }

    fn top(&mut self) -> &mut Frame {
        self.stack.last_mut().expect("marco raíz")
    }

    fn long_path(&self, rel: &str) -> bool {
        self.root.display.encode_utf16().count() + 1 + rel.encode_utf16().count() > 259
    }

    /// Política efectiva ante un objeto existente (pregunta si hace falta).
    fn ask(
        &mut self,
        directory: bool,
        info: &NodeInfo,
        requested: &str,
        existing: &Path,
    ) -> ConflictPolicy {
        let policy = if directory {
            self.dir_policy
        } else {
            self.file_policy
        };
        if policy != ConflictPolicy::Ask {
            return policy;
        }
        let md = std::fs::symlink_metadata(existing).ok();
        let q = ConflictQuestion {
            node: info.id,
            directory,
            deleted: info.has(flags::DELETED),
            requested: requested.to_string(),
            existing: PathBuf::from(dest::display_path(existing)),
            existing_is_dir: md.as_ref().is_some_and(|m| m.is_dir()),
            existing_size: md.as_ref().filter(|m| m.is_file()).map(|m| m.len()),
            new_size: if directory { 0 } else { info.size },
        };
        let ConflictAnswer {
            decision,
            apply_to_all,
        } = self.obs.conflict(&q);
        let p = match decision {
            ConflictDecision::Rename => ConflictPolicy::Rename,
            ConflictDecision::Skip => ConflictPolicy::Skip,
            ConflictDecision::SkipIfIdentical => ConflictPolicy::SkipIfIdentical,
            ConflictDecision::Overwrite => ConflictPolicy::Overwrite,
            ConflictDecision::Cancel => {
                self.stop = Some(RunStatus::Cancelled);
                return ConflictPolicy::Skip;
            }
        };
        if apply_to_all {
            if directory {
                self.dir_policy = p;
            } else {
                self.file_policy = p;
            }
        }
        p
    }

    fn conflict_obj(policy: ConflictPolicy, requested: &str, reason: &str) -> Obj {
        Obj::new()
            .str("politica", policy.code())
            .str("nombre_pedido", requested)
            .str("motivo", reason)
    }

    // ------------------------------------------------------------------ directorios

    fn enter_dir(&mut self, info: NodeInfo) -> Result<(), RecoverError> {
        let dn = names::dest_name(self.vfs.name_units(info.id));
        let parent_state = self.top().state;
        let parent_path = self.top().path.clone();
        let parent_rel = self.top().rel.clone();
        let (state, name, conflict, action) = match parent_state {
            DirState::Skipped => (
                DirState::Skipped,
                dn.name.clone(),
                None,
                "omitido_directorio",
            ),
            DirState::Failed => (DirState::Failed, dn.name.clone(), None, "error"),
            _ => match self.resolve_dir(&info, &dn) {
                DirTarget::Create(n, c) => (DirState::Pending, n, c, "creado"),
                DirTarget::Merge(n, c) => (DirState::PendingMerge, n, c, "fusionado"),
                DirTarget::Skip(n, c) => (DirState::Skipped, n, Some(c), "omitido_conflicto"),
                DirTarget::Cancel => (DirState::Skipped, dn.name.clone(), None, "cancelado"),
            },
        };
        let path = parent_path.join(&name);
        let rel = join_rel(&parent_rel, &name);
        if conflict.is_some() && state != DirState::Skipped {
            self.totals.renamed += u64::from(name != dn.name);
        }
        self.stack.push(Frame {
            node: Some(info.id),
            path,
            rel,
            state,
            names: HashSet::new(),
            info: Some(info),
            dest: Some(dn),
            conflict,
            emitted: false,
            action,
            error: None,
            streams: Vec::new(),
        });
        let pending = matches!(state, DirState::Pending | DirState::PendingMerge);
        // Un directorio con flujos alternativos tiene datos propios: se crea aunque no reciba
        // ningún fichero.
        let has_streams = || {
            let i = self.stack.last().and_then(|f| f.info.clone());
            self.opts.alternate_streams
                && i.as_ref().is_some_and(|i| {
                    i.kind == NodeKind::Directory
                        && self
                            .content
                            .streams(i)
                            .is_ok_and(|v| v.iter().any(|s| !s.name.is_empty()))
                })
        };
        if pending && (self.opts.create_empty_dirs || has_streams()) {
            let _ = self.ensure_dirs();
        }
        Ok(())
    }

    fn resolve_dir(&mut self, info: &NodeInfo, dn: &DestName) -> DirTarget {
        let deleted = info.has(flags::DELETED);
        let cs = self.caps.case_sensitive;
        let mut conflict: Option<Obj> = None;
        let mut n = 0u32;
        loop {
            if self.stop.is_some() {
                return DirTarget::Cancel;
            }
            if n > self.opts.rename_limit {
                let c = conflict.unwrap_or_else(|| {
                    Self::conflict_obj(self.dir_policy, &dn.name, "existe_en_destino")
                });
                return DirTarget::Skip(
                    dn.name.clone(),
                    c.str("decision", "omitido_limite")
                        .num("intentos", u64::from(n)),
                );
            }
            let cand = if n == 0 {
                dn.name.clone()
            } else {
                names::with_suffix(&dn.name, &names::conflict_suffix(n, deleted), true)
            };
            let key = names::fold_key(&cand, cs);
            let top = self.stack.last().expect("marco");
            if top.names.contains(&key) {
                conflict.get_or_insert_with(|| {
                    Self::conflict_obj(self.dir_policy, &dn.name, "misma_recuperacion")
                });
                n += 1;
                continue;
            }
            let path = top.path.join(&cand);
            let rel = join_rel(&top.rel, &cand);
            let md = std::fs::symlink_metadata(&path).ok();
            let Some(md) = md else {
                self.top().names.insert(key);
                let c = conflict.map(|c| {
                    c.str("decision", "renombrado")
                        .num("intentos", u64::from(n))
                });
                return DirTarget::Create(cand, c);
            };
            let policy = self.ask(true, info, &rel, &path);
            if self.stop.is_some() {
                return DirTarget::Cancel;
            }
            let base = conflict
                .take()
                .unwrap_or_else(|| Self::conflict_obj(policy, &dn.name, "existe_en_destino"));
            let mergeable = md.is_dir() && !dest::is_reparse_point(&path);
            match policy {
                ConflictPolicy::Skip => {
                    self.top().names.insert(key);
                    return DirTarget::Skip(
                        cand,
                        base.str("decision", "omitido")
                            .num("intentos", u64::from(n)),
                    );
                }
                ConflictPolicy::SkipIfIdentical | ConflictPolicy::Overwrite if mergeable => {
                    self.top().names.insert(key);
                    return DirTarget::Merge(
                        cand,
                        Some(
                            base.str("decision", "fusionado")
                                .num("intentos", u64::from(n)),
                        ),
                    );
                }
                _ => {
                    conflict = Some(base);
                    n += 1;
                }
            }
        }
    }

    /// Crea (o comprueba, en una fusión) los directorios pendientes de la pila. `Err` si alguno
    /// está omitido o falló: entonces el fichero no se escribe.
    fn ensure_dirs(&mut self) -> Result<(), (&'static str, String)> {
        for i in 1..self.stack.len() {
            match self.stack[i].state {
                DirState::Ready => continue,
                DirState::Skipped => {
                    return Err(("omitido_directorio", self.stack[i].rel.clone()));
                }
                DirState::Failed => return Err(("error_directorio", self.stack[i].rel.clone())),
                DirState::Pending | DirState::PendingMerge => {}
            }
            let merge = self.stack[i].state == DirState::PendingMerge;
            let path = self.stack[i].path.clone();
            let r = if merge {
                Ok(())
            } else {
                match std::fs::create_dir(&path) {
                    Ok(()) => Ok(()),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err((
                        "destination_exists",
                        format!(
                            "{} apareció entre la decisión y la creación",
                            dest::display_path(&path)
                        ),
                    )),
                    Err(e) => Err(("destination_io", e.to_string())),
                }
            }
            .and_then(|()| {
                dest::check_contained(&self.root, &path).map_err(|m| ("destination_escape", m))
            });
            match r {
                Ok(()) => {
                    self.stack[i].state = DirState::Ready;
                    if merge {
                        self.totals.dirs_merged += 1;
                    } else {
                        self.totals.dirs_created += 1;
                    }
                    self.dir_streams(i);
                    if let Err(e) = self.emit_dir(i) {
                        self.stop = Some(RunStatus::Failed);
                        self.reason = Some(e.to_string());
                    }
                }
                Err((code, msg)) => {
                    self.stack[i].state = DirState::Failed;
                    self.stack[i].action = "error";
                    self.stack[i].error = Some(("destino".into(), code, msg.clone()));
                    self.totals.errors += 1;
                    self.progress.errors += 1;
                    if let Err(e) = self.emit_dir(i) {
                        self.stop = Some(RunStatus::Failed);
                        self.reason = Some(e.to_string());
                    }
                    return Err(("error_directorio", self.stack[i].rel.clone()));
                }
            }
        }
        Ok(())
    }

    /// Flujos alternativos de un directorio recién creado o fusionado.
    fn dir_streams(&mut self, i: usize) {
        if !self.opts.alternate_streams {
            return;
        }
        let Some(info) = self.stack[i].info.clone() else {
            return;
        };
        if info.kind != NodeKind::Directory || info.origin.is_none() {
            return;
        }
        let Ok(list) = self.content.streams(&info) else {
            return;
        };
        let dir_path = self.stack[i].path.clone();
        let dir_rel = self.stack[i].rel.clone();
        let parent_path = self.stack[i - 1].path.clone();
        let parent_rel = self.stack[i - 1].rel.clone();
        let dir_name = self.stack[i]
            .dest
            .as_ref()
            .map(|d| d.name.clone())
            .unwrap_or_default();
        for s in list.into_iter().filter(|s| !s.name.is_empty()) {
            let out = if self.caps.named_streams {
                let sn = names::stream_name(&s.name).name;
                let target = PathBuf::from(format!("{}:{sn}", dir_path.to_string_lossy()));
                let rel = format!("{dir_rel}:{sn}");
                self.copy_direct(&info, &s, &target, rel, "flujo")
            } else {
                let sib = names::sibling_name(&dir_name, &s.name);
                let target = parent_path.join(&sib);
                let rel = join_rel(&parent_rel, &sib);
                self.copy_sibling(&info, &s, &target, rel)
            };
            self.stack[i].streams.push(out);
        }
    }

    fn emit_dir(&mut self, i: usize) -> Result<(), RecoverError> {
        if self.stack[i].emitted {
            return Ok(());
        }
        self.stack[i].emitted = true;
        let f = &mut self.stack[i];
        let Some(info) = f.info.clone() else {
            return Ok(());
        };
        let streams = std::mem::take(&mut f.streams);
        let conflict = f.conflict.take();
        let error = f.error.take();
        let action = f.action;
        let rel = f.rel.clone();
        let created = matches!(action, "creado" | "fusionado");
        let dn = f.dest.clone();
        let j = self
            .node_obj("directorio", &info, dn.as_ref())
            .opt_str("destino", created.then_some(rel.as_str()))
            .bool("ruta_larga", created && self.long_path(&rel))
            .str("accion", action)
            .opt_obj("conflicto", conflict)
            .raw(
                "flujos",
                &array(streams.iter().map(|s| self.stream_json(s, None))),
            )
            .opt_obj("error", error.map(|(c, code, m)| error_obj(&c, code, &m)))
            .finish();
        self.line(j)
    }

    fn pop(&mut self) -> Result<(), RecoverError> {
        let i = self.stack.len() - 1;
        if !self.stack[i].emitted {
            if self.stack[i].action == "creado" || self.stack[i].action == "fusionado" {
                self.stack[i].action = "no_creado";
            }
            self.emit_dir(i)?;
        }
        let f = self.stack.pop().expect("marco");
        if f.state == DirState::Ready && self.opts.times {
            if let Some(info) = &f.info {
                let t = info.times;
                let _ = dest::set_times(&f.path, t.created, t.modified, t.accessed, true);
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------ diario

    fn node_obj(&self, kind: &str, info: &NodeInfo, dn: Option<&DestName>) -> Obj {
        let units = self.vfs.name_units(info.id);
        let lossy = mr_vfs::name::is_lossy(units);
        let o = info.origin;
        Obj::new()
            .str("tipo", kind)
            .num("nodo", u64::from(info.id.0))
            .str("ruta_virtual", &self.vfs.path(info.id))
            .str("nombre", &names::original_display(units))
            .opt_str(
                "nombre_utf16",
                lossy.then(|| names::units_hex(units)).as_deref(),
            )
            .bool("nombre_cambiado", dn.is_some_and(|d| d.changed))
            .bool("nombre_recortado", dn.is_some_and(|d| d.truncated))
            .str("categoria", info.category.code())
            .bool("borrado", info.has(flags::DELETED))
            .str("origen_nombre", info.name_reliability.code())
            .raw(
                "indicadores",
                &array(mr_vfs::flags::names(info.flags).into_iter().map(string)),
            )
            .opt_num("registro", o.map(|o| o.record))
            .opt_num("secuencia", o.map(|o| u64::from(o.sequence)))
            .num("tamano", info.size)
            .obj("tiempos", times_obj(&info.times))
            .num("atributos", u64::from(info.attributes))
    }

    fn stream_json(&self, s: &StreamOut, state: Option<DataState>) -> String {
        let counts = RangeState::ALL
            .iter()
            .enumerate()
            .fold(Obj::new(), |o, (i, r)| o.num(r.code(), s.counts[i]));
        let base = self.source.volume_start_lba;
        let ranges = s.untrusted.iter().map(|u| {
            Obj::new()
                .num("desplazamiento", u.offset)
                .num("longitud", u.length)
                .str("estado", u.state.code())
                .bool("relleno", u.filled)
                .opt_num("lba", u.lba)
                .opt_num("lba_origen", u.lba.map(|l| l + base.unwrap_or(0)))
                .opt_num("sectores", u.sectors)
                .finish()
        });
        let untrusted = s.untrusted_bytes();
        let clean_state =
            state.is_none_or(|d| matches!(d, DataState::Integro | DataState::IntegroNoVerificable));
        let hashes = if s.hashes.is_empty() {
            Obj::new().str("ambito", "no_calculado")
        } else if untrusted == 0 && clean_state {
            hashes_obj("contenido", &s.hashes)
        } else {
            hashes_obj("escrito_con_relleno", &s.hashes)
        };
        let parts = (!s.parts.is_empty()).then(|| {
            array(s.parts.iter().map(|(rel, off, len, h)| {
                let scope = if untrusted == 0 && clean_state {
                    "contenido"
                } else {
                    "escrito_con_relleno"
                };
                Obj::new()
                    .str("destino", rel)
                    .num("desplazamiento", *off)
                    .num("longitud", *len)
                    .obj("hashes", hashes_obj(scope, h))
                    .finish()
            }))
        });
        let o = Obj::new()
            .str("nombre", &s.name)
            .opt_str("destino", s.dest.as_deref())
            .str("forma", s.form)
            .num("tamano", s.size)
            .num("bytes_escritos", s.written)
            .obj("bytes_por_estado", counts)
            .num("bytes_no_fiables", untrusted)
            .num("bytes_relleno", s.filled)
            .raw("tramos_no_fiables", &array(ranges))
            .bool("tramos_truncados", s.truncated)
            .obj("hashes", hashes);
        let o = match parts {
            Some(p) => o.raw("partes", &p),
            None => o.null("partes"),
        };
        o.opt_obj(
            "error",
            s.error.as_ref().map(|(c, code, m)| error_obj(c, code, m)),
        )
        .finish()
    }

    // ------------------------------------------------------------------ ficheros

    fn file(&mut self, info: NodeInfo) -> Result<(), RecoverError> {
        let dn = names::dest_name(self.vfs.name_units(info.id));
        let state0 = info.data.unwrap_or(DataState::NoDisponible);
        let parent_state = self.top().state;
        let mut ev = FileEvent {
            info,
            dn,
            state0,
            state: state0,
            action: "escrito",
            rel: None,
            conflict: None,
            streams: Vec::new(),
            mrbad: None,
            times_applied: false,
            error: None,
        };
        self.progress.current = join_rel(&self.top().rel, &ev.dn.name);
        match parent_state {
            DirState::Skipped => {
                ev.action = "omitido_directorio";
                self.totals.skipped_conflict += 1;
                return self.file_done(ev);
            }
            DirState::Failed => {
                ev.action = "error";
                ev.error = Some(("destino".into(), "error_directorio", self.top().rel.clone()));
                self.totals.errors += 1;
                return self.file_done(ev);
            }
            _ => {}
        }
        if self.opts.write_states & state0.bit() == 0 {
            ev.action = "omitido_estado";
            self.totals.skipped_state += 1;
            return self.file_done(ev);
        }
        let streams = match self.content.streams(&ev.info) {
            Ok(s) => s,
            Err(e) => return self.source_failed(ev, e),
        };
        let main_size = streams
            .iter()
            .find(|s| s.name.is_empty())
            .map_or(0, |s| s.size);
        let part = match self.opts.split {
            SplitPolicy::Never => None,
            SplitPolicy::Size(n) => (main_size > n).then_some(n),
            SplitPolicy::Auto => self
                .caps
                .max_file_size
                .filter(|m| main_size > *m)
                .map(|m| AUTO_PART.min(m)),
        };
        if part.is_none() {
            if let Some(m) = self.caps.max_file_size.filter(|m| main_size > *m) {
                ev.action = "error";
                ev.error = Some((
                    "destino".into(),
                    "destination_file_too_large",
                    format!(
                        "el flujo principal ({main_size} bytes) no cabe en el destino (máximo {m}) y la división está desactivada"
                    ),
                ));
                self.totals.errors += 1;
                return self.file_done(ev);
            }
        }
        for attempt in 0..=MAX_PUBLISH_RETRIES {
            let has_main = streams.iter().any(|s| s.name.is_empty());
            let target = self.resolve_file(&ev, part.is_some(), has_main);
            match target {
                Target::Cancel => {
                    ev.action = "cancelado";
                    self.totals.cancelled_files += 1;
                    return self.file_done(ev);
                }
                Target::Skip(c) => {
                    ev.action = "omitido_conflicto";
                    ev.conflict = Some(c);
                    self.totals.skipped_conflict += 1;
                    return self.file_done(ev);
                }
                Target::Identical(c, path) => {
                    ev.action = "omitido_identico";
                    ev.conflict = Some(c);
                    let rel = join_rel(
                        &self.top().rel,
                        &path
                            .file_name()
                            .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
                    );
                    ev.rel = Some(rel);
                    self.totals.skipped_identical += 1;
                    return self.file_done(ev);
                }
                Target::Write {
                    name,
                    overwrite,
                    conflict,
                } => {
                    if let Err((code, msg)) = self.ensure_dirs() {
                        ev.action = if code == "omitido_directorio" {
                            self.totals.skipped_conflict += 1;
                            "omitido_directorio"
                        } else {
                            self.totals.errors += 1;
                            ev.error = Some(("destino".into(), "error_directorio", msg));
                            "error"
                        };
                        return self.file_done(ev);
                    }
                    if self.stop.is_some() {
                        ev.action = "cancelado";
                        return self.file_done(ev);
                    }
                    if conflict.is_some() && name != ev.dn.name {
                        self.totals.renamed += 1;
                    }
                    ev.conflict = conflict;
                    match self.write_file(&mut ev, &streams, &name, overwrite, part) {
                        Ok(()) => {
                            if overwrite {
                                ev.action = "escrito_sustituyendo";
                            }
                            return self.file_done(ev);
                        }
                        Err(Fail::Exists(_)) if attempt < MAX_PUBLISH_RETRIES => {
                            // El nombre apareció entre la decisión y la publicación: se vuelve a
                            // resolver (el nombre ya está reservado; se probará el siguiente).
                            ev.streams.clear();
                            continue;
                        }
                        Err(f) => return self.write_failed(ev, f),
                    }
                }
            }
        }
        unreachable!("el bucle de publicación siempre devuelve")
    }

    fn source_failed(&mut self, mut ev: FileEvent, e: SourceError) -> Result<(), RecoverError> {
        match e {
            SourceError::Unsupported { code, message } => {
                ev.action = "no_soportado";
                ev.error = Some(("origen".into(), code, message));
                self.totals.unsupported += 1;
            }
            SourceError::Cancelled => {
                ev.action = "cancelado";
                self.stop = Some(RunStatus::Cancelled);
                self.totals.cancelled_files += 1;
            }
            other => {
                ev.action = "error";
                ev.error = Some(("origen".into(), other.code(), other.message()));
                self.totals.errors += 1;
            }
        }
        self.file_done(ev)
    }

    fn write_failed(&mut self, mut ev: FileEvent, f: Fail) -> Result<(), RecoverError> {
        ev.rel = None;
        match f {
            Fail::Cancelled => {
                ev.action = "cancelado";
                self.stop = Some(RunStatus::Cancelled);
                self.totals.cancelled_files += 1;
            }
            Fail::Source(e) => return self.source_failed(ev, e),
            Fail::Dest(m, code) => {
                ev.action = "error";
                ev.error = Some(("destino".into(), code, m));
                self.totals.errors += 1;
            }
            Fail::Exists(p) => {
                ev.action = "error";
                ev.error = Some((
                    "destino".into(),
                    "destination_exists",
                    format!(
                        "{} apareció repetidamente durante la escritura",
                        p.display()
                    ),
                ));
                self.totals.errors += 1;
            }
            Fail::DiskFull(m) => {
                ev.action = "error";
                ev.error = Some(("destino".into(), "destination_full", m.clone()));
                self.totals.errors += 1;
                self.stop = Some(RunStatus::Failed);
                self.reason = Some(format!("destino lleno: {m}"));
            }
        }
        self.file_done(ev)
    }

    fn file_done(&mut self, ev: FileEvent) -> Result<(), RecoverError> {
        self.progress.files_done += 1;
        if ev.error.is_some() {
            self.progress.errors = self.totals.errors;
        }
        let rel = ev.rel.clone();
        let streams: Vec<String> = ev
            .streams
            .iter()
            .map(|s| {
                self.stream_json(
                    s,
                    Some(if s.name.is_empty() {
                        ev.state0
                    } else {
                        DataState::Integro
                    }),
                )
            })
            .collect();
        let j = self
            .node_obj("fichero", &ev.info, Some(&ev.dn))
            .opt_str("destino", rel.as_deref())
            .bool(
                "ruta_larga",
                rel.as_deref().is_some_and(|r| self.long_path(r)),
            )
            .str("estado_inicial", ev.state0.code())
            .str("estado", ev.state.code())
            .str("accion", ev.action)
            .opt_obj("conflicto", ev.conflict)
            .raw("flujos", &array(streams))
            .opt_str("marca_incompleto", ev.mrbad.as_deref())
            .bool("fechas_aplicadas", ev.times_applied)
            .opt_obj(
                "error",
                ev.error.map(|(c, code, m)| error_obj(&c, code, &m)),
            )
            .finish();
        self.line(j)?;
        self.obs.progress(&self.progress);
        Ok(())
    }

    /// Nombre del fichero y conflictos (antes de leer datos).
    fn resolve_file(&mut self, ev: &FileEvent, split: bool, has_main: bool) -> Target {
        let deleted = ev.info.has(flags::DELETED);
        let cs = self.caps.case_sensitive;
        let base = ev.dn.name.clone();
        let mut conflict: Option<Obj> = None;
        let mut n = 0u32;
        let mut identity: Option<Option<(u64, String)>> = None;
        loop {
            if self.stop.is_some() {
                return Target::Cancel;
            }
            if n > self.opts.rename_limit {
                let c = conflict.unwrap_or_else(|| {
                    Self::conflict_obj(self.file_policy, &base, "existe_en_destino")
                });
                return Target::Skip(
                    c.str("decision", "omitido_limite")
                        .num("intentos", u64::from(n)),
                );
            }
            let cand = if n == 0 {
                base.clone()
            } else {
                names::with_suffix(&base, &names::conflict_suffix(n, deleted), false)
            };
            let key = names::fold_key(&cand, cs);
            let (dir, dir_rel) = {
                let t = self.stack.last().expect("marco");
                if t.names.contains(&key) {
                    conflict.get_or_insert_with(|| {
                        Self::conflict_obj(self.file_policy, &base, "misma_recuperacion")
                    });
                    n += 1;
                    continue;
                }
                (t.path.clone(), t.rel.clone())
            };
            let path = dir.join(&cand);
            let probe = if split {
                dir.join(format!("{cand}.001"))
            } else {
                path.clone()
            };
            let md = std::fs::symlink_metadata(&probe)
                .ok()
                .or_else(|| std::fs::symlink_metadata(&path).ok());
            let Some(md) = md else {
                self.top().names.insert(key);
                let c = conflict.map(|c| {
                    c.str("decision", "renombrado")
                        .num("intentos", u64::from(n))
                });
                return Target::Write {
                    name: cand,
                    overwrite: false,
                    conflict: c,
                };
            };
            let rel = join_rel(&dir_rel, &cand);
            let policy = self.ask(false, &ev.info, &rel, &probe);
            if self.stop.is_some() {
                return Target::Cancel;
            }
            let c = conflict
                .take()
                .unwrap_or_else(|| Self::conflict_obj(policy, &base, "existe_en_destino"));
            let regular = md.is_file() && !dest::is_reparse_point(&probe);
            match policy {
                ConflictPolicy::Skip => {
                    self.top().names.insert(key);
                    return Target::Skip(
                        c.str("decision", "omitido").num("intentos", u64::from(n)),
                    );
                }
                ConflictPolicy::SkipIfIdentical if regular => {
                    let id = identity
                        .get_or_insert_with(|| self.source_identity(&ev.info, has_main))
                        .clone();
                    let existing = if split {
                        existing_parts_identity(&dir, &cand)
                    } else {
                        file_identity(&path)
                    };
                    if id.is_some() && id == existing {
                        self.top().names.insert(key);
                        return Target::Identical(
                            c.str("decision", "identico").num("intentos", u64::from(n)),
                            path,
                        );
                    }
                    conflict = Some(c);
                    n += 1;
                }
                ConflictPolicy::Overwrite if regular && !split => {
                    self.top().names.insert(key);
                    return Target::Write {
                        name: cand,
                        overwrite: true,
                        conflict: Some(
                            c.str("decision", "sustituido")
                                .num("intentos", u64::from(n)),
                        ),
                    };
                }
                _ => {
                    conflict = Some(c);
                    n += 1;
                }
            }
        }
    }

    /// Tamaño y SHA-256 de lo que se escribiría del flujo principal (para `omitir_si_identico`).
    fn source_identity(&self, info: &NodeInfo, has_main: bool) -> Option<(u64, String)> {
        if !has_main {
            // Sin flujo principal se escribe un fichero vacío.
            let h = MultiHasher::new(&[HashAlgorithm::Sha256]);
            return Some((0, h.finish().pop()?.1));
        }
        let cancel = Cancel(self.obs);
        let r = self.content.open(info, "", &cancel).ok()?;
        let mut h = MultiHasher::new(&[HashAlgorithm::Sha256]);
        let mut off = 0u64;
        while off < r.size() {
            if self.obs.cancelled() {
                return None;
            }
            let mut c = r.read(off, COPY_CHUNK).ok()?;
            if c.data.is_empty() {
                break;
            }
            self.apply_fill(&mut c);
            h.update(&c.data);
            off += c.data.len() as u64;
        }
        Some((off, h.finish().pop()?.1))
    }

    /// Sustituye por el relleno los tramos `possibly_overwritten` (salvo la opción).
    fn apply_fill(&self, c: &mut mr_fs_ntfs::StreamChunk) {
        if self.opts.include_reallocated_bytes {
            return;
        }
        for r in &c.ranges {
            if r.state == RangeState::PossiblyOverwritten {
                let s = (r.offset - c.offset) as usize;
                for i in 0..r.length as usize {
                    c.data[s + i] = self.opts.fill[((r.offset + i as u64) % 4) as usize];
                }
            }
        }
    }

    /// Copia un flujo a `out` con hashes, recuentos por estado y tramos no fiables.
    fn copy(
        &mut self,
        r: &dyn StreamReader,
        out: &mut dyn ChunkSink,
        so: &mut StreamOut,
        main: bool,
    ) -> Result<(), Fail> {
        let mut h = MultiHasher::new(&ALGS);
        so.size = r.size();
        let sector = u64::from(r.sector_size().max(1));
        let mut off = 0u64;
        while off < so.size {
            if self.obs.cancelled() {
                return Err(Fail::Cancelled);
            }
            let mut c = r.read(off, COPY_CHUNK).map_err(Fail::Source)?;
            if c.data.is_empty() {
                break;
            }
            for rg in &c.ranges {
                so.counts[state_index(rg.state)] += rg.length;
                if rg.state.is_trustworthy() {
                    continue;
                }
                let filled = rg.state.is_filled()
                    || (rg.state == RangeState::PossiblyOverwritten
                        && !self.opts.include_reallocated_bytes);
                if filled {
                    so.filled += rg.length;
                }
                if let Some((lba, n)) = rg.lbas {
                    so.add_untrusted(Untrusted {
                        offset: rg.offset,
                        length: rg.length,
                        state: rg.state,
                        filled,
                        lba: Some(lba),
                        sectors: Some(n),
                    });
                    continue;
                }
                // Tramo sin LBA de la lectura: se sitúa por la disposición (puede abarcar varios
                // runs).
                let mut p = rg.offset;
                let end = rg.offset + rg.length;
                while p < end {
                    let (lba, take) = match r.locate(p) {
                        Some((lba, contiguous)) if contiguous > 0 => {
                            (Some(lba), contiguous.min(end - p))
                        }
                        _ => (None, end - p),
                    };
                    so.add_untrusted(Untrusted {
                        offset: p,
                        length: take,
                        state: rg.state,
                        filled,
                        lba,
                        sectors: lba.map(|_| take.div_ceil(sector)),
                    });
                    p += take;
                }
            }
            self.apply_fill(&mut c);
            h.update(&c.data);
            out.put(&c.data)?;
            let n = c.data.len() as u64;
            so.written += n;
            off += n;
            if main {
                self.progress.bytes_done += n;
            }
            self.obs.progress(&self.progress);
        }
        so.hashes = h.finish();
        self.progress.untrusted_bytes += so.untrusted_bytes();
        Ok(())
    }

    /// Escribe el fichero (flujo principal, flujos alternativos, publicación, fechas, marca).
    fn write_file(
        &mut self,
        ev: &mut FileEvent,
        streams: &[StreamDesc],
        name: &str,
        overwrite: bool,
        part: Option<u64>,
    ) -> Result<(), Fail> {
        let cancel = Cancel(self.obs);
        let (dir, dir_rel) = {
            let t = self.stack.last().expect("marco");
            (t.path.clone(), t.rel.clone())
        };
        let path = dir.join(name);
        let rel = join_rel(&dir_rel, name);
        let content = self.content;
        let has_main = streams.iter().any(|s| s.name.is_empty());
        let main_reader = if has_main {
            Some(content.open(&ev.info, "", &cancel).map_err(Fail::Source)?)
        } else {
            None
        };
        let mut main = StreamOut {
            name: String::new(),
            dest: Some(rel.clone()),
            form: "principal",
            ..StreamOut::default()
        };
        let mut ads: Vec<StreamOut> = Vec::new();
        let mut written_paths: Vec<PathBuf> = Vec::new();
        if let Some(p) = part {
            // División: cada parte atómica; los flujos alternativos van como hermanos.
            let mut parts = Parts {
                sink: self.sink,
                dir: dir.clone(),
                rel_dir: dir_rel.clone(),
                base: name.to_string(),
                part_size: p,
                overwrite,
                current: None,
                done: Vec::new(),
                offset: 0,
            };
            let r = match &main_reader {
                Some(r) => self.copy(r.as_ref(), &mut parts, &mut main, true),
                None => Ok(()),
            };
            if let Err(e) = r {
                parts.discard();
                return Err(e);
            }
            let done = match parts.finish() {
                Ok(d) => d,
                Err(e) => {
                    parts.discard();
                    return Err(e);
                }
            };
            main.dest = None;
            main.form = "partes";
            for (p, rel, off, len, h) in done {
                written_paths.push(p);
                main.parts.push((rel, off, len, h));
            }
        } else {
            let mut out = self.sink.create(&path, overwrite).map_err(create_fail)?;
            if let Some(r) = &main_reader {
                self.copy(r.as_ref(), &mut out, &mut main, true)?;
            }
            if self.opts.alternate_streams && self.caps.named_streams {
                out.flush().map_err(|e| dest_io(e, "destination_write"))?;
                for s in streams.iter().filter(|s| !s.name.is_empty()) {
                    let sn = names::stream_name(&s.name).name;
                    let target =
                        PathBuf::from(format!("{}:{sn}", out.temp_path().to_string_lossy()));
                    let so = self.copy_direct(&ev.info, s, &target, format!("{rel}:{sn}"), "flujo");
                    if so.error.as_ref().is_some_and(|e| e.1 == "cancelled") {
                        return Err(Fail::Cancelled);
                    }
                    ads.push(so);
                }
            }
            let p = out.commit().map_err(create_fail)?;
            written_paths.push(p);
        }
        // Publicado: a partir de aquí el fichero existe con su nombre final.
        ev.rel = Some(rel.clone());
        if self.opts.alternate_streams && (!self.caps.named_streams || part.is_some()) {
            for s in streams.iter().filter(|s| !s.name.is_empty()) {
                let sib = names::sibling_name(name, &s.name);
                let so = self.copy_sibling(&ev.info, s, &dir.join(&sib), join_rel(&dir_rel, &sib));
                if let Some(p) = so.dest.as_ref().filter(|_| so.error.is_none()) {
                    let _ = p;
                    written_paths.push(dir.join(&sib));
                }
                ads.push(so);
            }
        }
        // Estado final: se puede rebajar, nunca subir.
        let untrusted: u64 =
            main.untrusted_bytes() + ads.iter().map(StreamOut::untrusted_bytes).sum::<u64>();
        if untrusted > 0
            && matches!(
                ev.state0,
                DataState::Integro | DataState::IntegroNoVerificable
            )
        {
            ev.state = DataState::Parcial;
        }
        self.totals.files_written += 1;
        self.totals.written_by_state[ev.state as usize] += 1;
        self.totals.bytes_written += main.written + ads.iter().map(|s| s.written).sum::<u64>();
        self.totals.untrusted_bytes += untrusted;
        self.totals.filled_bytes += main.filled + ads.iter().map(|s| s.filled).sum::<u64>();
        if self.long_path(&rel) {
            self.totals.long_paths += 1;
        }
        // Marca de incompleto.
        if untrusted > 0 && self.opts.mark_incomplete {
            let base = if part.is_some() {
                format!("{name}.001")
            } else {
                name.to_string()
            };
            let mname = format!("{base}.mrbad");
            let text = self.mrbad_text(ev, &main, &ads);
            match self.sink.create(&dir.join(&mname), false) {
                Ok(mut f) => {
                    let ok = f.write_all(text.as_bytes()).is_ok();
                    if ok && f.commit().is_ok() {
                        ev.mrbad = Some(join_rel(&dir_rel, &mname));
                        self.totals.marked_incomplete += 1;
                    }
                }
                Err(_) => {
                    // Ya existe una marca (de otra ejecución): se anota en el error del flujo.
                    main.error.get_or_insert_with(|| {
                        (
                            "destino".into(),
                            "mrbad_exists",
                            format!("no se pudo crear la marca {mname}: ya existe"),
                        )
                    });
                }
            }
        }
        ev.streams.push(main);
        ev.streams.extend(ads);
        // Fechas y, al final, solo lectura.
        if self.opts.times {
            let t = ev.info.times;
            let mut ok = true;
            for p in &written_paths {
                ok &= dest::set_times(p, t.created, t.modified, t.accessed, false).is_ok();
            }
            ev.times_applied = ok && !written_paths.is_empty();
        }
        if self.opts.attributes && ev.info.attributes & 1 != 0 {
            for p in &written_paths {
                let _ = dest::set_readonly(p);
            }
        }
        Ok(())
    }

    fn mrbad_text(&self, ev: &FileEvent, main: &StreamOut, ads: &[StreamOut]) -> String {
        let mut t = String::new();
        t.push_str("MemRescue: fichero recuperado INCOMPLETO o NO FIABLE.\r\n");
        t.push_str("Los tramos listados NO son el contenido original del fichero.\r\n");
        t.push_str(&format!(
            "Ruta en el origen: {}\r\n",
            self.vfs.path(ev.info.id)
        ));
        t.push_str(&format!(
            "Estado: {} ({})\r\n",
            ev.state.code(),
            ev.state.label()
        ));
        let fill: String = self
            .opts
            .fill
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(" ");
        t.push_str(&format!("Patrón de relleno: {fill}\r\n"));
        for s in std::iter::once(main).chain(ads.iter()) {
            if s.untrusted.is_empty() && !s.truncated {
                continue;
            }
            let label = if s.name.is_empty() {
                "flujo principal".to_string()
            } else {
                format!("flujo «{}»", s.name)
            };
            t.push_str(&format!(
                "{label}: {} bytes no fiables de {}\r\n",
                s.untrusted_bytes(),
                s.size
            ));
            for u in &s.untrusted {
                t.push_str(&format!(
                    "  bytes {}..{} ({} B): {}{}{}\r\n",
                    u.offset,
                    u.offset + u.length,
                    u.length,
                    u.state.label(),
                    if u.filled {
                        ", escrito con el patrón de relleno"
                    } else {
                        ", bytes del disco posiblemente de otro fichero"
                    },
                    match (u.lba, u.sectors) {
                        (Some(l), Some(n)) => format!(
                            ", LBA del volumen {l} ({n} sectores), LBA del origen {}",
                            l + self.source.volume_start_lba.unwrap_or(0)
                        ),
                        _ => String::new(),
                    }
                ));
            }
            if s.truncated {
                t.push_str("  (lista truncada: los recuentos son exactos, ver el diario)\r\n");
            }
        }
        t.push_str(&format!(
            "Diario: {}\r\n",
            dest::display_path(self.journal.path())
        ));
        t
    }

    /// Copia un flujo alternativo directamente a `target` (`fichero:flujo` del temporal o
    /// `directorio:flujo`). Si falla, se borra lo escrito.
    fn copy_direct(
        &mut self,
        info: &NodeInfo,
        s: &StreamDesc,
        target: &Path,
        rel: String,
        form: &'static str,
    ) -> StreamOut {
        let mut so = StreamOut {
            name: s.name.clone(),
            dest: Some(rel),
            form,
            ..StreamOut::default()
        };
        let cancel = Cancel(self.obs);
        let r = match self.content.open(info, &s.name, &cancel) {
            Ok(r) => r,
            Err(e) => {
                so.dest = None;
                so.error = Some(("origen".into(), e.code(), e.message()));
                return so;
            }
        };
        let mut f = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)
        {
            Ok(f) => std::io::BufWriter::new(f),
            Err(e) => {
                so.dest = None;
                so.error = Some(("destino".into(), "destination_stream", e.to_string()));
                return so;
            }
        };
        let res = self
            .copy(r.as_ref(), &mut f, &mut so, false)
            .and_then(|()| f.flush().map_err(|e| dest_io(e, "destination_write")));
        drop(f);
        if let Err(e) = res {
            let _ = std::fs::remove_file(target);
            so.dest = None;
            so.error = Some(match e {
                Fail::Cancelled => ("origen".into(), "cancelled", "operación cancelada".into()),
                Fail::Source(e) => ("origen".into(), e.code(), e.message()),
                Fail::Dest(m, c) => ("destino".into(), c, m),
                Fail::DiskFull(m) => ("destino".into(), "destination_full", m),
                Fail::Exists(p) => (
                    "destino".into(),
                    "destination_exists",
                    p.display().to_string(),
                ),
            });
        }
        so
    }

    /// Flujo alternativo como fichero hermano (destino sin flujos con nombre).
    fn copy_sibling(
        &mut self,
        info: &NodeInfo,
        s: &StreamDesc,
        target: &Path,
        rel: String,
    ) -> StreamOut {
        let mut so = StreamOut {
            name: s.name.clone(),
            dest: Some(rel),
            form: "fichero_hermano",
            ..StreamOut::default()
        };
        let cancel = Cancel(self.obs);
        let r = match self.content.open(info, &s.name, &cancel) {
            Ok(r) => r,
            Err(e) => {
                so.dest = None;
                so.error = Some(("origen".into(), e.code(), e.message()));
                return so;
            }
        };
        let res = self
            .sink
            .create(target, false)
            .map_err(create_fail)
            .and_then(|mut out| {
                self.copy(r.as_ref(), &mut out, &mut so, false)?;
                out.commit().map_err(create_fail)
            });
        if let Err(e) = res {
            so.dest = None;
            so.error = Some(match e {
                Fail::Cancelled => ("origen".into(), "cancelled", "operación cancelada".into()),
                Fail::Source(e) => ("origen".into(), e.code(), e.message()),
                Fail::Dest(m, c) => ("destino".into(), c, m),
                Fail::DiskFull(m) => ("destino".into(), "destination_full", m),
                Fail::Exists(p) => (
                    "destino".into(),
                    "destination_exists",
                    format!("{} ya existe", dest::display_path(&p)),
                ),
            });
        }
        so
    }
}

const AUTO_PART: u64 = crate::AUTO_PART_SIZE;

/// Acontecimiento de un fichero en curso.
struct FileEvent {
    info: NodeInfo,
    dn: DestName,
    state0: DataState,
    state: DataState,
    action: &'static str,
    rel: Option<String>,
    conflict: Option<Obj>,
    streams: Vec<StreamOut>,
    mrbad: Option<String>,
    times_applied: bool,
    error: Option<(String, &'static str, String)>,
}

/// Tamaño y SHA-256 de un fichero existente.
fn file_identity(p: &Path) -> Option<(u64, String)> {
    let mut f = std::fs::File::open(p).ok()?;
    let mut h = MultiHasher::new(&[HashAlgorithm::Sha256]);
    let mut buf = vec![0u8; 1 << 20];
    let mut n = 0u64;
    loop {
        let k = std::io::Read::read(&mut f, &mut buf).ok()?;
        if k == 0 {
            break;
        }
        h.update(&buf[..k]);
        n += k as u64;
    }
    Some((n, h.finish().pop()?.1))
}

/// Tamaño y SHA-256 de la concatenación de las partes `<base>.001`, `.002`...
fn existing_parts_identity(dir: &Path, base: &str) -> Option<(u64, String)> {
    let mut h = MultiHasher::new(&[HashAlgorithm::Sha256]);
    let mut n = 0u64;
    let mut buf = vec![0u8; 1 << 20];
    for i in 1..=999 {
        let p = dir.join(format!("{base}.{i:03}"));
        let Ok(mut f) = std::fs::File::open(&p) else {
            if i == 1 {
                return None;
            }
            break;
        };
        loop {
            let k = std::io::Read::read(&mut f, &mut buf).ok()?;
            if k == 0 {
                break;
            }
            h.update(&buf[..k]);
            n += k as u64;
        }
    }
    Some((n, h.finish().pop()?.1))
}
