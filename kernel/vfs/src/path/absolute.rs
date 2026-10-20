use alloc::borrow::ToOwned;
use core::fmt::{Display, Formatter};
use core::ops::Deref;
use core::ptr;

use crate::path::{AbsoluteOwnedPath, Path, PathNotAbsoluteError};

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(transparent)]
pub struct AbsolutePath {
    inner: Path,
}

impl Display for AbsolutePath {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        Display::fmt(&self.inner, f)
    }
}

impl AbsolutePath {
    /// Creates a new [`AbsolutePath`] from a string slice.
    ///
    /// # Errors
    /// Returns an error if the path is not absolute.
    pub fn try_new(path: &str) -> Result<&Self, PathNotAbsoluteError> {
        path.try_into()
    }

    /// # Safety
    /// `path` must start with [`crate::path::FILEPATH_SEPARATOR`].
    pub(crate) unsafe fn new_unchecked(path: &Path) -> &Self {
        unsafe { &*(ptr::from_ref::<Path>(path) as *const AbsolutePath) }
    }

    #[must_use]
    pub fn parent(&self) -> Option<&AbsolutePath> {
        // Safety: the parent keeps the leading separator of an absolute path.
        self.inner
            .parent()
            .map(|v| unsafe { AbsolutePath::new_unchecked(v) })
    }

    /// Rebuilds the path from its components, so `//a//b/` becomes `/a/b`.
    #[must_use]
    pub fn normalized(&self) -> AbsoluteOwnedPath {
        let mut path = AbsoluteOwnedPath::new();
        self.filenames().for_each(|name| path.push(name));
        path
    }
}

impl Deref for AbsolutePath {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl AsRef<AbsolutePath> for AbsolutePath {
    fn as_ref(&self) -> &AbsolutePath {
        self
    }
}

impl TryFrom<&str> for &AbsolutePath {
    type Error = PathNotAbsoluteError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Path::new(value).try_into()
    }
}

impl TryFrom<&Path> for &AbsolutePath {
    type Error = PathNotAbsoluteError;

    fn try_from(value: &Path) -> Result<Self, Self::Error> {
        if value.is_absolute() {
            Ok(unsafe { &*(ptr::from_ref::<Path>(value) as *const AbsolutePath) })
        } else {
            Err(PathNotAbsoluteError)
        }
    }
}

impl ToOwned for AbsolutePath {
    type Owned = AbsoluteOwnedPath;

    fn to_owned(&self) -> Self::Owned {
        unsafe { AbsoluteOwnedPath::new_unchecked(self.inner.to_owned()) }
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use super::*;

    #[test]
    fn test_try_new_valid() {
        assert!(AbsolutePath::try_new("/").is_ok());
        assert!(AbsolutePath::try_new("//").is_ok());
        assert!(AbsolutePath::try_new("///").is_ok());
        assert!(AbsolutePath::try_new("/foo").is_ok());
        assert!(AbsolutePath::try_new("/foo/bar").is_ok());
        assert!(AbsolutePath::try_new("/foo/bar/baz").is_ok());

        assert!(AbsolutePath::try_new("/foo-bar").is_ok());
        assert!(AbsolutePath::try_new("/foo_bar").is_ok());
        assert!(AbsolutePath::try_new("/foo.bar").is_ok());
        assert!(AbsolutePath::try_new("/foo bar").is_ok());

        assert!(AbsolutePath::try_new("/.").is_ok());
        assert!(AbsolutePath::try_new("/..").is_ok());
        assert!(AbsolutePath::try_new("/.hidden").is_ok());
    }

    #[test]
    fn test_try_new_invalid() {
        assert!(AbsolutePath::try_new("").is_err());
        assert!(AbsolutePath::try_new("foo").is_err());
        assert!(AbsolutePath::try_new("foo/bar").is_err());
        assert!(AbsolutePath::try_new("./foo").is_err());
        assert!(AbsolutePath::try_new("../foo").is_err());
        assert!(AbsolutePath::try_new("foo/").is_err());
        assert!(AbsolutePath::try_new(" /foo").is_err());
    }

    #[test]
    fn test_parent_absolute() {
        let path: &AbsolutePath = "/foo/bar".try_into().unwrap();
        let parent = path.parent();
        assert!(parent.is_some());
        assert_eq!(&parent.unwrap().to_string(), "/foo");

        let path: &AbsolutePath = "/foo".try_into().unwrap();
        let parent = path.parent();
        assert_eq!(&parent.unwrap().to_string(), "/");

        let path: &AbsolutePath = "/".try_into().unwrap();
        let parent = path.parent();
        assert!(parent.is_none());

        let path: &AbsolutePath = "//foo//bar".try_into().unwrap();
        let parent = path.parent();
        assert!(parent.is_some());
        assert_eq!(&parent.unwrap().to_string(), "//foo");

        let path: &AbsolutePath = "//foo".try_into().unwrap();
        let parent = path.parent();
        assert_eq!(&parent.unwrap().to_string(), "/");

        let path: &AbsolutePath = "/foo/bar/".try_into().unwrap();
        let parent = path.parent();
        assert!(parent.is_some());
        assert_eq!(&parent.unwrap().to_string(), "/foo");

        let path: &AbsolutePath = "/a/b/c/d/e/f".try_into().unwrap();
        let parent = path.parent();
        assert!(parent.is_some());
        assert_eq!(&parent.unwrap().to_string(), "/a/b/c/d/e");
    }
}
