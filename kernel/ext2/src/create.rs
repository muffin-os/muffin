use kernel_device::block::BlockDevice;

use crate::{DirType, Directory, Error, Ext2Fs, Inode, InodeAddress, RegularFile, Resource, Type};

impl<T> Ext2Fs<T>
where
    T: BlockDevice,
{
    /// Allocates an inode of type `typ` with one link, writes it, and then
    /// adds it to `parent` as `name`. If the inode cannot be written or the
    /// entry cannot be added, the inode is freed again.
    ///
    /// A directory needs its first block, so [`Type::Directory`] is handed to
    /// [`Ext2Fs::create_directory`].
    ///
    /// # Errors
    /// - [`Error::NoSpace`] if no inode is free.
    /// - [`Error::DeviceWrite`] if the inode cannot be written.
    /// - Every error of [`Ext2Fs::add_entry_to_dir`].
    /// - The deallocator's errors if freeing the inode after a failure fails.
    pub fn create_inode(
        &mut self,
        parent: &mut Directory,
        name: &str,
        typ: Type,
    ) -> Result<(InodeAddress, Inode), Error> {
        if typ == Type::Directory {
            return self
                .create_directory(parent, name)
                .map(Directory::into_inner);
        }
        let inode_address = self.allocate_inode()?.ok_or(Error::NoSpace)?;
        let mut inode = Inode::new(typ);
        // the entry about to be added in `parent`
        *inode.num_hard_links_mut() = 1;
        if let Err(e) = self.link_inode(parent, name, inode_address, &inode, typ.into()) {
            self.deallocate(Resource::Inode, inode_address.get())?;
            return Err(e);
        }
        Ok((inode_address, inode))
    }

    /// [`Ext2Fs::create_inode`] for a regular file.
    ///
    /// # Errors
    /// Every error of [`Ext2Fs::create_inode`].
    pub fn create_regular_file(
        &mut self,
        parent: &mut Directory,
        name: &str,
    ) -> Result<RegularFile, Error> {
        self.create_inode(parent, name, Type::RegularFile)
            .and_then(|created| RegularFile::try_from(created).map_err(|_| Error::NotRegularFile))
    }

    /// Creates an empty directory named `name` in `parent`: one block holding
    /// `.` and `..`, two links, and one more link on `parent`. If anything
    /// fails before the entry exists in `parent`, the inode and block are
    /// freed again.
    ///
    /// # Errors
    /// - [`Error::NotSupported`] if `parent` already has the most links a
    ///   directory can have, or the block size does not fit a directory entry.
    /// - [`Error::NoSpace`] if no inode or no block is free.
    /// - [`Error::DeviceWrite`] if the block or an inode cannot be written.
    /// - Every error of [`Ext2Fs::add_entry_to_dir`].
    /// - The deallocator's errors if freeing after a failure fails.
    pub fn create_directory(
        &mut self,
        parent: &mut Directory,
        name: &str,
    ) -> Result<Directory, Error> {
        let parent_links = parent
            .num_hard_links()
            .checked_add(1)
            .ok_or(Error::NotSupported)?;
        let inode_address = self.allocate_inode()?.ok_or(Error::NoSpace)?;
        let mut inode = Inode::new(Type::Directory);
        // `.` and the entry about to be added in `parent`
        *inode.num_hard_links_mut() = 2;
        let block = match self.allocate_counted(&mut inode) {
            Ok(block) => block,
            Err(e) => {
                self.deallocate(Resource::Inode, inode_address.get())?;
                return Err(e);
            }
        };
        inode.set_direct_ptr(0, Some(block));
        inode.set_len(self.superblock.block_size() as usize);
        let linked = self
            .empty_dir_block(inode_address, parent.inode_address())
            .and_then(|data| self.write_block(block, &data))
            .and_then(|_| self.link_inode(parent, name, inode_address, &inode, DirType::Directory));
        if let Err(e) = linked {
            self.deallocate_block(block)?;
            self.deallocate(Resource::Inode, inode_address.get())?;
            return Err(e);
        }
        *parent.inode_mut().num_hard_links_mut() = parent_links;
        self.write_inode(parent.inode_address(), parent)?;
        let group_index = ((inode_address.get() - 1) / self.superblock.inodes_per_group()) as usize;
        let descriptor = self
            .bgdt
            .get_mut(group_index)
            .ok_or(Error::InvalidInodeAddress(inode_address.get()))?;
        *descriptor.num_directories_mut() = descriptor.num_directories().saturating_add(1);
        self.persist_counters(group_index)?;
        Directory::try_from((inode_address, inode)).map_err(|_| Error::NotDirectory)
    }

    // the inode goes to disk before the entry, so a live entry never points at nothing
    fn link_inode(
        &mut self,
        parent: &mut Directory,
        name: &str,
        inode_address: InodeAddress,
        inode: &Inode,
        typ: DirType,
    ) -> Result<(), Error> {
        self.write_inode(inode_address, inode)?;
        self.add_entry_to_dir(parent, name, inode_address, typ)
    }
}
