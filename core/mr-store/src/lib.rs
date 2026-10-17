//! Base de datos de trabajo de MemRescue (SQLite).
//!
//! Garantías:
//! - Esquema versionado (`PRAGMA user_version`) con migraciones hacia delante.
//! - Modo WAL y `synchronous=FULL`: cada transacción confirmada sobrevive a un corte de energía.
//! - `PRAGMA quick_check` al abrir: una base corrupta se rechaza en lugar de usarse en silencio.
//! - **Propietario único**: un fichero `<base>.lock` con bloqueo exclusivo garantiza que solo un
//!   `Store` (de cualquier proceso) usa la base a la vez. Gracias a ello, al abrir es seguro marcar
//!   como `interrupted` los trabajos que quedaron en `running` (cierre abrupto): nadie más los ejecuta.
//! - Máquina de estados validada y aplicada de forma atómica (`UPDATE … WHERE state = <origen>`
//!   dentro de transacciones `IMMEDIATE`).
//! - Conversiones `u64`↔`i64` comprobadas: un valor fuera de rango es un error, no se satura.

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::fmt;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use thiserror::Error;

pub const SCHEMA_VERSION: i64 = 1;
const SCHEMA_V1: &str = include_str!("schema.sql");

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("SQLite: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("la base de datos tiene la versión de esquema {found}, más nueva que la soportada ({supported})")]
    NewerSchema { found: i64, supported: i64 },
    #[error("transición de estado no permitida: {from} → {to}")]
    InvalidTransition { from: JobState, to: JobState },
    #[error("trabajo inexistente: {0}")]
    NoSuchJob(i64),
    #[error("valor de estado desconocido en la base de datos: {0}")]
    UnknownState(String),
    #[error("la base de datos de trabajo ya está en uso por otra instancia: {0}")]
    InUse(PathBuf),
    #[error("la base de datos está dañada (quick_check): {0}")]
    Corrupt(String),
    #[error("valor fuera de rango: {0}")]
    OutOfRange(String),
    #[error("progreso no válido: {done} de {total}")]
    InvalidProgress { done: u64, total: u64 },
    #[error("E/S del fichero de bloqueo: {0}")]
    Lock(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, StoreError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Pending,
    Running,
    Paused,
    Interrupted,
    Cancelled,
    Failed,
    Completed,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            JobState::Pending => "pending",
            JobState::Running => "running",
            JobState::Paused => "paused",
            JobState::Interrupted => "interrupted",
            JobState::Cancelled => "cancelled",
            JobState::Failed => "failed",
            JobState::Completed => "completed",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            JobState::Cancelled | JobState::Failed | JobState::Completed
        )
    }

    /// Transiciones permitidas.
    pub fn can_transition_to(self, to: JobState) -> bool {
        use JobState::*;
        matches!(
            (self, to),
            (Pending, Running)
                | (Pending, Cancelled)
                | (Running, Paused)
                | (Running, Completed)
                | (Running, Failed)
                | (Running, Cancelled)
                | (Running, Interrupted)
                | (Paused, Running)
                | (Paused, Cancelled)
                | (Interrupted, Running)
                | (Interrupted, Cancelled)
        )
    }
}

