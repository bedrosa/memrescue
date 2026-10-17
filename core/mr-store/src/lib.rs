//! Base de datos de trabajo de MemRescue (SQLite).
//!
//! Garantías:
//! - Esquema versionado (`PRAGMA user_version`) con migraciones hacia delante.
//! - Modo WAL y `synchronous=NORMAL`: un corte de energía no corrompe la base.
//! - Máquina de estados de trabajos validada en código.
//! - Al abrir, los trabajos que quedaron en `running` (cierre abrupto) pasan a `interrupted`
//!   y pueden reanudarse desde su último punto de control.

use rusqlite::{params, Connection, OptionalExtension};
use std::fmt;
use std::path::Path;
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
}

impl Store {
    /// Abre (o crea) una base de datos en disco, aplica migraciones y marca trabajos interrumpidos.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init(conn)
    }

    /// Base de datos en memoria (pruebas).
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let mut store = Self { conn };
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
            params![path, to_i64(size), sector_size, sha256],
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
            params![kind, source_id, params_json, to_i64(total)],
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

    /// Cambia el estado validando la transición.
    pub fn set_state(&self, id: i64, to: JobState, error: Option<&str>) -> Result<()> {
        let from = self.job(id)?.state;
        if !from.can_transition_to(to) {
            return Err(StoreError::InvalidTransition { from, to });
        }
        self.conn.execute(
            "UPDATE jobs SET state = ?2, error = COALESCE(?3, error),
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?1",
            params![id, to.as_str(), error],
        )?;
        Ok(())
    }

    pub fn set_progress(&self, id: i64, done: u64, total: u64) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE jobs SET progress_done = ?2, progress_total = ?3,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?1",
            params![id, to_i64(done), to_i64(total)],
        )?;
        if n == 0 {
            return Err(StoreError::NoSuchJob(id));
        }
        Ok(())
    }

    /// Termina el trabajo con éxito guardando el resultado (JSON pequeño).
    pub fn complete(&self, id: i64, result_json: &str) -> Result<()> {
        let from = self.job(id)?.state;
        if !from.can_transition_to(JobState::Completed) {
            return Err(StoreError::InvalidTransition {
                from,
                to: JobState::Completed,
            });
        }
        self.conn.execute(
            "UPDATE jobs SET state = 'completed', result_json = ?2,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?1",
            params![id, result_json],
        )?;
        Ok(())
    }

    /// Guarda un punto de control y el progreso en una única transacción.
    pub fn checkpoint(&mut self, id: i64, position: u64, state_json: &str) -> Result<i64> {
        let tx = self.conn.transaction()?;
        let seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM job_checkpoints WHERE job_id = ?1",
            [id],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO job_checkpoints (job_id, seq, position, state_json) VALUES (?1, ?2, ?3, ?4)",
            params![id, seq, to_i64(position), state_json],
        )?;
        tx.execute(
            "UPDATE jobs SET progress_done = ?2, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?1",
            params![id, to_i64(position)],
        )?;
        tx.commit()?;
        Ok(seq)
    }

    pub fn last_checkpoint(&self, id: i64) -> Result<Option<Checkpoint>> {
        Ok(self
            .conn
            .query_row(
                "SELECT seq, position, state_json FROM job_checkpoints WHERE job_id = ?1 ORDER BY seq DESC LIMIT 1",
                [id],
                |r| {
                    Ok(Checkpoint {
                        seq: r.get(0)?,
                        position: from_i64(r.get(1)?),
                        state_json: r.get(2)?,
                    })
                },
            )
            .optional()?)
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
            progress_done: from_i64(self.done),
            progress_total: from_i64(self.total),
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

// SQLite almacena enteros con signo de 64 bits; los tamaños de disco caben de sobra (< 8 EiB).
fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn from_i64(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
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
        let s = Store::open_in_memory().unwrap();
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
        let s = Store::open(&db).unwrap();
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
}
