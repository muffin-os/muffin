use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;

use conquer_once::spin::OnceCell;
use spin::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::mcore::mtask::process::{Process, ProcessId};
static PROCESS_TREE: OnceCell<RwLock<ProcessTree>> = OnceCell::uninit();

pub fn process_tree() -> &'static RwLock<ProcessTree> {
    PROCESS_TREE.get_or_init(|| {
        RwLock::new(ProcessTree {
            children: BTreeMap::default(),
            processes: BTreeMap::default(),
        })
    })
}

pub struct ProcessTree {
    pub children: BTreeMap<ProcessId, Vec<Arc<Process>>>,
    pub processes: BTreeMap<ProcessId, Arc<Process>>,
}

impl ProcessTree {
    pub fn all(&self) -> impl Iterator<Item = &Arc<Process>> {
        self.processes.values()
    }
}

pub struct Children<'a> {
    guard: RwLockReadGuard<'a, ProcessTree>,
    pid: ProcessId,
}

impl Children<'_> {
    #[must_use]
    pub fn get(&self) -> Option<impl Iterator<Item = &Arc<Process>>> {
        self.guard.children.get(&self.pid).map(|x| x.iter())
    }
}

pub struct ChildrenMut<'a> {
    guard: RwLockWriteGuard<'a, ProcessTree>,
    pid: ProcessId,
}

impl ChildrenMut<'_> {
    pub fn get_mut(&mut self) -> Option<impl Iterator<Item = &mut Arc<Process>>> {
        self.guard.children.get_mut(&self.pid).map(|x| x.iter_mut())
    }

    pub fn insert(&mut self, process: Arc<Process>) {
        self.guard
            .children
            .entry(self.pid)
            .or_default()
            .push(process);
    }
}

impl Process {
    #[allow(clippy::missing_panics_doc)] // this panic must not happen, so the caller shouldn't have to care about it
    pub fn parent(&self) -> Arc<Process> {
        process_tree()
            .read()
            .processes
            .get(&*self.ppid.read())
            .expect("parent process not found")
            .clone()
    }

    pub fn children(&self) -> Children<'_> {
        let guard = process_tree().read();
        Children {
            guard,
            pid: self.pid,
        }
    }

    pub fn children_mut(&self) -> ChildrenMut<'_> {
        let guard = process_tree().write();
        ChildrenMut {
            guard,
            pid: self.pid,
        }
    }
}
