#![no_std]
#![no_main]
extern crate alloc;

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec;
use core::ffi::c_void;
use core::ptr;

use kernel::driver::block::BlockDevices;
use kernel::file::ext2::VirtualExt2Fs;
use kernel::file::vfs;
use kernel::limine::BASE_REVISION;
use kernel::mcore;
use kernel::mcore::mtask::process::{ExitOutcome, ParkOutcome, Process};
use kernel::mcore::mtask::scheduler::global::GlobalTaskQueue;
use kernel::mcore::mtask::task::Task;
use kernel_ext2::Ext2Fs;
use kernel_vfs::Stat;
use kernel_vfs::path::{AbsolutePath, ROOT};
use tracing::info;

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

    // A kernel stack overflow cannot be provoked from userspace, so the trigger has
    // to live in the kernel. It is opt-in through a marker file so that the shared
    // test kernel stays generic, and so no other suite pays for it.
    if let Ok(marker) = AbsolutePath::try_new("/kernel-stack-overflow")
        && vfs().write().open(marker).is_ok()
    {
        let task = Task::create_new(Process::root(), overflow_kernel_stack, ptr::null_mut())
            .expect("should be able to create the stack overflow task");
        kernel::serial_println!("test-kernel: kernel stack overflow armed");
        GlobalTaskQueue::enqueue(Box::pin(task));
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
        let mut pending: alloc::vec::Vec<Arc<Process>> = vec![];
        for line in manifest.lines() {
            if line.is_empty() {
                continue;
            }
            let path = AbsolutePath::try_new(line).expect("manifest entry should be a valid path");
            let proc = Process::create_from_executable(Process::root(), path)
                .expect("should be able to spawn manifest entry");
            kernel::serial_println!("test-kernel: spawned {} pid={}", line, proc.pid());
            pending.push(proc);
        }
        kernel::serial_println!("test-kernel: spawn complete count={}", pending.len());

        // No thread syscall exists, so the reap target is attached here,
        // opt-in through the marker file.
        if let Ok(marker) = AbsolutePath::try_new("/exec-sibling")
            && vfs().write().open(marker).is_ok()
        {
            let target = pending
                .first()
                .expect("manifest must spawn a process")
                .clone();
            let task = Task::create_new(&target, exec_sibling, ptr::null_mut())
                .expect("should be able to create the exec sibling task");
            kernel::serial_println!("test-kernel: sibling attached pid={}", target.pid());
            GlobalTaskQueue::enqueue(Box::pin(task));
        }

        // The other CPUs run the scheduler, so CPU 0 can poll here without
        // stalling user tasks.
        while !pending.is_empty() {
            pending.retain(|proc| match proc.exit_outcome() {
                Some(ExitOutcome::Exited(code)) => {
                    kernel::serial_println!(
                        "test-kernel: outcome pid={} exit={}",
                        proc.pid(),
                        code
                    );
                    false
                }
                Some(ExitOutcome::Signaled(signo)) => {
                    kernel::serial_println!(
                        "test-kernel: outcome pid={} signal={}",
                        proc.pid(),
                        signo.name()
                    );
                    false
                }
                None => true,
            });
            if !pending.is_empty() {
                x86_64::instructions::hlt();
            }
        }
    }

    mcore::exit_bootstrap()
}

#[panic_handler]
fn rust_panic(info: &core::panic::PanicInfo) -> ! {
    handle_panic(info);
    loop {
        x86_64::instructions::hlt();
    }
}

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

    match kernel::backtrace::Backtrace::try_capture() {
        Ok(bt) => {
            error!("stack backtrace:\n{bt}");
        }
        Err(e) => {
            error!("error capturing backtrace: {e:?}");
        }
    }
}

/// Recurses until RSP walks into the kernel stack guard page.
///
/// Each frame has to stay far below the 4 KiB guard page. A frame larger than
/// the guard page can step over it into unreserved virtual memory, where the
/// fault is no longer recognizable as a stack overflow. The volatile access
/// keeps the frame live, and the addition after the recursive call keeps the
/// call from becoming a tail call that the optimizer folds into a loop.
extern "C" fn overflow_kernel_stack(_: *mut c_void) {
    #[inline(never)]
    #[allow(unconditional_recursion)]
    fn recurse(depth: u64) -> u64 {
        let mut frame = [depth; 8];
        unsafe { core::ptr::write_volatile(&raw mut frame[0], depth) };
        let deeper = recurse(depth.wrapping_add(1));
        unsafe { core::ptr::read_volatile(&raw const frame[0]) }.wrapping_add(deeper)
    }

    core::hint::black_box(recurse(0));
}

/// Parks like a task blocked in a syscall, so the execve reaper must wake it
/// before it can observe the termination request. Returning falls into
/// `Task::exit`, which the task stack seeds as the return address.
extern "C" fn exec_sibling(_: *mut c_void) {
    let ctx = kernel::mcore::context::ExecutionContext::load();
    let task = ctx.current_task();
    let process = task.process().clone();
    match process.park_current_task(None, || false) {
        ParkOutcome::Interrupted => {
            kernel::serial_println!("test-kernel: sibling terminating");
        }
        ParkOutcome::Ready => {
            kernel::serial_println!("test-kernel: sibling woke without a reap");
        }
    }
}
