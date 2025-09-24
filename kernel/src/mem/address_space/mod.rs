use core::fmt::{Debug, Formatter};

use conquer_once::spin::OnceCell;
use limine::memory_map::EntryType;
use log::{debug, info, trace};
use mapper::AddressSpaceMapper;
use spin::RwLock;
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::mapper::{
    FlagUpdateError, MapToError, MappedFrame, MapperAllSizes, PageTableFrameMapping,
    TranslateResult,
};
use x86_64::structures::paging::page::PageRangeInclusive;
use x86_64::structures::paging::{
    MappedPageTable, Mapper, OffsetPageTable, Page, PageSize, PageTable, PageTableFlags, PhysFrame,
    RecursivePageTable, Size4KiB, Translate,
};
use x86_64::{PhysAddr, VirtAddr};

use crate::limine::{HHDM_REQUEST, KERNEL_ADDRESS_REQUEST, MEMORY_MAP_REQUEST};
use crate::mem::phys::PhysicalMemory;
use crate::mem::virt::{VirtualMemoryAllocator, VirtualMemoryHigherHalf};
use crate::{U64Ext, UsizeExt};

mod mapper;

static KERNEL_ADDRESS_SPACE: OnceCell<AddressSpace> = OnceCell::uninit();
pub static RECURSIVE_INDEX: OnceCell<usize> = OnceCell::uninit();

pub fn init() {
    let (pt_vaddr, pt_frame) = make_mapping_recursive();
    let address_space = unsafe { AddressSpace::create_from(pt_frame, pt_vaddr) };
    KERNEL_ADDRESS_SPACE.init_once(|| address_space);
}

fn make_mapping_recursive() -> (VirtAddr, PhysFrame) {
    let hhdm_offset = HHDM_REQUEST
        .get_response()
        .expect("should have a HHDM response")
        .offset();

    let (level_4_table, level_4_table_frame) = {
        let frame = PhysicalMemory::allocate_frame().unwrap();
        let pt = unsafe {
            &mut *VirtAddr::new(frame.start_address().as_u64() + hhdm_offset)
                .as_mut_ptr::<PageTable>()
        };
        pt.zero();
        (pt, frame)
    };

    let mut current_pt = unsafe {
        OffsetPageTable::new(
            &mut *VirtAddr::new(Cr3::read().0.start_address().as_u64() + hhdm_offset)
                .as_mut_ptr::<PageTable>(),
            VirtAddr::new(hhdm_offset),
        )
    };

    let mut new_pt = {
        struct Offset(u64);
        unsafe impl PageTableFrameMapping for Offset {
            fn frame_to_pointer(&self, frame: PhysFrame) -> *mut PageTable {
                VirtAddr::new(frame.start_address().as_u64() + self.0).as_mut_ptr::<PageTable>()
            }
        }
        unsafe { MappedPageTable::new(level_4_table, Offset(hhdm_offset)) }
    };

    let kernel_addr = KERNEL_ADDRESS_REQUEST
        .get_response()
        .unwrap()
        .virtual_base();
    assert_eq!(
        kernel_addr, 0xffff_ffff_8000_0000,
        "kernel address should be 0xffff_ffff_8000_0000, if it isn't, either check the linker file or you know what you're doing"
    );

    info!("remapping kernel");
    remap(
        &mut current_pt,
        &mut new_pt,
        VirtAddr::new(kernel_addr),
        usize::MAX - kernel_addr.into_usize() + 1, // remap from the kernel base until the end of the address space
    );

    MEMORY_MAP_REQUEST
        .get_response()
        .unwrap()
        .entries()
        .iter()
        .filter(|e| e.entry_type == EntryType::EXECUTABLE_AND_MODULES)
        .for_each(|e| {
            info!(
                "remapping module of size ~{}MiB ({} bytes) at virt={:p}",
                e.length / 1024 / 1024,
                e.length,
                VirtAddr::new(e.base + hhdm_offset),
            );
            remap(
                &mut current_pt,
                &mut new_pt,
                VirtAddr::new(e.base + hhdm_offset),
                e.length.into_usize(),
            );
        });

    MEMORY_MAP_REQUEST
        .get_response()
        .unwrap()
        .entries()
        .iter()
        .filter(|e| e.entry_type == EntryType::BOOTLOADER_RECLAIMABLE)
        .for_each(|e| {
            remap(
                &mut current_pt,
                &mut new_pt,
                VirtAddr::new(e.base + hhdm_offset),
                e.length.into_usize(),
            );
        });

    let recursive_index = (0..512)
        .rposition(|p| level_4_table[p].is_unused())
        .expect("should have an unused index in the level 4 table");
    level_4_table[recursive_index].set_frame(
        level_4_table_frame,
        PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE,
    );
    let vaddr = recursive_index_to_virtual_address(recursive_index);
    debug!("recursive index: {recursive_index:?}, vaddr: {vaddr:p}");
    RECURSIVE_INDEX.init_once(|| recursive_index);

    level_4_table
        .iter_mut()
        .skip(256)
        .filter(|e| e.is_unused())
        .for_each(|e| {
            e.set_frame(
                PhysicalMemory::allocate_frame().unwrap(),
                PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE,
            );
        });

    info!("switching to recursive mapping");
    unsafe {
        let cr3_flags = Cr3::read().1;
        Cr3::write(level_4_table_frame, cr3_flags);
    }

    (vaddr, level_4_table_frame)
}

