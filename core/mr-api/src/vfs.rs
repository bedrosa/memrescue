//! Árbol virtual (#25): construcción desde un volumen NTFS como tarea cancelable con progreso y
//! consultas con filtro. El árbol terminado es inmutable y se comparte (`Arc`) entre consultas.

use std::sync::Arc;

use mr_fs_ntfs::NtfsVolume;
use mr_vfs::filter::Filter;
use mr_vfs::{BuildObserver, NodeId, NtfsBuildOptions, Vfs};

use crate::task::{Task, TaskContext};
use crate::{ApiError, Result};

impl BuildObserver for TaskContext {
    fn cancelled(&self) -> bool {
        self.is_cancelled()
    }
    fn progress(&self, done: u64, total: u64) {
        self.set_total(total);
        self.set_done(done);
    }
}

/// Construye el árbol virtual del volumen: vivos, borrados, huérfanos y metadatos con el estado
/// de los datos de cada fichero (ver `mr_vfs::ntfs`). Progreso en milésimas por fase (3000 en
/// total). Al cancelar, [`ApiError::Cancelled`]; un límite de la arena superado,
/// [`ApiError::Vfs`] con `LimitExceeded`.
pub fn vfs_build_ntfs(
    vol: &NtfsVolume,
    opts: &NtfsBuildOptions,
    ctx: &TaskContext,
) -> Result<Arc<Vfs>> {
    mr_vfs::build_ntfs(vol, opts, ctx)?
        .map(Arc::new)
        .ok_or(ApiError::Cancelled)
}

/// Lanza [`vfs_build_ntfs`] en segundo plano.
pub fn start_vfs_build_task(
    vol: Arc<NtfsVolume>,
    opts: NtfsBuildOptions,
) -> Result<Arc<Task<Arc<Vfs>>>> {
    Task::spawn(move |ctx| vfs_build_ntfs(&vol, &opts, ctx))
}

/// Nodos visibles bajo `start` con el filtro (orden determinista; ver `mr_vfs::filter`).
pub fn vfs_query(
    vfs: &Vfs,
    start: NodeId,
    recursive: bool,
    filter: &Filter,
) -> Result<Vec<NodeId>> {
    Ok(mr_vfs::query(vfs, start, recursive, filter)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::TaskState;
    use mr_fs_ntfs::testvol::tree_volume;
    use mr_vfs::{Category, DataState};

    fn volume() -> (tempfile::TempDir, Arc<NtfsVolume>) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("arbol.img");
        std::fs::write(&p, tree_volume(true).image).unwrap();
        let src = crate::open_source(&p, 0).unwrap();
        (dir, Arc::new(crate::ntfs_open(&src).unwrap()))
    }

    #[test]
    fn build_task_reports_progress_and_queries() {
        let (_d, vol) = volume();
        let t = start_vfs_build_task(Arc::clone(&vol), NtfsBuildOptions::default()).unwrap();
        let v = t.wait().unwrap();
        assert_eq!(t.state(), TaskState::Completed);
        assert_eq!(t.progress(), (3000, 3000));
        let n = v.find("/Docs/ñandú.bin").unwrap();
        assert_eq!(v.data(n), Some(DataState::Integro));
        let f = Filter {
            deleted_only: true,
            ..Filter::default()
        };
        let r = vfs_query(&v, NodeId::ROOT, true, &f).unwrap();
        assert!(r.iter().any(|i| v.path(*i) == "/Docs/viejo.txt"));
        assert!(r.iter().all(|i| v.flags(*i) & mr_vfs::flags::DELETED != 0
            || v.kind(*i).is_some_and(|k| k.is_container())));
        assert!(v.stats().category(Category::Orphan) > 0);
        let bad = Filter {
            size_min: Some(2),
            size_max: Some(1),
            ..Filter::default()
        };
        assert!(matches!(
            vfs_query(&v, NodeId::ROOT, true, &bad),
            Err(ApiError::Vfs(_))
        ));
    }

    #[test]
    fn cancelled_build_and_limits() {
        let (_d, vol) = volume();
        let ctx = TaskContext::detached();
        ctx.cancel_for_tests();
        assert!(matches!(
            vfs_build_ntfs(&vol, &NtfsBuildOptions::default(), &ctx),
            Err(ApiError::Cancelled)
        ));
        let opts = NtfsBuildOptions {
            limits: mr_vfs::Limits {
                max_nodes: 4,
                ..mr_vfs::Limits::default()
            },
            ..NtfsBuildOptions::default()
        };
        assert!(matches!(
            vfs_build_ntfs(&vol, &opts, &TaskContext::detached()),
            Err(ApiError::Vfs(mr_vfs::VfsError::LimitExceeded { .. }))
        ));
    }
}
