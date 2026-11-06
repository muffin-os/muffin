//! This module is the device file `/dev/input/eventN`. A read returns only
//! whole [`InputEvent`]s. Each open file has its own [`InputQueue`], and each
//! event of the device goes into each queue. Thus two readers of one node
//! both get all the events.
//!
//! The device interrupt wakes a worker task. The worker moves the device
//! events into the queues and wakes the readers.

use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::Waker;

use kernel_abi::{
    InputDeviceInfo, InputEvent, InputEventKind, InputSupportedCodes, IoctlRequest, S_IFCHR,
    SyncCode,
};
use kernel_devfs::{DevFile, InputQueue};
use kernel_vfs::{
    FdLookup, IoctlError, ReadError, Readiness, Stat, StatError, WaitKey, WaitSource, WriteError,
};
use spin::Mutex;
use virtio_drivers::device::input::{DevIDs, InputEvent as RawInputEvent};
use virtio_drivers::transport::InterruptStatus;

use super::InputDevice;
use crate::mcore::mtask::wait::WaitQueue;
use crate::sync::IrqMutex;

/// This is the maximum number of events in the queue of one open file. At
/// 100 events per second, a reader can stop for 40 seconds before it loses a
/// batch.
const MAX_QUEUED_EVENTS: usize = 4096;

const DEVICE_BATCH_END: u16 = 0;

/// A drain moves at most this many events while it holds the device lock.
const DRAIN_BATCH: usize = 32;

/// This hub holds one input device and the queues of its open files.
pub struct InputHub {
    /// The interrupt handler takes this lock too.
    device: IrqMutex<InputDevice>,
    /// A reader goes away when its open file closes. The next drain then
    /// removes it from this list.
    readers: Mutex<Vec<Weak<InputReader>>>,
    pub(super) pending: AtomicBool,
    pub(super) work_ready: WaitQueue,
}

/// This is the queue of one open file and the tasks that wait for it.
pub struct InputReader {
    events: Mutex<InputQueue>,
    waiters: WaitQueue,
}

impl WaitSource for InputReader {
    fn register(&self, waker: Waker) -> WaitKey {
        self.waiters.register(waker)
    }

    fn unregister(&self, key: WaitKey) {
        self.waiters.unregister(key);
    }
}

impl InputHub {
    /// `pending` starts as `true`, so the worker drains the events that come
    /// before the interrupt is enabled.
    pub fn new(device: InputDevice) -> Self {
        Self {
            device: IrqMutex::new(device),
            readers: Mutex::new(Vec::new()),
            pending: AtomicBool::new(true),
            work_ready: WaitQueue::new(),
        }
    }

    /// Opens the device file. The new file does not get the events that a
    /// previous drain moved into the other queues. The next drain gives the new
    /// file the events that are still in the device queue.
    pub fn open(self: &Arc<Self>) -> InputDevFile {
        let reader = Arc::new(InputReader {
            events: Mutex::new(InputQueue::new(MAX_QUEUED_EVENTS)),
            waiters: WaitQueue::new(),
        });
        self.readers.lock().push(Arc::downgrade(&reader));
        InputDevFile {
            hub: self.clone(),
            reader,
        }
    }

    /// Acknowledges the device interrupt. This runs in the interrupt handler,
    /// so it does not take reader locks.
    pub(super) fn acknowledge(&self) {
        let status = self.device.lock_without_interrupts().ack_interrupt();
        if status.intersects(
            InterruptStatus::QUEUE_INTERRUPT | InterruptStatus::DEVICE_CONFIGURATION_INTERRUPT,
        ) {
            self.pending.store(true, Ordering::Release);
            self.work_ready.wake_all();
        }
    }

    pub(super) fn drain(&self) {
        loop {
            let mut batch = [InputEvent::default(); DRAIN_BATCH];
            let mut len = 0;
            let mut is_empty = false;
            {
                let mut device = self.device.lock_without_interrupts();
                while len < DRAIN_BATCH {
                    let Some(raw) = device.pop_pending_event() else {
                        is_empty = true;
                        break;
                    };
                    if let Some(event) = abi_event(raw) {
                        batch[len] = event;
                        len += 1;
                    }
                }
            }
            if len > 0 {
                self.deliver(&batch[..len]);
            }
            if is_empty {
                return;
            }
        }
    }

    fn deliver(&self, events: &[InputEvent]) {
        let live = {
            let mut readers = self.readers.lock();
            let mut live = Vec::with_capacity(readers.len());
            readers.retain(|reader| {
                let Some(reader) = reader.upgrade() else {
                    return false;
                };
                live.push(reader);
                true
            });
            live
        };
        for reader in live {
            {
                let mut queue = reader.events.lock();
                for &event in events {
                    queue.push(event);
                }
            }
            reader.waiters.wake_all();
        }
    }
}

pub struct InputDevFile {
    hub: Arc<InputHub>,
    reader: Arc<InputReader>,
}

