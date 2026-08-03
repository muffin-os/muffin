use alloc::boxed::Box;
use alloc::vec;

use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::paging::{PageSize, Size4KiB};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::{PrivilegeLevel, VirtAddr};

use crate::U64Ext;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;
pub const PAGE_FAULT_IST_INDEX: u16 = 1;

fn create_tss() -> TaskStateSegment {
    let mut tss = TaskStateSegment::new();
    tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = allocate_ist_stack(5);
    tss.interrupt_stack_table[PAGE_FAULT_IST_INDEX as usize] = allocate_ist_stack(5);

    // Boot fallback for Ring 3 -> Ring 0 transitions before the first reschedule
    // updates `privilege_stack_table[0]` to the running task's kernel stack. After
    // tasks start running, this value is overwritten on every context switch.
    tss.privilege_stack_table[0] = allocate_ist_stack(4);
    tss
}

fn allocate_ist_stack(pages: usize) -> VirtAddr {
    let stack_size = Size4KiB::SIZE.into_usize() * pages;
    let stack = Box::into_raw(vec![0_u8; stack_size].into_boxed_slice());

    let stack_start = VirtAddr::from_ptr(stack);
    stack_start + (stack_size as u64)
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