fn remap(
    current_pt: &mut OffsetPageTable,
    new_pt: &mut impl MapperAllSizes,
    start_vaddr: VirtAddr,
    len: usize,
) {
    let mut current_addr = start_vaddr;

    while current_addr.as_u64() <= start_vaddr.as_u64() - 1 + len as u64 {
        let result = current_pt.translate(current_addr);
        let TranslateResult::Mapped {
            frame,
            offset,
            flags,
        } = result
        else {
            break;
        };

        let flags = flags.intersection(
            PageTableFlags::PRESENT
                | PageTableFlags::WRITABLE
                | PageTableFlags::NO_EXECUTE
                | PageTableFlags::HUGE_PAGE,
        );

        if offset != 0 {
            // There are cases where limine maps huge pages across borders of memory regions
            // in the HHDM for example, the last pages of a 'usable' section and the first
            // pages of a 'bootloader reclaimable' section could be mapped to the same 2MiB or 1GiB
            // huge frame. We need to handle this accordingly.

            let mut flags = flags;
            flags.remove(PageTableFlags::HUGE_PAGE);

            let MappedFrame::Size2MiB(f) = frame else {
                todo!("support huge pages crossing region borders");
            };

            trace!(
                "breaking up cross-region huge page ({:p} offset {:x})",
                f.start_address(),
                offset
            );

            let mut off = 0;
            while (current_addr + off).as_u64() < (start_vaddr.as_u64() + len.into_u64())
                && (offset + off < frame.size())
            {
                let page = Page::<Size4KiB>::containing_address(current_addr + off);
                let f1 = PhysFrame::containing_address(f.start_address() + offset + off);
                unsafe {
                    let _ = new_pt.map_to(page, f1, flags, &mut PhysicalMemory).unwrap();
                }
                off += page.size();
            }
        } else {
            unsafe {
                match frame {
                    MappedFrame::Size4KiB(f) => {
                        let _ = new_pt
                            .map_to(
                                Page::containing_address(current_addr),
                                f,
                                flags,
                                &mut PhysicalMemory,
                            )
                            .unwrap();
                    }
                    MappedFrame::Size2MiB(f) => {
                        let _ = new_pt
                            .map_to(
                                Page::containing_address(current_addr),
                                f,
                                flags,
                                &mut PhysicalMemory,
                            )
                            .unwrap();
                    }
                    MappedFrame::Size1GiB(f) => {
                        let _ = new_pt
                            .map_to(
                                Page::containing_address(current_addr),
                                f,
                                flags,
                                &mut PhysicalMemory,
                            )
                            .unwrap();
                    }
                }
            }
        }
        current_addr += frame.size() - offset;
    }
}

#[must_use]
pub const fn recursive_index_to_virtual_address(recursive_index: usize) -> VirtAddr {
    let i = recursive_index as u64;
    let addr = (i << 39) | (i << 30) | (i << 21) | (i << 12);

    let addr = sign_extend_vaddr(addr);

    VirtAddr::new(addr)
}

#[must_use]
pub const fn virt_addr_from_page_table_indices(indices: [u16; 4], offset: u64) -> VirtAddr {
    let addr = ((indices[0] as u64) << 39)
        | ((indices[1] as u64) << 30)
        | ((indices[2] as u64) << 21)
        | ((indices[3] as u64) << 12)
        | (offset & ((1 << 12) - 1));
    VirtAddr::new(sign_extend_vaddr(addr))
}

#[must_use]
pub const fn sign_extend_vaddr(vaddr: u64) -> u64 {
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_wrap)]
    let result = ((vaddr << 16) as i64 >> 16) as u64; // only works for 48-bit addresses
    result
}

pub struct AddressSpace {
    level4_frame: PhysFrame,
    inner: RwLock<AddressSpaceMapper>,
}

impl Debug for AddressSpace {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AddressSpace")
            .field("level4_frame", &self.level4_frame)
            .field("active", &self.inner.read().is_active())
            .finish_non_exhaustive()
    }
}

impl !Default for AddressSpace {}

