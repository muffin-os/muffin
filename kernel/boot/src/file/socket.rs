use kernel_unix_socket::{Notify, Registry, UnixSocket};

use crate::mcore::mtask::wait::WaitQueue;

impl Notify for WaitQueue {
    fn notify_all(&self) {
        self.wake_all();
    }
}

pub type Socket = UnixSocket<WaitQueue>;

pub static UNIX_SOCKETS: Registry<WaitQueue> = Registry::new(WaitQueue::new());
