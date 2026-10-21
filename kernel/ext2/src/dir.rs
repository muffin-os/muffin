use alloc::vec;
use alloc::vec::Vec;
use core::fmt::{Debug, Formatter};

use kernel_device::block::BlockDevice;

use crate::error::Error;
use crate::{
    Directory, Ext2Fs, Inode, InodeAddress, Type, bytefield, bytefield_field_read,
    bytefield_field_write, check_is_implemented,
};

/// Longest directory entry name the on-disk format allows (`EXT2_NAME_LEN`).
pub const MAX_NAME_LEN: usize = 255;

impl<T> Ext2Fs<T>
where
    T: BlockDevice,
{
    pub fn list_dir(&self, dir: &Inode) -> Result<Vec<DirEntry>, Error> {
        if dir.typ() != Some(Type::Directory) {
            return Err(Error::NotDirectory);
        }

        let mut entries = Vec::new();
        let block_size = self.superblock.block_size() as usize;
        let dir_entries_have_type = self.superblock.dir_entries_have_type();

        // shortcut: only the 12 direct blocks are scanned, so entries past
        // 12 * block_size bytes of a directory are missing from the result.
        // Walk resolve_block_index once a directory needs to grow that large.
        for addr in dir.direct_ptrs().flatten() {
            let mut data = vec![0_u8; block_size];
            self.read_block(addr, &mut data)
                .map_err(|_| Error::DeviceRead)?;

            let mut offset = 0;
            while offset < block_size - 8 {
                let record = DirRecord::parse(dir_entries_have_type, &data[offset..])?;
                // we don't need to align the offset, as there must be no space between entries
                offset += usize::from(record.total_size());
                if let DirRecord::Used(entry) = record {
                    entries.push(entry);
                }
            }
        }

        Ok(entries)
    }

    pub fn find_entry<P>(&self, dir: &Directory, p: P) -> Result<Option<DirEntry>, Error>
    where
        P: FnMut(&DirEntry) -> bool,
    {
        Ok(self.list_dir(dir)?.into_iter().find(p))
    }

    pub fn find_and_resolve_entry<P>(
        &self,
        dir: &Directory,
        p: P,
    ) -> Result<Option<(InodeAddress, Inode)>, Error>
    where
        P: FnMut(&DirEntry) -> bool,
    {
        self.find_entry(dir, p)?
            .map(|e| self.resolve_dir_entry(e))
            .transpose()
    }

    pub fn resolve_dir_entry(&self, entry: DirEntry) -> Result<(InodeAddress, Inode), Error> {
        self.read_inode(entry.inode)
    }

    /// Adds an entry named `name` for `inode_address` to `dir`, appending a
    /// directory block when no existing block has room.
    ///
    /// # Errors
    /// - [`Error::NameTooLong`] / [`Error::EntryExists`] before anything changes.
    /// - [`Error::NotSupported`] if all 12 direct blocks of `dir` are full.
    /// - [`Error::NoSpace`] if a new directory block cannot be allocated.
    /// - [`Error::InvalidDirEntry`] / [`Error::InvalidDirEntryType`] for a
    ///   corrupt directory block, and device errors.
    pub fn add_entry_to_dir(
        &mut self,
        dir: &mut Directory,
        name: &str,
        inode_address: InodeAddress,
        typ: DirType,
    ) -> Result<(), Error> {
        if name.len() > MAX_NAME_LEN {
            return Err(Error::NameTooLong);
        }
        if self.find_entry(dir, |e| e.name() == Some(name))?.is_some() {
            return Err(Error::EntryExists);
        }

        let block_size = self.superblock.block_size() as usize;
        let dir_entries_have_type = self.superblock.dir_entries_have_type();

        let required_size = DirEntry::size(name.len() as u16);
        let new_entry = |total_size| DirEntry {
            inode: inode_address,
            total_size,
            type_indicator: dir_entries_have_type.then_some(typ),
            name_bytes: name.as_bytes().to_vec(),
        };

        for block in dir.direct_ptrs().flatten() {
            let mut block_data = vec![0_u8; block_size];
            self.read_block(block, &mut block_data)?;

            // Every entry owns the slack up to the next one, so the new entry is
            // carved out of the first entry whose slack can hold it.
            let mut offset = 0;
            while offset < block_size - 8 {
                let record = DirRecord::parse(dir_entries_have_type, &block_data[offset..])?;
                let old_total_size = record.total_size();
                // a deleted record is reused whole, a live one gives up its slack
                let kept_size = match &record {
                    DirRecord::Used(entry) => DirEntry::size(entry.name_bytes.len() as u16),
                    DirRecord::Unused { .. } => 0,
                };
                if old_total_size >= required_size + kept_size {
                    if let DirRecord::Used(mut entry) = record {
                        entry.total_size = kept_size;
                        let entry_serialized = entry.serialize();
                        block_data[offset..offset + entry_serialized.len()]
                            .copy_from_slice(&entry_serialized);
                    }
                    let new_entry_offset = offset + usize::from(kept_size);
                    let new_entry = new_entry(old_total_size - kept_size);

                    let new_entry_serialized = new_entry.serialize();
                    block_data[new_entry_offset..new_entry_offset + new_entry_serialized.len()]
                        .copy_from_slice(&new_entry_serialized);

                    self.write_block(block, &block_data)?;

                    return Ok(());
                }

                offset += usize::from(old_total_size);
            }
        }

        // shortcut: a directory grows through its 12 direct blocks only. Walk
        // the indirect tiers here once directories need more entries.
        let Some(slot) = dir.direct_ptrs().position(|ptr| ptr.is_none()) else {
            return Err(Error::NotSupported);
        };
        let total_size = u16::try_from(block_size).map_err(|_| Error::NotSupported)?;
        let mut block_data = new_entry(total_size).serialize();
        block_data.resize(block_size, 0);
        let block = self.allocate_counted(dir.inode_mut())?;
        self.write_block(block, &block_data)?;
        let inode = dir.inode_mut();
        inode.set_direct_ptr(slot, Some(block));
        inode.set_len((slot + 1) * block_size);
        self.write_inode(dir.inode_address(), dir)
    }

    // `..` owns the rest of the block, so later entries are carved out of it
    pub(crate) fn empty_dir_block(
        &self,
        own: InodeAddress,
        parent: InodeAddress,
    ) -> Result<Vec<u8>, Error> {
        let block_size = self.superblock.block_size() as usize;
        let dir_entries_have_type = self.superblock.dir_entries_have_type();
        let dot_size = DirEntry::size(1);
        let dot_dot_size = u16::try_from(block_size)
            .map_err(|_| Error::NotSupported)?
            .saturating_sub(dot_size);
        let entry = |inode, total_size, name: &[u8]| DirEntry {
            inode,
            total_size,
            type_indicator: dir_entries_have_type.then_some(DirType::Directory),
            name_bytes: name.to_vec(),
        };
        let mut data = entry(own, dot_size, b".").serialize();
        // serialize emits header and name only, so the record is padded by hand
        data.resize(usize::from(dot_size), 0);
        data.extend(entry(parent, dot_dot_size, b"..").serialize());
        data.resize(block_size, 0);
        Ok(data)
    }
}

