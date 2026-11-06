use alloc::sync::Arc;

use kernel_memapi::{Guarded, Location, MemoryApi};
use kernel_syscall::tls::TlsLayout;
use kernel_vfs::node::VfsNode;
use x86_64::VirtAddr;

use crate::mcore::mtask::process::Process;
use crate::mcore::mtask::process::elf::{LoadExecutableError, read_exact};
use crate::mem::memapi::{
    LowerHalfAllocOptions, LowerHalfAllocation, LowerHalfMemoryApi, Writable,
};
use crate::{U64Ext, UsizeExt};

/// This type holds the `PT_TLS` image of the executable of a process. Each
/// thread of the process gets a fresh block from it.
#[derive(Clone)]
pub struct TlsTemplate {
    node: VfsNode,
    offset: usize,
    layout: TlsLayout,
}

impl TlsTemplate {
    pub fn new(node: VfsNode, offset: usize, layout: TlsLayout) -> Self {
        Self {
            node,
            offset,
            layout,
        }
    }

    /// This function allocates a TLS block in `process` and fills it from the
    /// image of the executable.
    ///
    /// # Errors
    /// It returns `AllocationFailed` when no memory is available. It also
    /// returns each read error of the executable.
    pub fn instantiate(&self, process: &Arc<Process>) -> Result<TlsBlock, LoadExecutableError> {
        let mut allocation = LowerHalfMemoryApi::new(process.clone())
            .allocate(
                Location::Anywhere,
                self.layout.block(),
                LowerHalfAllocOptions::USER_ACCESSIBLE | LowerHalfAllocOptions::PER_THREAD,
                Guarded::No,
            )
            .ok_or(LoadExecutableError::AllocationFailed)?;
        let start = allocation.start().as_u64().into_usize();
        let tp = self.layout.init(allocation.as_mut(), start, |image| {
            read_exact(&self.node, image, self.offset)
        })?;
        Ok(TlsBlock {
            allocation,
            thread_pointer: VirtAddr::new(tp.into_u64()),
        })
    }
}

/// This type is the TLS block of one thread.
#[derive(Debug)]
pub struct TlsBlock {
    allocation: LowerHalfAllocation<Writable>,
    thread_pointer: VirtAddr,
}

impl TlsBlock {
    /// This function returns the value of `FsBase` for the thread that owns the block.
    pub fn thread_pointer(&self) -> VirtAddr {
        self.thread_pointer
    }

    /// This function returns the first address of the allocation that holds the block.
    pub fn start(&self) -> VirtAddr {
        self.allocation.start()
    }

    /// See [`LowerHalfAllocation::abandon`].
    pub(crate) fn abandon(self) {
        self.allocation.abandon();
    }

    /// This function returns the block at the same address in `process`. A fork
    /// clones that block from this block. It returns `None` when `process`
    /// holds no region there.
    pub(crate) fn counterpart(&self, process: &Arc<Process>) -> Option<Self> {
        Some(Self {
            allocation: self.allocation.counterpart(process)?,
            thread_pointer: self.thread_pointer,
        })
    }
}
