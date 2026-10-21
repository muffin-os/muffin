#![no_std]
#![no_main]

use kernel::cmdline::cmdline;
use kernel::driver::block::BlockDevices;
use kernel::file::ext2::VirtualExt2Fs;
use kernel::file::vfs;
use kernel::limine::BASE_REVISION;
use kernel::mcore;
use kernel::mcore::mtask::process::Process;
use kernel_ext2::Ext2Fs;
use kernel_vfs::CreateMode;
use kernel_vfs::path::{AbsolutePath, ROOT};
use tracing::{Level, info, span};

#[unsafe(export_name = "kernel_main")]
unsafe extern "C" fn main() -> ! {
    assert!(BASE_REVISION.is_supported());

    kernel::init();

    span!(Level::INFO, "mounting root filesystem").in_scope(|| {
        let root_block_device = BlockDevices::by_id(0).expect("should have block device with id 0");
        vfs()
            .write()
            .mount(
                ROOT,
                VirtualExt2Fs::from(
                    Ext2Fs::try_new(root_block_device).expect("should be able to create ext2fs"),
                ),
            )
            .expect("should be able to mount ext2fs at /");
    });

    span!(Level::INFO, "starting init process").in_scope(|| {
        let init = cmdline()
            .init()
            .expect("should have init argument on cmdline");
        let path = AbsolutePath::try_new(init).expect("init path should be absolute");
        Process::create_from_executable(Process::root(), path)
            .expect("should be able to create process from executable");
    });

    // TODO(tpdenk): launch /bin/fbdemo from init instead of the kernel
    let path = AbsolutePath::try_new("/bin/fbdemo").expect("executable path should be absolute");
    Process::create_from_executable(Process::root(), path)
        .expect("should be able to create process from executable");

    span!(Level::INFO, "ext2 write smoke").in_scope(|| {
        const CHUNK: usize = 4096;
        const TOTAL: usize = 10 * 1024 * 1024;
        let dir = AbsolutePath::try_new("/scratch").expect("path should be absolute");
        info!("creating {dir}");
        vfs()
            .read()
            .create_directory(dir)
            .expect("should be able to create /scratch");
        let path = AbsolutePath::try_new("/scratch/big.bin").expect("path should be absolute");
        info!("opening {path}");
        let node = vfs()
            .read()
            .open(path, CreateMode::IfMissing)
            .expect("should be able to create /scratch/big.bin");
        info!("writing {TOTAL} bytes in {CHUNK} byte chunks");
        let chunk = [0xA5_u8; CHUNK];
        for offset in (0..TOTAL).step_by(CHUNK) {
            let written = node
                .write(chunk, offset)
                .expect("should be able to write a chunk");
            assert_eq!(CHUNK, written);
            if (offset + CHUNK).is_multiple_of(1024 * 1024) {
                info!("{} MiB written", (offset + CHUNK) >> 20);
            }
        }
        info!("syncing");
        node.fsync()
            .expect("should be able to fsync /scratch/big.bin");
        info!("synced");
    });

    mcore::exit_bootstrap()
}

#[panic_handler]
#[cfg(not(test))]
fn rust_panic(info: &core::panic::PanicInfo) -> ! {
    handle_panic(info);
    loop {
        x86_64::instructions::hlt();
    }
}

#[cfg(not(test))]
fn handle_panic(info: &core::panic::PanicInfo) {
    use tracing::error;

    if let Some(location) = info.location() {
        error!(
            "kernel panicked at {}:{}:{}:",
            location.file(),
            location.line(),
            location.column(),
        );
    } else {
        error!("kernel panicked at <unknown location>:");
    }
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
