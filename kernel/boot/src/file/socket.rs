use alloc::sync::Arc;

use kernel_unix_socket::{Notify, Registry, UnixSocket};

use crate::file::OpenFileDescription;
use crate::mcore::mtask::wait::WaitQueue;

impl Notify for WaitQueue {
    fn notify_all(&self) {
        self.wake_all();
    }
}

pub type Socket = UnixSocket<WaitQueue, Arc<OpenFileDescription>>;

pub static UNIX_SOCKETS: Registry<WaitQueue, Arc<OpenFileDescription>> =
    Registry::new(WaitQueue::new());
