use std::convert::Infallible;
use std::ffi::c_int;
use std::io;

use kernel_abi::{InputEvent as RawEvent, PollEvents, PollFd};
use muffin_sys::{EINTR, ENOENT, Errno, O_RDONLY};

use crate::{Decoder, DeviceEvent, DeviceId, Event, InputBuffer};

/// One read takes at most this number of events.
const READ_EVENTS: usize = 64;

/// The identity of one open `/dev/input/eventN` node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// `N` of the node. Each [`DeviceEvent`] of this device carries it.
    pub id: DeviceId,
    pub path: String,
    /// The name that the device reports. `None` when the name is not UTF-8.
    pub name: Option<String>,
}

/// The open `/dev/input/eventN` files. Each open file gets every event of
/// its device, also when another process reads the same device.
pub struct InputDevices {
    devices: Vec<Device>,
    /// `poll_fds[i]` is the descriptor of `devices[i]`.
    poll_fds: Vec<PollFd>,
}

struct Device {
    info: DeviceInfo,
    fd: c_int,
    decoder: Decoder,
}

impl Drop for Device {
    fn drop(&mut self) {
        let _ = muffin_sys::close(self.fd);
    }
}

fn errno_error(path: &str, errno: Errno) -> io::Error {
    io::Error::other(format!("{path}: {errno:?}"))
}

impl InputDevices {
    /// Opens `/dev/input/event0`, `event1`, and so on, up to the first node
    /// that does not exist. Returns an error of kind
    /// [`io::ErrorKind::NotFound`] when no node exists.
    pub fn open_all() -> io::Result<Self> {
        let mut devices = Vec::new();
        // devfs cannot list a directory, so a probe open is the only way to
        // find the nodes.
        for index in 0.. {
            let path = format!("/dev/input/event{index}");
            let fd = match muffin_sys::open(&path, O_RDONLY) {
                Ok(fd) => fd,
                Err(ENOENT) => break,
                Err(e) => return Err(errno_error(&path, e)),
            };
            let mut device = Device {
                info: DeviceInfo {
                    id: DeviceId(index),
                    path,
                    name: None,
                },
                fd,
                decoder: Decoder::new(),
            };
            // `device` owns `fd` before the ioctl, so an error closes `fd`.
            let identity =
                muffin_sys::input_device_info(fd).map_err(|e| errno_error(&device.info.path, e))?;
            device.info.name = identity.name().map(str::to_owned);
            devices.push(device);
        }
        if devices.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "no /dev/input/eventN node",
            ));
        }
        let poll_fds = devices
            .iter()
            .map(|device| PollFd::new(device.fd, PollEvents::IN))
            .collect();
        Ok(Self { devices, poll_fds })
    }

    /// Returns the open devices in the sequence of their ids.
    pub fn devices(&self) -> impl Iterator<Item = &DeviceInfo> {
        self.devices.iter().map(|device| &device.info)
    }

    /// Reads the devices on the calling thread and pushes the decoded events
    /// into `buffer`. Each event carries the id of its device. Blocks until
    /// an event arrives, and returns only on an error.
    pub fn fill(&mut self, buffer: &InputBuffer) -> io::Result<Infallible> {
        let mut bytes = [0u8; RawEvent::SIZE * READ_EVENTS];
        let mut decoded: Vec<Event> = Vec::new();
        let mut batch: Vec<DeviceEvent> = Vec::new();
        loop {
            match muffin_sys::poll(&mut self.poll_fds, None) {
                Ok(_) => {}
                // `revents` can hold the result of the previous call.
                Err(EINTR) => continue,
                Err(e) => return Err(errno_error("poll /dev/input", e)),
            }
            for (poll_fd, device) in self.poll_fds.iter().zip(&mut self.devices) {
                if poll_fd
                    .revents
                    .intersects(PollEvents::HUP | PollEvents::NVAL)
                {
                    return Err(io::Error::other(format!(
                        "{}: poll {:?}",
                        device.info.path, poll_fd.revents
                    )));
                }
                if !poll_fd.revents.contains(PollEvents::IN) {
                    continue;
                }
                // Only this file description reads the queue, so a readable
                // queue stays readable and the read does not block.
                let n = match muffin_sys::read(device.fd, &mut bytes) {
                    Ok(n) => n,
                    Err(EINTR) => continue,
                    Err(e) => return Err(errno_error(&device.info.path, e)),
                };
                for raw in bytes[..n].as_chunks::<{ RawEvent::SIZE }>().0 {
                    device
                        .decoder
                        .decode(RawEvent::from_bytes(*raw), &mut decoded);
                }
                let id = device.info.id;
                batch.extend(
                    decoded
                        .drain(..)
                        .map(|event| DeviceEvent { device: id, event }),
                );
            }
            if !batch.is_empty() {
                buffer.extend(batch.drain(..));
            }
        }
    }
}
