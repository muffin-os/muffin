use kernel_device::block::BlockDevice;

use crate::{Directory, Error, Ext2Fs, Inode, InodeAddress, RegularFile, Resource, Type};

impl<T> Ext2Fs<T>
where
    T: BlockDevice,
{
    /// Allocates an inode of type `typ` with one link and adds it to `parent`
    /// as `name`. If the entry cannot be added, the inode is freed again.
    ///
    /// # Errors
    /// - [`Error::NoSpace`] if no inode is free.
    /// - Every error of [`Ext2Fs::add_entry_to_dir`].
    /// - [`Error::DeviceWrite`] if the inode cannot be written.
    pub fn create_inode(
        &mut self,
        parent: &mut Directory,
        name: &str,
        typ: Type,
    ) -> Result<(InodeAddress, Inode), Error> {
        let inode_address = self.allocate_inode()?.ok_or(Error::NoSpace)?;
        if let Err(e) = self.add_entry_to_dir(parent, name, inode_address, typ.into()) {
            self.deallocate(Resource::Inode, inode_address.get())?;
            return Err(e);
        }

        let mut inode = Inode::new(typ);
        // the entry just added in `parent`
        *inode.num_hard_links_mut() = 1;
        self.write_inode(inode_address, &inode)?;
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
}
