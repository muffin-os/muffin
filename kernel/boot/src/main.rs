#![no_std]
#![no_main]
extern crate alloc;

use alloc::vec;

use kernel::driver::block::BlockDevices;
use kernel::file::ext2::VirtualExt2Fs;
use kernel::file::vfs;
use kernel::hpet::hpet;
use kernel::limine::BASE_REVISION;
use kernel::mcore;
use kernel::mcore::mtask::process::Process;
use kernel_ext2::Ext2Fs;
use kernel_vfs::Stat;
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

    {
        let launch_span = span!(Level::INFO, "launch /bin/init");
        let pid = launch_span.in_scope(|| {
            let init_path = AbsolutePath::try_new("/bin/init").unwrap();
            let _ = vfs().read().open(init_path).expect("should have /bin/init");
            let proc = Process::create_from_executable(Process::root(), init_path).unwrap();
            proc.pid()
        });
        launch_span.record("pid", pid.as_u64());
    }

    {
        info!("starting fbdemo process...");
        let fbdemo_path = AbsolutePath::try_new("/bin/fbdemo").unwrap();
        let _ = vfs()
            .read()
            .open(fbdemo_path)
            .expect("should have /bin/fbdemo");
        let proc = Process::create_from_executable(Process::root(), fbdemo_path).unwrap();
        info!(pid = %proc.pid(), "started process");
    }

    {
        const CHUNK_LEN: usize = 64 * 1024;

        info!("reading large file");
        let large_path = AbsolutePath::try_new("/var/large.bin").unwrap();
        let node = vfs()
            .read()
            .open(large_path)
            .expect("should have /var/large.bin");

        let mut stat = Stat::default();
        node.stat(&mut stat)
            .expect("should be able to stat /var/large.bin");

        let mut buf = vec![0_u8; CHUNK_LEN];
        let start_ns = hpet().read().elapsed_ns();
        let mut total = 0;
        while total < stat.size {
            match node
                .read(&mut buf, total)
                .expect("should be able to read /var/large.bin")
            {
                0 => break,
                n => total += n,
            }
        }
        let elapsed_ns = (hpet().read().elapsed_ns() - start_ns).max(1);

        let rate = (total as f64 / 1024.0 / 1024.0) / (elapsed_ns as f64 / 1e9);
        info!(
            bytes = total,
            elapsed_ms = elapsed_ns / 1_000_000,
            "read large file at {rate:.1} MiB/s",
        );
    }

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