fn decode<const N: usize, T>(
    arg: &[u8],
    from_bytes: impl FnOnce([u8; N]) -> T,
) -> Result<T, IoctlError> {
    let bytes = <[u8; N]>::try_from(arg).map_err(|_| IoctlError::InvalidArgument)?;
    Ok(from_bytes(bytes))
}

fn encode<const N: usize>(arg: &mut [u8], bytes: [u8; N]) -> Result<usize, IoctlError> {
    let out = <&mut [u8; N]>::try_from(arg).map_err(|_| IoctlError::InvalidArgument)?;
    *out = bytes;
    Ok(0)
}

/// The device sends the value as `u32`, and the ABI value is signed. The
/// kernel owns the sync codes of the ABI. Thus the batch end of the device
/// becomes [`SyncCode::BatchEnd`], and each other sync event of the device
/// gives `None`.
fn abi_event(raw: RawInputEvent) -> Option<InputEvent> {
    let value = i32::from_ne_bytes(raw.value.to_ne_bytes());
    if raw.event_type == u16::from(InputEventKind::Sync) {
        return (raw.code == DEVICE_BATCH_END).then(|| InputEvent::sync(SyncCode::BatchEnd, value));
    }
    Some(InputEvent {
        event_type: raw.event_type,
        code: raw.code,
        value,
    })
}

fn device_info(ids: &DevIDs, name: &str) -> InputDeviceInfo {
    let mut info = InputDeviceInfo {
        bustype: ids.bustype,
        vendor: ids.vendor,
        product: ids.product,
        version: ids.version,
        ..InputDeviceInfo::default()
    };
    // QEMU pads the config string with a NUL terminator.
    info.set_name(name.trim_end_matches('\0'));
    info
}

impl DevFile for InputDevFile {
    /// Copies the queued events that fit into `buf`. A buffer that is shorter
    /// than one event causes an error. An empty buffer reads nothing. When the
    /// queue is empty, the read blocks.
    fn read(&mut self, buf: &mut [u8], _offset: usize) -> Result<usize, ReadError> {
        if buf.is_empty() {
            return Ok(0);
        }
        if buf.len() < InputEvent::SIZE {
            return Err(ReadError::BufferTooSmall);
        }
        let mut queue = self.reader.events.lock();
        let mut written = 0;
        for slot in buf.as_chunks_mut::<{ InputEvent::SIZE }>().0 {
            let Some(event) = queue.pop() else {
                break;
            };
            slot.copy_from_slice(&event.to_bytes());
            written += InputEvent::SIZE;
        }
        if written == 0 {
            return Err(ReadError::WouldBlock);
        }
        Ok(written)
    }

    fn write(&mut self, _buf: &[u8], _offset: usize) -> Result<usize, WriteError> {
        Err(WriteError::NotWritable)
    }

    /// The file is readable when its queue holds an event.
    fn readiness(&mut self) -> Readiness {
        Readiness {
            readable: !self.reader.events.lock().is_empty(),
            writable: false,
            hung_up: false,
        }
    }

    fn stat(&mut self, stat: &mut Stat) -> Result<(), StatError> {
        stat.size = 0;
        stat.mode = S_IFCHR | 0o444;
        Ok(())
    }

    fn ioctl(
        &mut self,
        request: IoctlRequest,
        arg: &mut [u8],
        _lookup: &dyn FdLookup,
    ) -> Result<usize, IoctlError> {
        match request {
            IoctlRequest::InputGetInfo => {
                let mut device = self.hub.device.lock_without_interrupts();
                let ids = device.ids().map_err(|_| IoctlError::InvalidArgument)?;
                let name = device.name().map_err(|_| IoctlError::InvalidArgument)?;
                drop(device);
                encode(arg, device_info(&ids, &name).to_bytes())
            }
            IoctlRequest::InputGetSupportedCodes => {
                let mut codes = decode(arg, InputSupportedCodes::from_bytes)?;
                let event_type =
                    u8::try_from(codes.event_type).map_err(|_| IoctlError::InvalidArgument)?;
                let supported = self
                    .hub
                    .device
                    .lock_without_interrupts()
                    .ev_bits(event_type)
                    .map_err(|_| IoctlError::InvalidArgument)?;
                codes.set_bits(&supported);
                encode(arg, codes.to_bytes())
            }
            IoctlRequest::FbGetScreenInfo
            | IoctlRequest::GpuGetInfo
            | IoctlRequest::GpuImport
            | IoctlRequest::GpuTransfer
            | IoctlRequest::GpuFenceInfo
            | IoctlRequest::GpuWaitFence
            | IoctlRequest::GpuSetScanout
            | IoctlRequest::GpuFlush => Err(IoctlError::NotSupported),
        }
    }

    fn wait_source(&mut self) -> Option<Arc<dyn WaitSource>> {
        Some(self.reader.clone())
    }
}