impl fmt::Display for JobState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for JobState {
    type Err = StoreError;
    fn from_str(s: &str) -> Result<Self> {
        Ok(match s {
            "pending" => JobState::Pending,
            "running" => JobState::Running,
            "paused" => JobState::Paused,
            "interrupted" => JobState::Interrupted,
            "cancelled" => JobState::Cancelled,
            "failed" => JobState::Failed,
            "completed" => JobState::Completed,
            other => return Err(StoreError::UnknownState(other.to_string())),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: i64,
    pub kind: String,
    pub source_id: Option<i64>,
    pub state: JobState,
    pub params_json: String,
    pub progress_done: u64,
    pub progress_total: u64,
    pub result_json: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    pub seq: i64,
    pub position: u64,
    pub state_json: String,
}

pub struct Store {
    conn: Connection,
    /// Bloqueo exclusivo de propietario; se libera al soltar el `Store`.
    _lock: Option<File>,
}

impl Store {
    /// Abre (o crea) una base de datos en disco con propietario exclusivo, comprueba su
    /// integridad, aplica migraciones y marca los trabajos interrumpidos.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let lock_path = {
            let mut s = path.as_os_str().to_owned();
            s.push(".lock");
            PathBuf::from(s)
        };
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(StoreError::InUse(path.to_path_buf()))
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(StoreError::Lock(e)),
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init(conn, Some(lock))
    }

    /// Base de datos en memoria (pruebas).
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?, None)
    }

    fn init(conn: Connection, lock: Option<File>) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let check: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if check != "ok" {
            return Err(StoreError::Corrupt(check));
        }
        let mut store = Self { conn, _lock: lock };
        store.migrate()?;
        store.mark_interrupted()?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<()> {
        let version: i64 = self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(StoreError::NewerSchema {
                found: version,
                supported: SCHEMA_VERSION,
            });
        }
        if version < 1 {
            let tx = self.conn.transaction()?;
            tx.execute_batch(SCHEMA_V1)?;
            tx.execute(
                "INSERT INTO schema_meta (key, value) VALUES ('created_by', ?1)",
                [concat!("MemRescue ", env!("CARGO_PKG_VERSION"))],
            )?;
            tx.pragma_update(None, "user_version", 1)?;
            tx.commit()?;
        }
        Ok(())
    }

    fn mark_interrupted(&self) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE jobs SET state = 'interrupted', updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE state = 'running'",
            [],
        )?)
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?)
    }

    pub fn add_image_source(
        &self,
        path: &str,
        size: u64,
        sector_size: u32,
        sha256: Option<&str>,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO sources (kind, path, size_bytes, sector_size, sha256) VALUES ('image', ?1, ?2, ?3, ?4)",
            params![path, to_i64(size)?, sector_size, sha256],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn create_job(
        &self,
        kind: &str,
        source_id: Option<i64>,
        params_json: &str,
        total: u64,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO jobs (kind, source_id, state, params_json, progress_total) VALUES (?1, ?2, 'pending', ?3, ?4)",
            params![kind, source_id, params_json, to_i64(total)?],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn job(&self, id: i64) -> Result<Job> {
        self.conn
            .query_row(
                "SELECT id, kind, source_id, state, params_json, progress_done, progress_total, result_json, error
                 FROM jobs WHERE id = ?1",
                [id],
                row_to_job_raw,
            )
            .optional()?
            .ok_or(StoreError::NoSuchJob(id))
            .and_then(JobRaw::into_job)
    }

    pub fn jobs(&self) -> Result<Vec<Job>> {
        let mut st = self.conn.prepare(
            "SELECT id, kind, source_id, state, params_json, progress_done, progress_total, result_json, error
             FROM jobs ORDER BY id",
        )?;
        let rows = st.query_map([], row_to_job_raw)?;
        rows.map(|r| r.map_err(StoreError::from).and_then(JobRaw::into_job))
            .collect()
    }

    /// Cambia el estado de forma atómica validando la transición.
    pub fn set_state(&mut self, id: i64, to: JobState, error: Option<&str>) -> Result<()> {
        self.transition(id, to, error, None)
    }

    /// Transición atómica: lee y escribe dentro de una transacción `IMMEDIATE` y la escritura
    /// solo se aplica si el estado sigue siendo el leído.
    fn transition(
        &mut self,
        id: i64,
        to: JobState,
        error: Option<&str>,
        result: Option<&str>,
    ) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let from: String = tx
            .query_row("SELECT state FROM jobs WHERE id = ?1", [id], |r| r.get(0))
            .optional()?
            .ok_or(StoreError::NoSuchJob(id))?;
        let from: JobState = from.parse()?;
        if !from.can_transition_to(to) {
            return Err(StoreError::InvalidTransition { from, to });
        }
        let n = tx.execute(
            "UPDATE jobs SET state = ?2, error = COALESCE(?3, error), result_json = COALESCE(?4, result_json),
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?1 AND state = ?5",
            params![id, to.as_str(), error, result, from.as_str()],
        )?;
        if n != 1 {
            return Err(StoreError::InvalidTransition { from, to });
        }
        tx.commit()?;
        Ok(())
    }

    pub fn set_progress(&self, id: i64, done: u64, total: u64) -> Result<()> {
        if done > total {
            return Err(StoreError::InvalidProgress { done, total });
        }
        let n = self.conn.execute(
            "UPDATE jobs SET progress_done = ?2, progress_total = ?3,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?1",
            params![id, to_i64(done)?, to_i64(total)?],
        )?;
        if n == 0 {
            return Err(StoreError::NoSuchJob(id));
        }
        Ok(())
    }

    /// Termina el trabajo con éxito guardando el resultado (JSON pequeño) de forma atómica.
    pub fn complete(&mut self, id: i64, result_json: &str) -> Result<()> {
        self.transition(id, JobState::Completed, None, Some(result_json))
    }

    /// Guarda un punto de control y el progreso en una única transacción `IMMEDIATE`.
    /// Solo se admite para trabajos en curso o en pausa.
    pub fn checkpoint(&mut self, id: i64, position: u64, state_json: &str) -> Result<i64> {
        let pos = to_i64(position)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state: String = tx
            .query_row("SELECT state FROM jobs WHERE id = ?1", [id], |r| r.get(0))
            .optional()?
            .ok_or(StoreError::NoSuchJob(id))?;
        let state: JobState = state.parse()?;
        if !matches!(state, JobState::Running | JobState::Paused) {
            return Err(StoreError::InvalidTransition {
                from: state,
                to: state,
            });
        }
        let seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM job_checkpoints WHERE job_id = ?1",
            [id],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO job_checkpoints (job_id, seq, position, state_json) VALUES (?1, ?2, ?3, ?4)",
            params![id, seq, pos, state_json],
        )?;
        tx.execute(
            "UPDATE jobs SET progress_done = ?2, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?1",
            params![id, pos],
        )?;
        tx.commit()?;
        Ok(seq)
    }

    pub fn last_checkpoint(&self, id: i64) -> Result<Option<Checkpoint>> {
        let row: Option<(i64, i64, String)> = self
            .conn
            .query_row(
                "SELECT seq, position, state_json FROM job_checkpoints WHERE job_id = ?1 ORDER BY seq DESC LIMIT 1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        row.map(|(seq, position, state_json)| {
            Ok(Checkpoint {
                seq,
                position: from_i64(position)?,
                state_json,
            })
        })
        .transpose()
    }

    pub fn log_event(&self, id: i64, level: &str, message: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO job_events (job_id, level, message) VALUES (?1, ?2, ?3)",
            params![id, level, message],
        )?;
        Ok(())
    }

    pub fn event_count(&self, id: i64) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM job_events WHERE job_id = ?1",
            [id],
            |r| r.get(0),
        )?)
    }
}

