//! Tareas en segundo plano con progreso y cancelación cooperativa.

use crate::{ApiError, Result};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TaskState {
    Running = 1,
    Completed = 2,
    Failed = 3,
    Cancelled = 4,
}

impl TaskState {
    fn from_u8(v: u8) -> Self {
        match v {
            2 => TaskState::Completed,
            3 => TaskState::Failed,
            4 => TaskState::Cancelled,
            _ => TaskState::Running,
        }
    }
}

/// Estado compartido que ve la operación en ejecución.
#[derive(Default)]
pub struct TaskContext {
    cancel: AtomicBool,
    done: AtomicU64,
    total: AtomicU64,
}

impl TaskContext {
    /// Contexto sin tarea asociada (ejecución síncrona y pruebas).
    pub fn detached() -> Self {
        Self::default()
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    pub fn set_total(&self, v: u64) {
        self.total.store(v, Ordering::Relaxed);
    }
    pub fn set_done(&self, v: u64) {
        self.done.store(v, Ordering::Relaxed);
    }
    pub fn done(&self) -> u64 {
        self.done.load(Ordering::Relaxed)
    }
    pub fn total(&self) -> u64 {
        self.total.load(Ordering::Relaxed)
    }
}

/// Tarea con resultado de tipo `T`.
pub struct Task<T> {
    ctx: Arc<TaskContext>,
    state: AtomicU8,
    result: Mutex<Option<Result<T>>>,
    finished: Condvar,
}

impl<T: Send + 'static> Task<T> {
    /// Ejecuta `f` en un hilo nuevo.
    pub fn spawn<F>(f: F) -> Arc<Self>
    where
        F: FnOnce(&TaskContext) -> Result<T> + Send + 'static,
    {
        let task = Arc::new(Self {
            ctx: Arc::new(TaskContext::default()),
            state: AtomicU8::new(TaskState::Running as u8),
            result: Mutex::new(None),
            finished: Condvar::new(),
        });
        let t = Arc::clone(&task);
        thread::Builder::new()
            .name("mr-task".into())
            .spawn(move || {
                let r = f(&t.ctx);
                let st = match &r {
                    Ok(_) => TaskState::Completed,
                    Err(ApiError::Cancelled) => TaskState::Cancelled,
                    Err(_) => TaskState::Failed,
                };
                let mut slot = t.result.lock().unwrap_or_else(|p| p.into_inner());
                *slot = Some(r);
                t.state.store(st as u8, Ordering::Release);
                t.finished.notify_all();
            })
            .expect("no se pudo crear el hilo de la tarea");
        task
    }
}

impl<T> Task<T> {
    pub fn cancel(&self) {
        self.ctx.cancel.store(true, Ordering::Relaxed);
    }

    pub fn state(&self) -> TaskState {
        TaskState::from_u8(self.state.load(Ordering::Acquire))
    }

    /// (hecho, total) en las unidades de la operación (bytes para hash).
    pub fn progress(&self) -> (u64, u64) {
        (self.ctx.done(), self.ctx.total())
    }

    /// Espera al final y extrae el resultado (solo la primera llamada lo obtiene).
    pub fn wait(&self) -> Result<T> {
        let mut slot = self.result.lock().unwrap_or_else(|p| p.into_inner());
        while slot.is_none() {
            slot = self.finished.wait(slot).unwrap_or_else(|p| p.into_inner());
        }
        slot.take()
            .unwrap_or_else(|| Err(ApiError::InvalidArgument("resultado ya consumido".into())))
    }

    /// Extrae el resultado si ya terminó, sin bloquear.
    pub fn try_take(&self) -> Option<Result<T>> {
        if self.state() == TaskState::Running {
            return None;
        }
        self.result.lock().unwrap_or_else(|p| p.into_inner()).take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn cancellation_is_cooperative() {
        let t = Task::spawn(|ctx| {
            ctx.set_total(1_000_000);
            for i in 0..1_000_000u64 {
                if ctx.is_cancelled() {
                    return Err(ApiError::Cancelled);
                }
                ctx.set_done(i);
                thread::sleep(Duration::from_micros(10));
            }
            Ok(())
        });
        thread::sleep(Duration::from_millis(20));
        t.cancel();
        assert!(matches!(t.wait(), Err(ApiError::Cancelled)));
        assert_eq!(t.state(), TaskState::Cancelled);
        assert!(t.progress().0 < 1_000_000);
    }

    #[test]
    fn errors_mark_failed() {
        let t: Arc<Task<()>> = Task::spawn(|_| Err(ApiError::InvalidArgument("x".into())));
        assert!(t.wait().is_err());
        assert_eq!(t.state(), TaskState::Failed);
    }
}
