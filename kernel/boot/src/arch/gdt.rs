use alloc::boxed::Box;
use core::mem;

use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::{PrivilegeLevel, VirtAddr};

use crate::mcore::mtask::task::HigherHalfStack;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;
pub const PAGE_FAULT_IST_INDEX: u16 = 1;

fn create_tss() -> TaskStateSegment {
    let mut tss = TaskStateSegment::new();
    tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = allocate_exception_stack(5);
    tss.interrupt_stack_table[PAGE_FAULT_IST_INDEX as usize] = allocate_exception_stack(5);

    // Boot fallback for Ring 3 -> Ring 0 transitions before the first reschedule
    // updates `privilege_stack_table[0]` to the running task's kernel stack. After
    // tasks start running, this value is overwritten on every context switch.
    tss.privilege_stack_table[0] = allocate_exception_stack(4);
    tss
}

/// Allocates a stack for the hardware to switch to on an exception, with
/// `usable_pages` writable pages above an unmapped guard page.
fn allocate_exception_stack(usable_pages: usize) -> VirtAddr {
    let stack = HigherHalfStack::allocate_plain(usable_pages + 1)
        .expect("should be able to allocate an exception stack");
    let top = stack.top();

    // The TSS holds the only reference for the lifetime of the CPU, and the
    // hardware reads it without asking us. Dropping the handle would unmap the
    // stack and turn the next exception into a triple fault.
    mem::forget(stack);
    top
}

#[allow(dead_code)]
#[derive(Debug)]
pub struct Selectors {
    pub kernel_code: SegmentSelector,
    pub kernel_data: SegmentSelector,
    pub tss: SegmentSelector,
    pub user_code: SegmentSelector,
    pub user_data: SegmentSelector,
}

pub fn create_gdt_and_tss() -> (GlobalDescriptorTable, Selectors, *mut TaskStateSegment) {
    let mut gdt = GlobalDescriptorTable::new();
    let kernel_code = gdt.append(Descriptor::kernel_code_segment());
    let kernel_data = gdt.append(Descriptor::kernel_data_segment());

    let tss_ptr: *mut TaskStateSegment = Box::into_raw(Box::new(create_tss()));
    // Safety: tss_ptr is a freshly leaked allocation, so it is non-null and lives
    // for 'static. Subsequent writes through tss_ptr (per-task RSP0 updates) are
    // serialized on a single CPU's reschedule path.
    let tss_static: &'static TaskStateSegment = unsafe { &*tss_ptr };
    let tss = gdt.append(Descriptor::tss_segment(tss_static));
    let mut user_code = gdt.append(Descriptor::user_code_segment());
    user_code.set_rpl(PrivilegeLevel::Ring3);
    let mut user_data = gdt.append(Descriptor::user_data_segment());
    user_data.set_rpl(PrivilegeLevel::Ring3);
    (
        gdt,
        Selectors {
            kernel_code,
            kernel_data,
            tss,
            user_code,
            user_data,
        },
        tss_ptr,
    )
}
