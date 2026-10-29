#![no_std]
#![no_main]
extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::{format, vec};
use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use kernel::driver::block::BlockDevices;
use kernel::file::ext2::VirtualExt2Fs;
use kernel::file::vfs;
use kernel::limine::BASE_REVISION;
use kernel::mcore;
use kernel::mcore::mtask::process::mem::{MemoryRegion, PrivateMemoryRegion};
use kernel::mcore::mtask::process::{ExitOutcome, ParkOutcome, Process};
use kernel::mcore::mtask::scheduler::global::GlobalTaskQueue;
use kernel::mcore::mtask::task::Task;
use kernel::mem::address_space::AddressSpace;
use kernel::mem::heap::Heap;
use kernel::mem::phys::PhysicalMemory;
use kernel::mem::virt::{VirtualMemoryAllocator, VirtualMemoryHigherHalf};
use kernel_ext2::Ext2Fs;
use kernel_vfs::path::{AbsolutePath, ROOT};
use kernel_vfs::{CreateMode, Stat};
use kernel_virtual_memory::Segment;
use tracing::info;
use x86_64::VirtAddr;
use x86_64::registers::control::{Cr8, PriorityClass};
use x86_64::structures::paging::{Page, PageSize, PageTableFlags, Size4KiB};

const COW_PAGE_COUNT: usize = 3;
const PAGE_SIZE: usize = Size4KiB::SIZE as usize;
const ORIGIN_PATTERN: [u8; COW_PAGE_COUNT] = [0xA5, 0x5A, 0xC3];
const ORIGIN_WRITER_BYTE: u8 = 0x11;
const CLONE_BYTE: u8 = 0x22;
const ORIGIN_BREAK_BYTE: u8 = 0x33;
const ORIGIN_RECLAIM_BYTE: u8 = 0x44;

static COW_REGION_BASE: AtomicU64 = AtomicU64::new(0);
static COW_REGION_READY: AtomicBool = AtomicBool::new(false);
static COW_WRITER_STARTED: AtomicBool = AtomicBool::new(false);
static COW_CLONED: AtomicBool = AtomicBool::new(false);
static COW_WRITER_DONE: AtomicBool = AtomicBool::new(false);
static COW_READER_STARTED: AtomicBool = AtomicBool::new(false);
static COW_CLONE_WROTE: AtomicBool = AtomicBool::new(false);
static COW_READER_DONE: AtomicBool = AtomicBool::new(false);
static COW_CLONE_DONE: AtomicBool = AtomicBool::new(false);
static COW_PROBE_DONE: AtomicBool = AtomicBool::new(false);