struct JobRaw {
    id: i64,
    kind: String,
    source_id: Option<i64>,
    state: String,
    params_json: String,
    done: i64,
    total: i64,
    result_json: Option<String>,
    error: Option<String>,
}

impl JobRaw {
    fn into_job(self) -> Result<Job> {
        Ok(Job {
            id: self.id,
            kind: self.kind,
            source_id: self.source_id,
            state: self.state.parse()?,
            params_json: self.params_json,
            progress_done: from_i64(self.done)?,
            progress_total: from_i64(self.total)?,
            result_json: self.result_json,
            error: self.error,
        })
    }
}

fn row_to_job_raw(r: &rusqlite::Row<'_>) -> rusqlite::Result<JobRaw> {
    Ok(JobRaw {
        id: r.get(0)?,
        kind: r.get(1)?,
        source_id: r.get(2)?,
        state: r.get(3)?,
        params_json: r.get(4)?,
        done: r.get(5)?,
        total: r.get(6)?,
        result_json: r.get(7)?,
        error: r.get(8)?,
    })
}

// SQLite almacena enteros con signo de 64 bits. Los tamaños reales caben (< 8 EiB); cualquier valor
// fuera de rango indica un error o una base dañada y se devuelve como tal, sin saturar.
fn to_i64(v: u64) -> Result<i64> {
    i64::try_from(v)
        .map_err(|_| StoreError::OutOfRange(format!("{v} no cabe en un entero de SQLite")))
}

