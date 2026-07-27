#![no_std]
#![no_main]
extern crate alloc;

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec;
use core::error::Error;

use ext2::Ext2Fs;
use kernel::driver::KernelDeviceId;
use kernel::driver::block::BlockDevices;
use kernel::file::ext2::VirtualExt2Fs;
use kernel::file::vfs;
use kernel::limine::BASE_REVISION;
use kernel::mcore;
use kernel::mcore::mtask::process::Process;
use kernel_device::block::{BlockBuf, BlockDevice};
use kernel_vfs::Stat;
use kernel_vfs::path::{AbsolutePath, ROOT};
use log::info;
use spin::RwLock;

/// Boots the real kernel, mounts the root ext2 filesystem, then spawns one
/// process per line of the `/spawn` manifest baked into the disk image. Each
/// manifest line is an absolute in-OS path spawned in order, so the first
/// entry becomes pid 1. This is the generic test kernel shared by every
/// host-side integration test in `tests/`, replacing the per-suite kernels.
#[unsafe(export_name = "kernel_main")]
unsafe extern "C" fn main() -> ! {
    assert!(BASE_REVISION.is_supported());

    kernel::init();

    {
        info!("mounting root filesystem");
        let root_block_device = BlockDevices::by_id(0).expect("should have block device with id 0");
        let root_block_device = ArcLockedBlockDevice(root_block_device);
        vfs()
            .write()
            .mount(
                ROOT,
                VirtualExt2Fs::from(
                    Ext2Fs::try_new(root_block_device).expect("should be able to create ext2fs"),
                ),
            )
            .expect("should be able to mount ext2fs at /");
    }

    {
        info!("reading spawn manifest");
        let manifest_path = AbsolutePath::try_new("/spawn").expect("should be a valid path");
        let node = vfs()
            .write()
            .open(manifest_path)
            .expect("should be able to open /spawn manifest");
        let stat = {
            let mut stat = Stat::default();
            node.stat(&mut stat)
                .expect("should be able to stat /spawn manifest");
            stat
        };

        let mut buf = vec![0u8; stat.size];
        let mut offset = 0;
        loop {
            let read = node
                .read(&mut buf[offset..], offset)
                .expect("should be able to read /spawn manifest");
            if read == 0 {
                break;
            }
            offset += read;
        }

        let manifest = core::str::from_utf8(&buf).expect("/spawn manifest should be valid UTF-8");
        let mut count = 0;
        for line in manifest.lines() {
            if line.is_empty() {
                continue;
            }
            let path = AbsolutePath::try_new(line).expect("manifest entry should be a valid path");
            let proc = Process::create_from_executable(Process::root(), path)
                .expect("should be able to spawn manifest entry");
            info!("test-kernel: spawned {line} pid={}", proc.pid());
            count += 1;
        }
        info!("test-kernel: spawn complete count={count}");
    }

    mcore::turn_idle()
}

struct ArcLockedBlockDevice<const N: usize>(
    Arc<RwLock<dyn BlockDevice<KernelDeviceId, N> + Send + Sync>>,
);

impl<const N: usize> filesystem::BlockDevice for ArcLockedBlockDevice<N> {
    type Error = Box<dyn Error>;

    fn sector_size(&self) -> usize {
        N
    }

    fn sector_count(&self) -> usize {
        self.0.read().block_count()
    }

    fn read_sector(&self, sector_index: usize, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let mut read_buf = BlockBuf::new();
        self.0.write().read_block(sector_index, &mut read_buf)?;
        buf.copy_from_slice(&read_buf[..]);
        Ok(buf.len())
    }

    fn write_sector(&mut self, sector_index: usize, buf: &[u8]) -> Result<usize, Self::Error> {
        let mut write_buf = BlockBuf::new();
        write_buf.copy_from_slice(buf);
        self.0
            .write()
            .write_block(sector_index, &write_buf)
            .map(|()| buf.len())
    }
}

#[panic_handler]
fn rust_panic(info: &core::panic::PanicInfo) -> ! {
    handle_panic(info);
    loop {
        x86_64::instructions::hlt();
    }
}

fn handle_panic(info: &core::panic::PanicInfo) {
    use log::error;

    let location = info.location().unwrap();
    error!(
        "kernel panicked at {}:{}:{}:",
        location.file(),
        location.line(),
        location.column(),
    );
    error!("{}", info.message());

    match kernel::backtrace::Backtrace::try_capture() {
        Ok(bt) => {
            error!("stack backtrace:\n{bt}");
        }
        Err(e) => {
            error!("error capturing backtrace: {e:?}");
        }
    }
}