/// Boots the real kernel, mounts the root ext2 filesystem, then spawns one
/// process per line of the `/spawn` manifest baked into the disk image. Each
/// manifest line is an absolute in-OS path spawned in order, so the first
/// entry becomes pid 1.
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
        && vfs().write().open(marker, CreateMode::Never).is_ok()
    {
        let task = Task::create_new(Process::root(), overflow_kernel_stack, ptr::null_mut())
            .expect("should be able to create the stack overflow task");
        kernel::serial_println!("test-kernel: kernel stack overflow armed");
        GlobalTaskQueue::enqueue(Box::pin(task));
    }

    // A mid-range map failure cannot be provoked from userspace either, so the
    // probe pre-maps one page to make `map_range` fail partway through.
    if AbsolutePath::try_new("/map-range-rollback")
        .is_ok_and(|marker| vfs().write().open(marker, CreateMode::Never).is_ok())
    {
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;
        let window = VirtualMemoryHigherHalf
            .reserve(4)
            .expect("should be able to reserve the probe window");
        let first = Page::<Size4KiB>::containing_address(window.start);
        let blocker = PhysicalMemory::allocate_frame::<Size4KiB>().expect("should have a frame");
        AddressSpace::kernel()
            .map(first + 2, blocker, flags)
            .expect("should be able to pre-map the blocking page");
        let owned =
            PhysicalMemory::allocate_frames::<Size4KiB>(4).expect("should have four frames");
        assert!(
            AddressSpace::kernel()
                .map_range::<Size4KiB>(&*window, *owned, flags)
                .is_err(),
            "map_range should fail on the pre-mapped page"
        );
        let still_mapped = (0..2)
            .filter(|i| {
                AddressSpace::kernel()
                    .translate((first + *i).start_address())
                    .is_some()
            })
            .count();
        if let Some(frame) = AddressSpace::kernel().unmap(first + 2) {
            PhysicalMemory::deallocate_frame(frame);
        }
        kernel::serial_println!(
            "test-kernel: map-range rollback still_mapped={}",
            still_mapped
        );
    }

    // The heap's end bounds are kernel-internal, so this probe reports whether
    // a page past the heap end is mapped and whether the heap's last page is
    // reserved in the higher-half manager.
    if AbsolutePath::try_new("/heap-bounds")
        .is_ok_and(|marker| vfs().write().open(marker, CreateMode::Never).is_ok())
    {
        let end = Heap::bottom() + Heap::size() as u64;
        let past_end_mapped = AddressSpace::kernel().translate(end).is_some();
        let last_page = Segment::new(end - Size4KiB::SIZE, Size4KiB::SIZE);
        let last_page_reserved = VirtualMemoryHigherHalf.mark_as_reserved(last_page).is_err();
        kernel::serial_println!(
            "test-kernel: heap bounds past_end_mapped={} last_page_reserved={}",
            past_end_mapped,
            last_page_reserved
        );
    }

    // Directory creation has no syscall yet, so the probe drives the vfs
    // directly and leaves a file behind for the host to read back.
    if AbsolutePath::try_new("/vfs-mkdir")
        .is_ok_and(|marker| vfs().write().open(marker, CreateMode::Never).is_ok())
    {
        match create_probe_directory_with_file() {
            Ok(()) => kernel::serial_println!("test-kernel: vfs-mkdir ok"),
            Err(e) => kernel::serial_println!("test-kernel: vfs-mkdir FAIL {}", e),
        }
    }

    // No syscall clones the memory regions of a process yet, so the probe
    // drives the clone directly. Only a task of pid 0 can map task stacks
    // and page tables, so this task creates every process and task.
    if AbsolutePath::try_new("/cow-clone")
        .is_ok_and(|marker| vfs().write().open(marker, CreateMode::Never).is_ok())
    {
        let origin =
            Process::create_new(Process::root(), "cow-origin".into(), None::<&AbsolutePath>);
        let clone = Process::create_new(Process::root(), "cow-clone".into(), None::<&AbsolutePath>);
        spawn_task(
            &origin,
            cow_origin,
            Arc::into_raw(clone.clone()).cast_mut().cast(),
        );
        spawn_task(&origin, cow_origin_writer, ptr::null_mut());
        spawn_task(&clone, cow_clone_task, ptr::null_mut());
        spawn_task(&clone, cow_clone_reader, ptr::null_mut());
        wait_for(&COW_PROBE_DONE);
    }

    let attach_exec_sibling = AbsolutePath::try_new("/exec-sibling")
        .is_ok_and(|marker| vfs().write().open(marker, CreateMode::Never).is_ok());

    {
        info!("reading spawn manifest");
        let manifest_path = AbsolutePath::try_new("/spawn").expect("should be a valid path");
        let node = vfs()
            .write()
            .open(manifest_path, CreateMode::Never)
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
            let proc = if attach_exec_sibling && pending.is_empty() {
                let (proc, main_task) = Process::create_unscheduled(Process::root(), path)
                    .expect("should be able to spawn manifest entry");
                let sibling = Task::create_new(&proc, exec_sibling, ptr::null_mut())
                    .expect("should be able to create the exec sibling task");
                GlobalTaskQueue::enqueue(Box::pin(sibling));
                kernel::serial_println!("test-kernel: sibling attached pid={}", proc.pid());
                GlobalTaskQueue::enqueue(Box::pin(main_task));
                proc
            } else {
                Process::create_from_executable(Process::root(), path)
                    .expect("should be able to spawn manifest entry")
            };
            kernel::serial_println!("test-kernel: spawned {} pid={}", line, proc.pid());
            pending.push(proc);
        }
        kernel::serial_println!("test-kernel: spawn complete count={}", pending.len());

        // The other CPUs run the scheduler, so CPU 0 can poll here without
        // stalling user tasks.
        while !pending.is_empty() {
            pending.retain(|proc| {
                let Some(outcome) = proc.exit_outcome() else {
                    return true;
                };
                // the harness pulls the plug once every outcome is printed, so
                // the disk the host inspects must be complete before that
                vfs()
                    .read()
                    .sync_all()
                    .expect("should be able to sync file systems");
                match outcome {
                    ExitOutcome::Exited(code) => kernel::serial_println!(
                        "test-kernel: outcome pid={} exit={}",
                        proc.pid(),
                        code
                    ),
                    ExitOutcome::Signaled(signo) => kernel::serial_println!(
                        "test-kernel: outcome pid={} signal={}",
                        proc.pid(),
                        signo.name()
                    ),
                }
                false
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

    if let Some(location) = info.location() {
        error!(
            "kernel panicked at {}:{}:{}:",
            location.file(),
            location.line(),
            location.column(),
        );
    }
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
extern "C" fn overflow_kernel_stack(_: *mut c_void) {
    // Each frame has to stay far below the 4 KiB guard page, or it could step
    // over it into unreserved virtual memory, where the fault is no longer
    // recognizable as a stack overflow. The volatile access keeps the frame
    // live, and the addition after the recursive call keeps it from becoming a
    // tail call the optimizer folds into a loop.
    #[inline(never)]
    #[expect(unconditional_recursion)]
    fn recurse(depth: u64) -> u64 {
        let mut frame = [depth; 8];
        unsafe { core::ptr::write_volatile(&raw mut frame[0], depth) };
        let deeper = recurse(depth.wrapping_add(1));
        unsafe { core::ptr::read_volatile(&raw const frame[0]) }.wrapping_add(deeper)
    }

    core::hint::black_box(recurse(0));
}

fn create_probe_directory_with_file() -> Result<(), String> {
    let vfs = vfs().read();
    vfs.create_directory(AbsolutePath::try_new("/probe").expect("should be a valid path"))
        .map_err(|e| format!("mkdir: {e}"))?;
    let file = vfs
        .open(
            AbsolutePath::try_new("/probe/f").expect("should be a valid path"),
            CreateMode::Exclusive,
        )
        .map_err(|e| format!("open: {e}"))?;
    file.write(b"made by the test kernel\n", 0)
        .map_err(|e| format!("write: {e}"))?;
    Ok(())
}

fn wait_for(flag: &AtomicBool) {
    while !flag.load(Ordering::Acquire) {
        x86_64::instructions::hlt();
    }
}

/// Runs `f` with the timer interrupt masked on this CPU. The timer vector
/// (`InterruptIndex::Timer`) is in priority class 2. The shootdown vector
/// (`InterruptIndex::TlbShootdown`) is in class 3 and still arrives.
/// Without the timer the scheduler never switches this CPU. The task stays
/// on this CPU. The CPU keeps its TLB entries.
fn with_timer_masked<R>(f: impl FnOnce() -> R) -> R {
    Cr8::write(Some(PriorityClass::PriorityClass2));
    let result = f();
    Cr8::write(None);
    result
}

fn page_ptr(index: usize) -> *mut u8 {
    (COW_REGION_BASE.load(Ordering::Acquire) as usize + index * PAGE_SIZE) as *mut u8
}

fn read_page(index: usize) -> u8 {
    // Safety: `page_ptr` points into a region of the current process.
    unsafe { ptr::read_volatile(page_ptr(index)) }
}

fn write_page(index: usize, byte: u8) {
    // Safety: see `read_page`.
    unsafe { ptr::write_volatile(page_ptr(index), byte) }
}

// The lookup reads the active page table, so each task calls it for its
// own process only.
fn page_frame(index: usize) -> String {
    let frame = Process::current()
        .address_space()
        .translate(VirtAddr::new(page_ptr(index) as u64));
    format!("{frame:?}")
}

fn spawn_task(process: &Arc<Process>, entry: extern "C" fn(*mut c_void), arg: *mut c_void) {
    let task =
        Task::create_new(process, entry, arg).expect("should be able to create a cow probe task");
    GlobalTaskQueue::enqueue(Box::pin(task));
}

extern "C" fn cow_origin(clone: *mut c_void) {
    // Safety: the main task created `clone` with `Arc::into_raw` for this
    // task alone.
    let clone = unsafe { Arc::from_raw(clone.cast_const().cast::<Process>()) };
    let process = Task::current().process().clone();
    let segment = process
        .vmm()
        .reserve(COW_PAGE_COUNT)
        .expect("should be able to reserve the cow pages");
    let region = PrivateMemoryRegion::new(
        segment,
        COW_PAGE_COUNT * PAGE_SIZE,
        PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::USER_ACCESSIBLE
            | PageTableFlags::NO_EXECUTE,
    );
    let region = process
        .memory_regions()
        .add_region(MemoryRegion::Private(region));
    COW_REGION_BASE.store(region.addr().as_u64(), Ordering::Release);
    for (index, byte) in ORIGIN_PATTERN.iter().enumerate() {
        write_page(index, *byte);
    }

    COW_REGION_READY.store(true, Ordering::Release);
    wait_for(&COW_WRITER_STARTED);

    let clone_vmm = clone.vmm();
    process
        .memory_regions()
        .clone_into(process.address_space(), clone.memory_regions(), &clone_vmm)
        .expect("should be able to clone the regions");
    COW_CLONED.store(true, Ordering::Release);
    wait_for(&COW_CLONE_DONE);

    let pages = [read_page(0), read_page(1), read_page(2)];
    if pages == [ORIGIN_WRITER_BYTE, ORIGIN_PATTERN[1], ORIGIN_PATTERN[2]] {
        kernel::serial_println!("test-kernel: cow origin isolated=ok");
    } else {
        kernel::serial_println!(
            "test-kernel: cow origin isolated=FAIL p0={:#x} p1={:#x} p2={:#x}",
            pages[0],
            pages[1],
            pages[2]
        );
    }

    let shared_frame = page_frame(2);
    write_page(2, ORIGIN_BREAK_BYTE);
    let page2 = read_page(2);
    let private_frame = page_frame(2);
    if page2 == ORIGIN_BREAK_BYTE {
        kernel::serial_println!(
            "test-kernel: cow origin break=ok frame_changed={}",
            shared_frame != private_frame
        );
    } else {
        kernel::serial_println!("test-kernel: cow origin break=FAIL p2={:#x}", page2);
    }

    let released_frame = page_frame(1);
    write_page(1, ORIGIN_RECLAIM_BYTE);
    let page1 = read_page(1);
    let reclaimed_frame = page_frame(1);
    if page1 == ORIGIN_RECLAIM_BYTE {
        kernel::serial_println!(
            "test-kernel: cow origin reclaim=ok frame_same={}",
            released_frame == reclaimed_frame
        );
    } else {
        kernel::serial_println!("test-kernel: cow origin reclaim=FAIL p1={:#x}", page1);
    }

    COW_PROBE_DONE.store(true, Ordering::Release);
}

/// This task keeps a writable TLB entry for page 0 until the clone exists.
/// Then it writes one time. The write must fault, because the clone demoted
/// the page.
extern "C" fn cow_origin_writer(_: *mut c_void) {
    wait_for(&COW_REGION_READY);
    with_timer_masked(|| {
        COW_WRITER_STARTED.store(true, Ordering::Release);
        while !COW_CLONED.load(Ordering::Acquire) {
            write_page(0, ORIGIN_PATTERN[0]);
        }
        write_page(0, ORIGIN_WRITER_BYTE);
    });
    COW_WRITER_DONE.store(true, Ordering::Release);
}

/// This task stays alive until the probe ends, so the origin's break of
/// page 2 finds the shared backing.
extern "C" fn cow_clone_task(_: *mut c_void) {
    wait_for(&COW_WRITER_DONE);
    wait_for(&COW_READER_STARTED);

    let pages = [read_page(0), read_page(1), read_page(2)];
    if pages == ORIGIN_PATTERN {
        kernel::serial_println!("test-kernel: cow clone shared_read=ok");
    } else {
        kernel::serial_println!(
            "test-kernel: cow clone shared_read=FAIL p0={:#x} p1={:#x} p2={:#x}",
            pages[0],
            pages[1],
            pages[2]
        );
    }

    let shared_frame = page_frame(1);
    write_page(1, CLONE_BYTE);
    let pages = [read_page(0), read_page(1), read_page(2)];
    let private_frame = page_frame(1);
    if pages == [ORIGIN_PATTERN[0], CLONE_BYTE, ORIGIN_PATTERN[2]] {
        kernel::serial_println!(
            "test-kernel: cow clone private_write=ok frame_changed={}",
            shared_frame != private_frame
        );
    } else {
        kernel::serial_println!(
            "test-kernel: cow clone private_write=FAIL p0={:#x} p1={:#x} p2={:#x}",
            pages[0],
            pages[1],
            pages[2]
        );
    }

    COW_CLONE_WROTE.store(true, Ordering::Release);
    wait_for(&COW_READER_DONE);
    COW_CLONE_DONE.store(true, Ordering::Release);
    wait_for(&COW_PROBE_DONE);
}

/// This task keeps a read-only TLB entry for page 1 until the clone wrote
/// the page. Then it reads one time. Without the shootdown this CPU reads
/// the old frame.
extern "C" fn cow_clone_reader(_: *mut c_void) {
    wait_for(&COW_WRITER_DONE);
    let page1 = with_timer_masked(|| {
        COW_READER_STARTED.store(true, Ordering::Release);
        while !COW_CLONE_WROTE.load(Ordering::Acquire) {
            read_page(1);
        }
        read_page(1)
    });
    if page1 == CLONE_BYTE {
        kernel::serial_println!("test-kernel: cow clone reader_sees_write=ok");
    } else {
        kernel::serial_println!(
            "test-kernel: cow clone reader_sees_write=FAIL p1={:#x}",
            page1
        );
    }
    COW_READER_DONE.store(true, Ordering::Release);
}

/// Parks like a task blocked in a syscall, so the execve reaper must wake it
/// before it can observe the termination request.
extern "C" fn exec_sibling(_: *mut c_void) {
    let task = kernel::mcore::mtask::task::Task::current();
    let process = task.process().clone();
    match process.park_current_task(None, || false) {
        ParkOutcome::Interrupted => {
            kernel::serial_println!("test-kernel: sibling terminating");
        }
        ParkOutcome::Ready => {
            kernel::serial_println!("test-kernel: sibling woke without a reap");
        }
    }
    // Returning falls into `Task::exit`, which the task stack seeds as the
    // return address.
}