fn from_i64(v: i64) -> Result<u64> {
    u64::try_from(v).map_err(|_| StoreError::OutOfRange(format!("valor negativo almacenado: {v}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_schema_v1() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.schema_version().unwrap(), 1);
    }

    #[test]
    fn job_lifecycle_and_invalid_transitions() {
        let mut s = Store::open_in_memory().unwrap();
        let src = s.add_image_source("a.img", 1024, 512, None).unwrap();
        let id = s.create_job("hash", Some(src), "{}", 1024).unwrap();
        assert_eq!(s.job(id).unwrap().state, JobState::Pending);
        assert!(matches!(
            s.set_state(id, JobState::Completed, None),
            Err(StoreError::InvalidTransition { .. })
        ));
        s.set_state(id, JobState::Running, None).unwrap();
        s.set_progress(id, 512, 1024).unwrap();
        s.complete(id, r#"{"sha256":"x"}"#).unwrap();
        let j = s.job(id).unwrap();
        assert_eq!(j.state, JobState::Completed);
        assert_eq!(j.progress_done, 512);
        assert!(j.state.is_terminal());
        assert!(s.set_state(id, JobState::Running, None).is_err());
    }

    #[test]
    fn checkpoints_are_ordered_and_update_progress() {
        let mut s = Store::open_in_memory().unwrap();
        let id = s.create_job("scan", None, "{}", 100).unwrap();
        s.set_state(id, JobState::Running, None).unwrap();
        assert_eq!(s.checkpoint(id, 10, "{}").unwrap(), 1);
        assert_eq!(s.checkpoint(id, 40, r#"{"k":1}"#).unwrap(), 2);
        let c = s.last_checkpoint(id).unwrap().unwrap();
        assert_eq!((c.seq, c.position), (2, 40));
        assert_eq!(s.job(id).unwrap().progress_done, 40);
    }

    #[test]
    fn crash_recovery_marks_running_as_interrupted_and_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("work.mrdb");
        let id;
        {
            let mut s = Store::open(&db).unwrap();
            id = s.create_job("clone", None, "{}", 1000).unwrap();
            s.set_state(id, JobState::Running, None).unwrap();
            s.checkpoint(id, 300, "{}").unwrap();
            // Se "cae" el proceso: se suelta la conexión sin cerrar el trabajo.
        }
        let mut s = Store::open(&db).unwrap();
        let j = s.job(id).unwrap();
        assert_eq!(j.state, JobState::Interrupted);
        assert_eq!(s.last_checkpoint(id).unwrap().unwrap().position, 300);
        s.set_state(id, JobState::Running, None).unwrap();
        assert_eq!(s.schema_version().unwrap(), 1);
    }

    #[test]
    fn rejects_newer_schema() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("future.mrdb");
        {
            let c = Connection::open(&db).unwrap();
            c.pragma_update(None, "user_version", 99).unwrap();
        }
        assert!(matches!(
            Store::open(&db),
            Err(StoreError::NewerSchema { found: 99, .. })
        ));
    }

    #[test]
    fn events_are_recorded() {
        let s = Store::open_in_memory().unwrap();
        let id = s.create_job("hash", None, "{}", 0).unwrap();
        s.log_event(id, "info", "inicio").unwrap();
        s.log_event(id, "warning", "aviso").unwrap();
        assert_eq!(s.event_count(id).unwrap(), 2);
        assert!(s.log_event(id, "fatal", "x").is_err());
    }

    // ---------- Regresiones de la revisión técnica (S1–S6) ----------

    /// S1: con la base en uso, otra instancia no puede abrirla (y por tanto no puede marcar como
    /// interrumpidos los trabajos que la primera está ejecutando).
    #[test]
    fn second_owner_is_refused_and_running_jobs_are_not_hijacked() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("w.mrdb");
        let mut a = Store::open(&db).unwrap();
        let id = a.create_job("scan", None, "{}", 10).unwrap();
        a.set_state(id, JobState::Running, None).unwrap();
        assert!(matches!(Store::open(&db), Err(StoreError::InUse(_))));
        assert_eq!(a.job(id).unwrap().state, JobState::Running);
        drop(a);
        // Liberado el propietario, el trabajo huérfano sí se marca como interrumpido.
        let b = Store::open(&db).unwrap();
        assert_eq!(b.job(id).unwrap().state, JobState::Interrupted);
    }

    /// S2: la transición se aplica de forma condicionada; terminales no se reabren.
    #[test]
    fn terminal_states_are_final_and_complete_requires_running() {
        let mut s = Store::open_in_memory().unwrap();
        let id = s.create_job("hash", None, "{}", 1).unwrap();
        assert!(matches!(
            s.complete(id, "{}"),
            Err(StoreError::InvalidTransition { .. })
        ));
        s.set_state(id, JobState::Running, None).unwrap();
        s.set_state(id, JobState::Cancelled, None).unwrap();
        for to in [
            JobState::Running,
            JobState::Completed,
            JobState::Failed,
            JobState::Paused,
        ] {
            assert!(s.set_state(id, to, None).is_err(), "{to} desde cancelled");
        }
        assert!(matches!(
            s.set_state(999, JobState::Running, None),
            Err(StoreError::NoSuchJob(999))
        ));
    }

    /// S3: no se registran puntos de control de trabajos que no están en marcha.
    #[test]
    fn checkpoints_only_for_running_or_paused_jobs() {
        let mut s = Store::open_in_memory().unwrap();
        let id = s.create_job("scan", None, "{}", 100).unwrap();
        assert!(s.checkpoint(id, 1, "{}").is_err());
        s.set_state(id, JobState::Running, None).unwrap();
        s.set_state(id, JobState::Paused, None).unwrap();
        assert_eq!(s.checkpoint(id, 5, "{}").unwrap(), 1);
        assert!(matches!(
            s.checkpoint(42, 1, "{}"),
            Err(StoreError::NoSuchJob(42))
        ));
    }

    /// S4/S5: durabilidad FULL y comprobación de integridad al abrir.
    #[test]
    fn durability_settings_and_corruption_detection() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("w.mrdb");
        {
            let s = Store::open(&db).unwrap();
            let sync: i64 = s
                .conn
                .pragma_query_value(None, "synchronous", |r| r.get(0))
                .unwrap();
            assert_eq!(sync, 2, "synchronous debe ser FULL (2)");
            let mode: String = s
                .conn
                .pragma_query_value(None, "journal_mode", |r| r.get(0))
                .unwrap();
            assert_eq!(mode, "wal");
            for i in 0..200 {
                s.create_job("x", None, &"{}".repeat(50), i).unwrap();
            }
            s.conn
                .pragma_update(None, "wal_checkpoint", "TRUNCATE")
                .unwrap();
        }
        // Corrompe páginas internas de la base (no la cabecera) y comprueba que se detecta.
        let mut bytes = std::fs::read(&db).unwrap();
        assert!(bytes.len() > 8192);
        for b in &mut bytes[4096..8192] {
            *b = 0xA5;
        }
        std::fs::write(&db, &bytes).unwrap();
        match Store::open(&db) {
            Err(StoreError::Corrupt(_)) | Err(StoreError::Sql(_)) => {}
            other => panic!("una base corrupta no debe abrirse: {:?}", other.err()),
        }
    }

    /// S6: valores fuera de rango y progreso incoherente son errores, no se saturan.
    #[test]
    fn out_of_range_values_are_errors() {
        let mut s = Store::open_in_memory().unwrap();
        assert!(matches!(
            s.create_job("x", None, "{}", u64::MAX),
            Err(StoreError::OutOfRange(_))
        ));
        let id = s.create_job("x", None, "{}", 10).unwrap();
        assert!(matches!(
            s.set_progress(id, 11, 10),
            Err(StoreError::InvalidProgress { .. })
        ));
        s.set_state(id, JobState::Running, None).unwrap();
        assert!(matches!(
            s.checkpoint(id, u64::MAX, "{}"),
            Err(StoreError::OutOfRange(_))
        ));
    }
}