impl AddressSpace {
    /// # Panics
    /// Panics if the kernel address space is not initialized yet.
    pub fn kernel() -> &'static Self {
        KERNEL_ADDRESS_SPACE
            .get()
            .expect("address space not initialized")
    }

    unsafe fn create_from(level4_frame: PhysFrame, level4_vaddr: VirtAddr) -> Self {
        Self {
            level4_frame,
            inner: RwLock::new(AddressSpaceMapper::new(level4_frame, level4_vaddr)),
        }
    }

    /// # Panics
    /// This function panics if not enough physical or virtual memory is available to create
    /// a new address space, or if something goes wrong during mapping of addresses.
    #[must_use]
    pub fn new() -> Self {
        let new_frame = PhysicalMemory::allocate_frame().unwrap();
        let new_pt_segment = VirtualMemoryHigherHalf.reserve(1).unwrap();
        let old_pt_segment = VirtualMemoryHigherHalf.reserve(1).unwrap();

        let old_pt_page = Page::containing_address(old_pt_segment.start);
        Self::kernel()
            .map::<Size4KiB>(
                old_pt_page,
                Self::kernel().level4_frame,
                PageTableFlags::PRESENT | PageTableFlags::NO_EXECUTE,
            )
            .unwrap();

        let new_pt_page = Page::containing_address(new_pt_segment.start);
        Self::kernel()
            .map::<Size4KiB>(
                new_pt_page,
                new_frame,
                PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE,
            )
            .unwrap();

        let new_page_table = unsafe { &mut *new_pt_segment.start.as_mut_ptr::<PageTable>() };
        let old_page_table = unsafe { &*old_pt_segment.start.as_mut_ptr::<PageTable>() };

        new_page_table.zero();
        new_page_table
            .iter_mut()
            .zip(old_page_table.iter())
            .skip(256)
            .for_each(|(new_entry, old_entry)| {
                *new_entry = old_entry.clone();
            });
        let recursive_index = *RECURSIVE_INDEX.get().unwrap();
        new_page_table[recursive_index].set_frame(
            new_frame,
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE,
        );

        Self::kernel()
            .unmap(old_pt_page)
            .expect("page should be mapped");
        Self::kernel()
            .unmap(new_pt_page)
            .expect("page should be mapped");

        unsafe { Self::create_from(new_frame, Self::kernel().inner.read().level4_vaddr) }
    }

    pub fn cr3_value(&self) -> usize {
        self.level4_frame.start_address().as_u64().into_usize()
    }

    #[allow(dead_code)]
    pub fn is_active(&self) -> bool {
        self.inner.read().is_active()
    }

    /// # Errors
    /// Returns an error if the page is already mapped or flags are invalid.
    #[allow(dead_code)]
    pub fn map<S: PageSize>(
        &self,
        page: Page<S>,
        frame: PhysFrame<S>,
        flags: PageTableFlags,
    ) -> Result<(), MapToError<S>>
    where
        for<'a> RecursivePageTable<'a>: Mapper<S>,
    {
        self.inner.write().map(page, frame, flags)
    }

    /// # Errors
    /// Returns an error if the pages are already mapped or flags are invalid.
    pub fn map_range<S: PageSize>(
        &self,
        pages: impl Into<PageRangeInclusive<S>>,
        frames: impl Iterator<Item = PhysFrame<S>>,
        flags: PageTableFlags,
    ) -> Result<(), MapToError<S>>
    where
        for<'a> RecursivePageTable<'a>: Mapper<S>,
    {
        self.inner.write().map_range(pages.into(), frames, flags)
    }

    pub fn unmap<S: PageSize>(&self, page: Page<S>) -> Option<PhysFrame<S>>
    where
        for<'a> RecursivePageTable<'a>: Mapper<S>,
    {
        self.inner.write().unmap(page)
    }

    pub fn unmap_range<S: PageSize>(
        &self,
        pages: impl Into<PageRangeInclusive<S>>,
        callback: impl Fn(PhysFrame<S>),
    ) where
        for<'a> RecursivePageTable<'a>: Mapper<S>,
    {
        self.inner.write().unmap_range(pages.into(), callback);
    }

    /// # Errors
    /// Returns an error if the page is not mapped or flags are invalid.
    pub fn remap<S: PageSize, F: Fn(PageTableFlags) -> PageTableFlags>(
        &self,
        page: Page<S>,
        f: F,
    ) -> Result<(), FlagUpdateError>
    where
        for<'a> RecursivePageTable<'a>: Mapper<S>,
    {
        self.inner.write().remap(page, &f)
    }

    /// # Errors
    /// Returns an error if the pages are not mapped or flags are invalid.
    pub fn remap_range<S: PageSize, F: Fn(PageTableFlags) -> PageTableFlags>(
        &self,
        pages: impl Into<PageRangeInclusive<S>>,
        f: F,
    ) -> Result<(), FlagUpdateError>
    where
        for<'a> RecursivePageTable<'a>: Mapper<S>,
    {
        self.inner.write().remap_range(pages.into(), &f)
    }

    #[allow(dead_code)]
    pub fn translate(&self, vaddr: VirtAddr) -> Option<PhysAddr> {
        self.inner.read().translate(vaddr)
    }
}
