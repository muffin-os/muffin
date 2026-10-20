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
                let dir_entry = DirEntry::from(dir_entries_have_type, &data[offset..])?;
                // we don't need to align the offset, as there must be no space between entries
                offset += dir_entry.total_size as usize;
                entries.push(dir_entry);
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

        let inode = &mut dir.inode_mut();

        let required_size = DirEntry::size(name.len() as u16);

        for block in inode.direct_ptrs().flatten() {
            let mut block_data = vec![0_u8; block_size];
            self.read_block(block, &mut block_data)?;

            // Every entry owns the slack up to the next one, so the new entry is
            // carved out of the first entry whose slack can hold it.
            let mut offset = 0;
            while offset < block_size - 8 {
                debug_assert_eq!(offset % 4, 0, "offset is not aligned");

                let mut entry = DirEntry::from(dir_entries_have_type, &block_data[offset..])?;
                let entry_size = DirEntry::size(entry.name_bytes.len() as u16);
                if entry.total_size >= required_size + entry_size {
                    let old_total_size = entry.total_size;
                    entry.total_size = entry_size;

                    let entry_serialized = entry.serialize(dir_entries_have_type);
                    block_data[offset..offset + entry_serialized.len()]
                        .copy_from_slice(&entry_serialized);

                    let new_entry_total_size = old_total_size - entry_size;
                    let new_entry_offset = offset + entry_size as usize;
                    debug_assert_eq!(new_entry_offset % 4, 0, "new entry offset is not aligned");

                    let new_entry = DirEntry {
                        inode: inode_address,
                        total_size: new_entry_total_size,
                        type_indicator: if dir_entries_have_type {
                            Some(typ)
                        } else {
                            None
                        },
                        name_bytes: name.as_bytes().to_vec(),
                    };

                    let new_entry_serialized = new_entry.serialize(dir_entries_have_type);
                    block_data[new_entry_offset..new_entry_offset + new_entry_serialized.len()]
                        .copy_from_slice(&new_entry_serialized);

                    self.write_block(block, &block_data)?;

                    return Ok(());
                }

                offset += entry.total_size as usize;
            }
        }

        todo!("add_entry_to_dir with indirect pointers")
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

    fn from(dir_entries_have_type: bool, value: &[u8]) -> Result<Self, Error> {
        let required_size = Self::size(0);
        debug_assert!(
            value.len() >= required_size as usize,
            "need at least {required_size} byte, but got {}",
            value.len()
        );

        let arr = DirEntryNoName::try_from(&value[0..8].try_into().unwrap()).unwrap();
        let name_length = if dir_entries_have_type {
            arr.name_length_lsb as u16
        } else {
            arr.name_length_lsb as u16 | ((arr.type_indicator_or_name_length_msb as u16) << 8)
        };
        let type_indicator = if dir_entries_have_type {
            let code = arr.type_indicator_or_name_length_msb;
            Some(DirType::try_from(code).map_err(|()| Error::InvalidDirEntryType(code))?)
        } else {
            None
        };

        let required_size = Self::size(name_length);
        debug_assert!(
            value.len() >= Self::size(name_length) as usize,
            "need at least {} byte, but have only {}",
            required_size,
            value.len()
        );

        let name_bytes = value[8..8 + name_length as usize].to_vec();
        Ok(Self {
            inode: InodeAddress::new(arr.inode).unwrap(),
            total_size: arr.total_size,
            type_indicator,
            name_bytes,
        })
    }

    pub fn serialize(self, dir_entries_have_type: bool) -> Vec<u8> {
        let name_length = self.name_bytes.len() as u16;
        let mut result = Vec::with_capacity(Self::size(name_length) as usize);

        let mut entry_no_name = DirEntryNoName::try_from([0; 8]).unwrap();
        entry_no_name.inode = self.inode.into();
        entry_no_name.total_size = self.total_size;
        entry_no_name.name_length_lsb = name_length as u8;
        if dir_entries_have_type {
            entry_no_name.type_indicator_or_name_length_msb = self.type_indicator.unwrap() as u8;
        } else {
            entry_no_name.type_indicator_or_name_length_msb = (name_length >> 8) as u8;
        }

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