pub struct DirEntry {
    inode: InodeAddress,
    total_size: u16,
    type_indicator: Option<DirType>,
    name_bytes: Vec<u8>,
}

impl DirEntry {
    const fn size(name_length: u16) -> u16 {
        let unaligned_size = 4 + // inode
            2 + // total_size
            2 + // name_length and type_indicator
            name_length;
        // align up to 4 byte
        (unaligned_size + 3) & !3
    }

    pub fn serialize(self) -> Vec<u8> {
        let name_length = self.name_bytes.len() as u16;
        let mut result = Vec::with_capacity(Self::size(name_length) as usize);

        let mut entry_no_name = DirEntryNoName::try_from([0; 8]).unwrap();
        entry_no_name.inode = self.inode.into();
        entry_no_name.total_size = self.total_size;
        entry_no_name.name_length_lsb = name_length as u8;
        entry_no_name.type_indicator_or_name_length_msb = match self.type_indicator {
            Some(typ) => typ as u8,
            None => (name_length >> 8) as u8,
        };

        result.extend_from_slice(&<[u8; 8]>::from(entry_no_name));
        result.extend_from_slice(&self.name_bytes);
        result
    }

    pub fn name(&self) -> Option<&str> {
        core::str::from_utf8(&self.name_bytes).ok()
    }

    pub fn typ(&self) -> Option<DirType> {
        self.type_indicator
    }

    pub fn inode(&self) -> InodeAddress {
        self.inode
    }
}

enum DirRecord {
    Used(DirEntry),
    // a deleted entry still owns its bytes up to the next record
    Unused { total_size: u16 },
}

impl DirRecord {
    fn total_size(&self) -> u16 {
        match self {
            Self::Used(entry) => entry.total_size,
            Self::Unused { total_size } => *total_size,
        }
    }

