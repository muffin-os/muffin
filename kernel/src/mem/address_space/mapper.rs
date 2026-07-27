use x86_64::registers::control::Cr3;
use x86_64::structures::paging::mapper::{FlagUpdateError, MapToError, TranslateResult};
use x86_64::structures::paging::page::PageRangeInclusive;
use x86_64::structures::paging::{
    Mapper, Page, PageSize, PageTable, PageTableFlags, PhysFrame, RecursivePageTable, Translate,
};
use x86_64::{PhysAddr, VirtAddr};

use crate::mem::phys::PhysicalMemory;

#[derive(Debug)]
pub struct AddressSpaceMapper {
    level4_frame: PhysFrame,
    pub(crate) level4_vaddr: VirtAddr,
    page_table: RecursivePageTable<'static>,
}

impl AddressSpaceMapper {
    pub fn new(level4_frame: PhysFrame, level4_vaddr: VirtAddr) -> Self {
        let page_table = {
            let pt = unsafe { &mut *level4_vaddr.as_mut_ptr::<PageTable>() };
            RecursivePageTable::new(pt).expect("should be a valid recursive page table")
        };

        Self {
            level4_frame,
            level4_vaddr,
            page_table,
        }
    }

    pub fn is_active(&self) -> bool {
        self.level4_frame == Cr3::read().0
    }

    pub fn map<S: PageSize>(
        &mut self,
        page: Page<S>,
        frame: PhysFrame<S>,
        flags: PageTableFlags,
    ) -> Result<(), MapToError<S>>
    where
        for<'a> RecursivePageTable<'a>: Mapper<S>,
    {
        assert!(self.is_active());

        #[cfg(debug_assertions)]
        {
            if !flags.contains(PageTableFlags::PRESENT) {
                ::log::warn!(
                    "mapping {:p} to {:p} without PRESENT flag",
                    page.start_address(),
                    frame.start_address()
                );
            }
        }

        unsafe {
            self.page_table
                .map_to(page, frame, flags, &mut PhysicalMemory)?
                .flush();
        }

        Ok(())
    }

    pub fn map_range<S: PageSize>(
        &mut self,
        pages: PageRangeInclusive<S>,
        frames: impl Iterator<Item = PhysFrame<S>>,
        flags: PageTableFlags,
    ) -> Result<(), MapToError<S>>
    where
        for<'a> RecursivePageTable<'a>: Mapper<S>,
    {
        assert!(self.is_active());

        let mut frames = frames.into_iter();

        for page in pages {
            let frame = frames.next().ok_or(MapToError::FrameAllocationFailed)?;
            self.map(page, frame, flags)?;
        }

        Ok(())
    }

    pub fn unmap<S: PageSize>(&mut self, page: Page<S>) -> Option<PhysFrame<S>>
    where
        for<'a> RecursivePageTable<'a>: Mapper<S>,
    {
        assert!(self.is_active());

        if let Ok((frame, flusher)) = self.page_table.unmap(page) {
            flusher.flush();
            Some(frame)
        } else {
            None
        }
    }

    pub fn unmap_range<S: PageSize>(
        &mut self,
        pages: PageRangeInclusive<S>,
        callback: impl Fn(PhysFrame<S>),
    ) where
        for<'a> RecursivePageTable<'a>: Mapper<S>,
    {
        assert!(self.is_active());

        for page in pages {
            self.unmap(page).map(&callback);
        }
    }

    pub fn remap<S: PageSize, F: Fn(PageTableFlags) -> PageTableFlags>(
        &mut self,
        page: Page<S>,
        f: &F,
    ) -> Result<(), FlagUpdateError>
    where
        for<'a> RecursivePageTable<'a>: Mapper<S>,
    {
        assert!(self.is_active());

        let TranslateResult::Mapped {
            frame: _,
            offset: _,
            flags,
        } = self.page_table.translate(page.start_address())
        else {
            return Err(FlagUpdateError::PageNotMapped);
        };
        let flusher = unsafe { self.page_table.update_flags(page, f(flags)) }?;
        flusher.flush();
        Ok(())
    }

    pub fn remap_range<S: PageSize, F: Fn(PageTableFlags) -> PageTableFlags>(
        &mut self,
        pages: PageRangeInclusive<S>,
        f: &F,
    ) -> Result<(), FlagUpdateError>
    where
        for<'a> RecursivePageTable<'a>: Mapper<S>,
    {
        assert!(self.is_active());

        for page in pages {
            self.remap(page, &f)?;
        }
        Ok(())
    }

    pub fn translate(&self, vaddr: VirtAddr) -> Option<PhysAddr> {
        self.page_table.translate_addr(vaddr)
    }

    pub fn translate_flags(&self, vaddr: VirtAddr) -> Option<PageTableFlags> {
        match self.page_table.translate(vaddr) {
            TranslateResult::Mapped { flags, .. } => Some(flags),
            TranslateResult::NotMapped | TranslateResult::InvalidFrameAddress(_) => None,
        }
    }
}
