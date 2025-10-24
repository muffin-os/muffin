use log::info;

use crate::limine::MEMORY_MAP_REQUEST;
use crate::mem::address_space::AddressSpace;
use crate::mem::heap::Heap;

pub mod address_space;
pub mod heap;
pub mod memapi;
pub mod phys;
pub mod virt;

#[allow(clippy::missing_panics_doc)]
pub fn init() {
    let response = MEMORY_MAP_REQUEST
        .get_response()
        .expect("should have a memory map response");

    let usable_physical_memory = phys::init_stage1(response.entries());

    address_space::init();

    let address_space = AddressSpace::kernel();

    heap::init(address_space, usable_physical_memory);

    virt::init();

    phys::init_stage2();

    heap::init_stage2();

    info!("memory initialized, {Heap:x?}");
}
