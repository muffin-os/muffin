//! This module is the device file `/dev/input/eventN`. A read returns only
//! whole [`InputEvent`]s. Each open file has its own [`InputQueue`], and each
//! event of the device goes into each queue. Thus two readers of one node
//! both get all the events.
//!
//! The driver has no interrupt path yet. Thus a read or a readiness check
//! on an open file moves the device events into the queues.

use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;

use kernel_abi::{
    InputDeviceInfo, InputEvent, InputEventKind, InputSupportedCodes, IoctlRequest, S_IFCHR,
    SyncCode,
};
use kernel_devfs::{DevFile, InputQueue};
use kernel_vfs::{FdLookup, IoctlError, ReadError, Readiness, Stat, StatError, WriteError};
use spin::Mutex;
use virtio_drivers::device::input::{DevIDs, InputEvent as RawInputEvent};

use super::InputDevice;

/// This is the maximum number of events in the queue of one open file. At
/// 100 events per second, a reader can stop for 40 seconds before it loses a
/// batch.
const MAX_QUEUED_EVENTS: usize = 4096;

const DEVICE_BATCH_END: u16 = 0;

/// This hub holds one input device and the queues of its open files.
pub struct InputHub {
    device: Mutex<InputDevice>,
    /// A queue goes away when its open file closes. The next drain then removes
    /// it from this list.
    readers: Mutex<Vec<Weak<Mutex<InputQueue>>>>,
}

impl InputHub {
    pub fn new(device: InputDevice) -> Self {
        Self {
            device: Mutex::new(device),
            readers: Mutex::new(Vec::new()),
        }
    }

    /// Opens the device file. The new file does not get the events that a
    /// previous drain moved into the other queues. The next drain gives the new
    /// file the events that are still in the device queue.
    pub fn open(self: &Arc<Self>) -> InputDevFile {
        let queue = Arc::new(Mutex::new(InputQueue::new(MAX_QUEUED_EVENTS)));
        self.readers.lock().push(Arc::downgrade(&queue));
        InputDevFile {
            hub: self.clone(),
            queue,
        }
    }

    fn drain(&self) {
        let mut device = self.device.lock();
        let mut readers = self.readers.lock();
        while let Some(raw) = device.pop_pending_event() {
            let Some(event) = abi_event(raw) else {
                continue;
            };
            readers.retain(|reader| {
                let Some(reader) = reader.upgrade() else {
                    return false;
                };
                reader.lock().push(event);
                true
            });
        }
    }
}

pub struct InputDevFile {
    hub: Arc<InputHub>,
    queue: Arc<Mutex<InputQueue>>,
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
        self.hub.drain();
        let mut queue = self.queue.lock();
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
        self.hub.drain();
        Readiness {
            readable: !self.queue.lock().is_empty(),
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
                let mut device = self.hub.device.lock();
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
                    .lock()
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
}
