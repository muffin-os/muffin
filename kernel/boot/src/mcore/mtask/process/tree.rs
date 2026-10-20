use alloc::collections::BTreeMap;
use alloc::sync::Arc;

use conquer_once::spin::OnceCell;
use spin::RwLock;

use crate::mcore::mtask::process::{Process, ProcessId};
static PROCESS_TREE: OnceCell<RwLock<ProcessTree>> = OnceCell::uninit();

pub fn process_tree() -> &'static RwLock<ProcessTree> {
    PROCESS_TREE.get_or_init(|| {
        RwLock::new(ProcessTree {
            processes: BTreeMap::default(),
        })
    })
}

pub struct ProcessTree {
    pub processes: BTreeMap<ProcessId, Arc<Process>>,
}

impl ProcessTree {
    pub fn all(&self) -> impl Iterator<Item = &Arc<Process>> {
        self.processes.values()
    }
}
