//! The device file `/dev/input/eventN`. A read returns whole
//! [`InputEvent`]s and never splits one. Every open file drains the same
//! device queue, so two readers of one node each see a part of the stream.

use alloc::sync::Arc;

use kernel_abi::{InputDeviceInfo, InputEvent, InputSupportedCodes, IoctlRequest, S_IFCHR};
use kernel_devfs::DevFile;
use kernel_vfs::{FdLookup, IoctlError, ReadError, Stat, StatError, WriteError};
use spin::Mutex;
use virtio_drivers::device::input::{DevIDs, InputEvent as RawInputEvent};

use super::InputDevice;

pub struct InputDevFile {
    device: Arc<Mutex<InputDevice>>,
}

impl InputDevFile {
    pub fn new(device: Arc<Mutex<InputDevice>>) -> Self {
        Self { device }
    }
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

/// The device sends the value as `u32`; the evdev value is signed.
fn abi_event(raw: RawInputEvent) -> InputEvent {
    InputEvent {
        event_type: raw.event_type,
        code: raw.code,
        value: i32::from_ne_bytes(raw.value.to_ne_bytes()),
    }
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
    /// Copies the pending events that fit into `buf`. A buffer shorter than
    /// one event is an error, except an empty one, which reads nothing. With
    /// no pending event the read would block.
    fn read(&mut self, buf: &mut [u8], _offset: usize) -> Result<usize, ReadError> {
        if buf.is_empty() {
            return Ok(0);
        }
        if buf.len() < InputEvent::SIZE {
            return Err(ReadError::BufferTooSmall);
        }
        let mut device = self.device.lock();
        let mut written = 0;
        for slot in buf.as_chunks_mut::<{ InputEvent::SIZE }>().0 {
            let Some(event) = device.pop_pending_event().map(abi_event) else {
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
                let mut device = self.device.lock();
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
