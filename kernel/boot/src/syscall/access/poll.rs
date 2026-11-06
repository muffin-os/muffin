use alloc::sync::Arc;
use alloc::vec::Vec;
use core::time::Duration;

use kernel_abi::{EBADF, EINTR, EIO, Errno, FdNum, PollEvents, PollFd};
use kernel_syscall::access::PollAccess;
use kernel_vfs::{Readiness, WaitSource};

use crate::file::OpenFileDescription;
use crate::hpet::{Instant, hpet};
use crate::mcore::mtask::process::ParkOutcome;
use crate::mcore::mtask::task::Task;
use crate::syscall::access::KernelAccess;

/// The state of a readiness wait after one check.
enum WaitEnd {
    Waiting,
    /// `sys_poll` must check its entries again.
    Settled,
    Interrupted,
    Failed(Errno),
}

fn readiness_of(description: &OpenFileDescription) -> Result<Readiness, Errno> {
    description.readiness().map_err(|_| EIO)
}

impl PollAccess for KernelAccess {
    type Fd = FdNum;

    fn readiness(&self, fd: Self::Fd) -> Result<Readiness, Errno> {
        let description = self
            .process
            .file_descriptors()
            .read()
            .get(&fd)
            .ok_or(EBADF)?
            .file_description()
            .clone();
        readiness_of(&description)
    }

    fn monotonic_now(&self) -> Duration {
        hpet().read().now().since_boot()
    }

    fn wait_for_readiness(&self, fds: &[PollFd], deadline: Option<Duration>) -> Result<(), Errno> {
        let deadline = deadline.map(Instant::from_since_boot);
        // `None` marks a descriptor that is not open. `sys_poll` reports `NVAL` for it.
        let (generation, entries) = {
            let table = self.process.file_descriptors().read();
            let entries: Vec<(PollEvents, Option<Arc<OpenFileDescription>>)> = fds
                .iter()
                .filter(|entry| entry.fd >= 0)
                .map(|entry| {
                    let description = table
                        .get(&FdNum::from(entry.fd))
                        .map(|fd| fd.file_description().clone());
                    (entry.events, description)
                })
                .collect();
            (self.process.fd_generation(), entries)
        };

        let mut sources: Vec<Arc<dyn WaitSource>> = Vec::new();
        for description in entries
            .iter()
            .filter_map(|(_, description)| description.as_ref())
        {
            if let Some(source) = description.wait_source().map_err(|_| EIO)?
                && !sources.iter().any(|known| Arc::ptr_eq(known, &source))
            {
                sources.push(source);
            }
        }
        let mut source_refs: Vec<&dyn WaitSource> = sources.iter().map(|s| &**s).collect();
        if !entries.is_empty() {
            source_refs.push(self.process.fd_changes());
        }

        let task = Task::current();
        let mut end = WaitEnd::Waiting;
        let outcome = self.process.park_current_task(deadline, &source_refs, || {
            end = if self.process.fd_generation() != generation {
                WaitEnd::Settled
            } else {
                check_entries(&entries)
            };
            if matches!(end, WaitEnd::Waiting) {
                if deadline.is_some_and(|deadline| hpet().read().now() >= deadline) {
                    end = WaitEnd::Settled;
                } else if task.has_interrupting_signal() {
                    end = WaitEnd::Interrupted;
                }
            }
            !matches!(end, WaitEnd::Waiting)
        });
        match (outcome, end) {
            (_, WaitEnd::Failed(errno)) => Err(errno),
            (ParkOutcome::Interrupted, _) | (_, WaitEnd::Interrupted) => Err(EINTR),
            (ParkOutcome::Ready, WaitEnd::Waiting | WaitEnd::Settled) => Ok(()),
        }
    }
}

fn check_entries(entries: &[(PollEvents, Option<Arc<OpenFileDescription>>)]) -> WaitEnd {
    for (events, description) in entries {
        let Some(description) = description else {
            return WaitEnd::Settled;
        };
        match readiness_of(description) {
            Ok(readiness) if !readiness.revents(*events).is_empty() => return WaitEnd::Settled,
            Ok(_) => {}
            Err(errno) => return WaitEnd::Failed(errno),
        }
    }
    WaitEnd::Waiting
}
