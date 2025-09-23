#![no_std]
#![no_main]
extern crate alloc;

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::error::Error;
use core::panic::PanicInfo;

use ext2::Ext2Fs;
use kernel::driver::block::BlockDevices;
use kernel::driver::KernelDeviceId;
use kernel::file::ext2::VirtualExt2Fs;
use kernel::file::vfs;
use kernel::limine::BASE_REVISION;
use kernel::mcore;
use kernel::mcore::mtask::process::Process;
use kernel_device::block::{BlockBuf, BlockDevice};
use kernel_vfs::path::{AbsolutePath, ROOT};
use log::{error, info};
use spin::RwLock;
use x86_64::instructions::hlt;

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
        info!("starting init process...");
        let init_path = AbsolutePath::try_new("/bin/init").unwrap();
        let _ = vfs().read().open(init_path).expect("should have /bin/init");
        let proc = Process::create_from_executable(Process::root(), init_path).unwrap();
        info!("started process pid={}", proc.pid());
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
fn rust_panic(info: &PanicInfo) -> ! {
    handle_panic(info);
    loop {
        hlt();
    }
}

fn handle_panic(info: &PanicInfo) {
    let location = info.location().unwrap();
    error!(
        "kernel panicked at {}:{}:{}:",
        location.file(),
        location.line(),
        location.column(),
    );
    error!("{}", info.message());

    #[cfg(feature = "backtrace")]
    match kernel::backtrace::Backtrace::try_capture() {
        Ok(bt) => {
            error!("stack backtrace:\n{bt}");
        }
        Err(e) => {
            error!("error capturing backtrace: {e:?}");
        }
    }
}