    // a record owns its bytes up to the next one, so a length that does not fit
    // the block or its own name is a corrupt block, not a short record
    fn parse(dir_entries_have_type: bool, value: &[u8]) -> Result<Self, Error> {
        let header = value.first_chunk().ok_or(Error::InvalidDirEntry)?;
        let arr = DirEntryNoName::try_from(header).map_err(|()| Error::InvalidDirEntry)?;
        let name_length = if dir_entries_have_type {
            u16::from(arr.name_length_lsb)
        } else {
            u16::from(arr.name_length_lsb) | (u16::from(arr.type_indicator_or_name_length_msb) << 8)
        };
        let total_size = arr.total_size;
        if usize::from(name_length) > MAX_NAME_LEN
            || total_size % 4 != 0
            || total_size < DirEntry::size(name_length)
            || usize::from(total_size) > value.len()
        {
            return Err(Error::InvalidDirEntry);
        }

        let Some(inode) = InodeAddress::new(arr.inode) else {
            return Ok(Self::Unused { total_size });
        };
        let type_indicator = if dir_entries_have_type {
            let code = arr.type_indicator_or_name_length_msb;
            Some(DirType::try_from(code).map_err(|()| Error::InvalidDirEntryType(code))?)
        } else {
            None
        };
        Ok(Self::Used(DirEntry {
            inode,
            total_size,
            type_indicator,
            name_bytes: value[8..8 + usize::from(name_length)].to_vec(),
        }))
    }
}

impl Debug for DirEntry {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        let mut debug_struct = f.debug_struct("DirEntry");
        debug_struct
            .field("inode", &self.inode)
            .field("total_size", &self.total_size)
            .field("name_length", &self.name_bytes.len());
        if let Some(type_indicator) = &self.type_indicator {
            debug_struct.field("type_indicator", &type_indicator);
        }
        debug_struct.field("name", &core::str::from_utf8(&self.name_bytes));
        debug_struct.field("name_bytes", &self.name_bytes);

        debug_struct.finish()
    }
}

bytefield! {
    pub struct DirEntryNoName ([u8; 8]) {
        inode: u32 = 0,
        total_size: u16 = 4,
        name_length_lsb: u8 = 6,
        type_indicator_or_name_length_msb: u8 = 7,
    }
}

/// On-disk directory entry type codes (`EXT2_FT_*`).
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
#[repr(u8)]
pub enum DirType {
    Unknown = 0,
    RegularFile = 1,
    Directory = 2,
    CharacterDevice = 3,
    BlockDevice = 4,
    FIFO = 5,
    UnixSocket = 6,
    SymLink = 7,
}

impl TryFrom<u8> for DirType {
    type Error = ();

    fn try_from(code: u8) -> Result<Self, ()> {
        Ok(match code {
            0 => Self::Unknown,
            1 => Self::RegularFile,
            2 => Self::Directory,
            3 => Self::CharacterDevice,
            4 => Self::BlockDevice,
            5 => Self::FIFO,
            6 => Self::UnixSocket,
            7 => Self::SymLink,
            _ => return Err(()),
        })
    }
}

impl From<Type> for DirType {
    fn from(value: Type) -> Self {
        match value {
            Type::RegularFile => Self::RegularFile,
            Type::Directory => Self::Directory,
            Type::CharacterDevice => Self::CharacterDevice,
            Type::BlockDevice => Self::BlockDevice,
            Type::FIFO => Self::FIFO,
            Type::UnixSocket => Self::UnixSocket,
            Type::SymLink => Self::SymLink,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Both entries point at inode 12 with a 16-byte record named "a.txt". Only
    // byte 7 differs: 0 is the name-length high byte, 1 is the regular-file type.
    const UNTYPED: [u8; 16] = [
        12, 0, 0, 0, 16, 0, 5, 0, b'a', b'.', b't', b'x', b't', 0, 0, 0,
    ];
    const TYPED: [u8; 16] = [
        12, 0, 0, 0, 16, 0, 5, 1, b'a', b'.', b't', b'x', b't', 0, 0, 0,
    ];

    fn parse_used(dir_entries_have_type: bool, value: &[u8]) -> DirEntry {
        match DirRecord::parse(dir_entries_have_type, value).unwrap() {
            DirRecord::Used(entry) => entry,
            DirRecord::Unused { total_size } => panic!("unused record of {total_size} bytes"),
        }
    }

    #[test]
    fn serialize_round_trips_both_layouts() {
        let untyped = parse_used(false, &UNTYPED);
        assert_eq!(None, untyped.typ());
        assert_eq!(&UNTYPED[..13], &untyped.serialize()[..]);

        let typed = parse_used(true, &TYPED);
        assert_eq!(Some(DirType::RegularFile), typed.typ());
        assert_eq!(&TYPED[..13], &typed.serialize()[..]);
    }
}
