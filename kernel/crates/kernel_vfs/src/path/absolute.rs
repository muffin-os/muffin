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
        write!(f, "{}", &self.inner)
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

    pub(crate) unsafe fn new_unchecked(path: &Path) -> &Self {
        unsafe { &*(ptr::from_ref::<Path>(path) as *const AbsolutePath) }
    }

    #[must_use]
    pub fn parent(&self) -> Option<&AbsolutePath> {
        self.inner
            .parent()
            .map(|v| unsafe { AbsolutePath::new_unchecked(v) })
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
        // Basic absolute paths
        assert!(AbsolutePath::try_new("/").is_ok());
        assert!(AbsolutePath::try_new("//").is_ok());
        assert!(AbsolutePath::try_new("///").is_ok());
        assert!(AbsolutePath::try_new("/foo").is_ok());
        assert!(AbsolutePath::try_new("/foo/bar").is_ok());
        assert!(AbsolutePath::try_new("/foo/bar/baz").is_ok());

        // Paths with special characters
        assert!(AbsolutePath::try_new("/foo-bar").is_ok());
        assert!(AbsolutePath::try_new("/foo_bar").is_ok());
        assert!(AbsolutePath::try_new("/foo.bar").is_ok());
        assert!(AbsolutePath::try_new("/foo bar").is_ok());

        // Paths with dots
        assert!(AbsolutePath::try_new("/.").is_ok());
        assert!(AbsolutePath::try_new("/..").is_ok());
        assert!(AbsolutePath::try_new("/.hidden").is_ok());
    }

    #[test]
    fn test_try_new_invalid() {
        // Relative paths should fail
        assert!(AbsolutePath::try_new("").is_err());
        assert!(AbsolutePath::try_new("foo").is_err());
        assert!(AbsolutePath::try_new("foo/bar").is_err());
        assert!(AbsolutePath::try_new("./foo").is_err());
        assert!(AbsolutePath::try_new("../foo").is_err());
        assert!(AbsolutePath::try_new("foo/").is_err());
        assert!(AbsolutePath::try_new(" /foo").is_err());
    }

    #[test]
    fn test_try_from_str() {
        // Valid conversions
        let result: Result<&AbsolutePath, _> = "/".try_into();
        assert!(result.is_ok());

        let result: Result<&AbsolutePath, _> = "/foo/bar".try_into();
        assert!(result.is_ok());

        // Invalid conversions
        let result: Result<&AbsolutePath, _> = "".try_into();
        assert!(result.is_err());

        let result: Result<&AbsolutePath, _> = "foo".try_into();
        assert!(result.is_err());

        let result: Result<&AbsolutePath, _> = "foo/bar".try_into();
        assert!(result.is_err());
    }

    #[test]
    fn test_try_from_path() {
        // Valid conversions
        let path = Path::new("/foo/bar");
        let result: Result<&AbsolutePath, _> = path.try_into();
        assert!(result.is_ok());

        // Invalid conversions
        let path = Path::new("foo/bar");
        let result: Result<&AbsolutePath, _> = path.try_into();
        assert!(result.is_err());

        let path = Path::new("");
        let result: Result<&AbsolutePath, _> = path.try_into();
        assert!(result.is_err());
    }

    #[test]
    fn test_parent_absolute() {
        // Basic parent tests
        let path: &AbsolutePath = "/foo/bar".try_into().unwrap();
        let parent = path.parent();
        assert!(parent.is_some());
        assert_eq!(&parent.unwrap().to_string(), "/foo");

        let path: &AbsolutePath = "/foo".try_into().unwrap();
        let parent = path.parent();
        assert!(parent.is_none());

        let path: &AbsolutePath = "/".try_into().unwrap();
        let parent = path.parent();
        assert!(parent.is_none());

        // Multiple slashes
        let path: &AbsolutePath = "//foo//bar".try_into().unwrap();
        let parent = path.parent();
        assert!(parent.is_some());
        assert_eq!(&parent.unwrap().to_string(), "//foo");

        // Double slash path
        let path: &AbsolutePath = "//foo".try_into().unwrap();
        let parent = path.parent();
        assert!(parent.is_none());

        // Trailing slashes
        let path: &AbsolutePath = "/foo/bar/".try_into().unwrap();
        let parent = path.parent();
        assert!(parent.is_some());
        assert_eq!(&parent.unwrap().to_string(), "/foo");

        // Deeply nested
        let path: &AbsolutePath = "/a/b/c/d/e/f".try_into().unwrap();
        let parent = path.parent();
        assert!(parent.is_some());
        assert_eq!(&parent.unwrap().to_string(), "/a/b/c/d/e");
    }

    #[test]
    fn test_deref_to_path() {
        let abs_path: &AbsolutePath = "/foo/bar".try_into().unwrap();
        let path: &Path = &**abs_path;
        assert_eq!(&**path, "/foo/bar");
    }

    #[test]
    fn test_display() {
        use alloc::format;

        let path: &AbsolutePath = "/foo/bar".try_into().unwrap();
        assert_eq!(format!("{}", path), "/foo/bar");

        let path: &AbsolutePath = "/".try_into().unwrap();
        assert_eq!(format!("{}", path), "/");

        let path: &AbsolutePath = "//foo".try_into().unwrap();
        assert_eq!(format!("{}", path), "//foo");
    }

    #[test]
    fn test_to_owned() {
        let path: &AbsolutePath = "/foo/bar".try_into().unwrap();
        let owned = path.to_owned();
        assert_eq!(owned.as_str(), "/foo/bar");

        let path: &AbsolutePath = "/".try_into().unwrap();
        let owned = path.to_owned();
        assert_eq!(owned.as_str(), "/");
    }
}
