//! This module describes the thread-local storage block of one thread in the
//! x86-64 ELF TLS variant II. The TLS data ends at the thread pointer `tp`.
//! The first word at `tp` holds `tp` itself.

use core::alloc::Layout;

const SELF_POINTER_LEN: usize = size_of::<usize>();

/// This type holds the layout of the TLS block that one `PT_TLS` header describes.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct TlsLayout {
    block: Layout,
    image_len: usize,
    /// This field holds `memsz` rounded up to the alignment. It is the
    /// distance from the block start to `tp`.
    tp_offset: usize,
}

impl TlsLayout {
    /// This function builds the layout from the `filesz`, `memsz` and `align`
    /// fields of a `PT_TLS` header. An `align` of 0 means no alignment. It
    /// returns `None` when `filesz > memsz`, when `align` is not a power of
    /// two, or when the block size overflows.
    #[must_use]
    pub fn new(filesz: usize, memsz: usize, align: usize) -> Option<Self> {
        if filesz > memsz {
            return None;
        }
        let align = align.max(1);
        if !align.is_power_of_two() {
            return None;
        }
        let tp_offset = memsz.checked_next_multiple_of(align)?;
        let block = Layout::from_size_align(
            tp_offset.checked_add(SELF_POINTER_LEN)?,
            align.max(align_of::<usize>()),
        )
        .ok()?;
        Some(Self {
            block,
            image_len: filesz,
            tp_offset,
        })
    }

    #[must_use]
    pub fn block(&self) -> Layout {
        self.block
    }

    /// This method initializes `block`. The block starts at the virtual address
    /// `block_start` and has the size and the alignment of [`TlsLayout::block`].
    /// `write_image` fills the image bytes at the block start. The bytes up
    /// to `tp` become zero, and the self pointer goes to `tp`. The method
    /// returns `tp`, which is the value of `FsBase` for the thread.
    ///
    /// # Errors
    /// This method returns each error of `write_image`.
    ///
    /// # Panics
    /// This method panics when `block` is shorter than the block size.
    pub fn init<E>(
        &self,
        block: &mut [u8],
        block_start: usize,
        write_image: impl FnOnce(&mut [u8]) -> Result<(), E>,
    ) -> Result<usize, E> {
        debug_assert!(block_start.is_multiple_of(self.block.align()));
        let (data, self_pointer) = block[..self.block.size()].split_at_mut(self.tp_offset);
        let (image, zeroed) = data.split_at_mut(self.image_len);
        write_image(image)?;
        zeroed.fill(0);
        let tp = block_start + self.tp_offset;
        self_pointer.copy_from_slice(&tp.to_ne_bytes());
        Ok(tp)
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    const BLOCK_START: usize = 0x4000;

    fn word_at(block: &[u8], offset: usize) -> usize {
        usize::from_ne_bytes(block[offset..offset + SELF_POINTER_LEN].try_into().unwrap())
    }

    #[test]
    fn tp_follows_the_rounded_data_and_points_at_itself() {
        let layout = TlsLayout::new(3, 20, 16).unwrap();
        assert_eq!(layout.block().size(), 32 + SELF_POINTER_LEN);
        assert_eq!(layout.block().align(), 16);

        let mut block = vec![0xAA; layout.block().size()];
        let tp = layout
            .init::<()>(&mut block, BLOCK_START, |image| {
                image.copy_from_slice(&[1, 2, 3]);
                Ok(())
            })
            .unwrap();

        assert_eq!(tp, BLOCK_START + 32);
        assert!(tp.is_multiple_of(16), "tp keeps the TLS alignment");
        assert_eq!(&block[..3], [1, 2, 3], "the image starts the block");
        assert!(
            block[3..32].iter().all(|&b| b == 0),
            "the bytes after the image are zero up to tp"
        );
        assert_eq!(word_at(&block, 32), tp, "the word at tp holds tp");
    }

    #[test]
    fn variable_offsets_count_from_tp_minus_the_data_size() {
        let layout = TlsLayout::new(8, 8, 8).unwrap();
        let mut block = vec![0; layout.block().size()];
        let image = 0x1122_3344_5566_7788_usize.to_ne_bytes();
        let tp = layout
            .init::<()>(&mut block, BLOCK_START, |dst| {
                dst.copy_from_slice(&image);
                Ok(())
            })
            .unwrap();

        // `%fs:-8` reads the variable at TLS offset 0 of an 8-byte image.
        assert_eq!(word_at(&block, tp - 8 - BLOCK_START), 0x1122_3344_5566_7788);
    }

    #[test]
    fn small_alignment_still_aligns_the_block_to_a_word() {
        let layout = TlsLayout::new(0, 4, 0).unwrap();
        assert_eq!(layout.block().size(), 4 + SELF_POINTER_LEN);
        assert_eq!(layout.block().align(), align_of::<usize>());
    }

    #[test]
    fn invalid_headers_have_no_layout() {
        assert_eq!(TlsLayout::new(8, 4, 8), None, "filesz > memsz");
        assert_eq!(TlsLayout::new(0, 4, 3), None, "alignment 3");
        assert_eq!(TlsLayout::new(0, usize::MAX, 16), None, "size overflow");
    }

    #[test]
    fn image_error_reaches_the_caller() {
        let layout = TlsLayout::new(4, 4, 4).unwrap();
        let mut block = vec![0; layout.block().size()];
        assert_eq!(
            layout.init(&mut block, BLOCK_START, |_| Err("read failed")),
            Err("read failed")
        );
    }
}
