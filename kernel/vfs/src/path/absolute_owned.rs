use core::borrow::Borrow;
use core::ops::{Deref, DerefMut};

use crate::path::{AbsolutePath, OwnedPath, PathNotAbsoluteError};

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(transparent)]
pub struct AbsoluteOwnedPath {
    inner: OwnedPath,
}

impl Default for AbsoluteOwnedPath {
    fn default() -> Self {
        Self::new()
    }
}

impl AbsoluteOwnedPath {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: OwnedPath::new("/"),
        }
    }

    /// # Safety
    /// `inner` must start with [`crate::path::FILEPATH_SEPARATOR`].
    pub(crate) unsafe fn new_unchecked(inner: OwnedPath) -> Self {
        Self { inner }
    }
}

impl TryFrom<&str> for AbsoluteOwnedPath {
    type Error = PathNotAbsoluteError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let path = OwnedPath::new(value);
        path.try_into()
    }
}

impl Deref for AbsoluteOwnedPath {
    type Target = OwnedPath;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl Borrow<AbsolutePath> for AbsoluteOwnedPath {
    fn borrow(&self) -> &AbsolutePath {
        unsafe { AbsolutePath::new_unchecked(&self.inner) }
    }
}

impl DerefMut for AbsoluteOwnedPath {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl TryFrom<OwnedPath> for AbsoluteOwnedPath {
    type Error = PathNotAbsoluteError;

    fn try_from(value: OwnedPath) -> Result<Self, Self::Error> {
        if value.is_absolute() {
            Ok(AbsoluteOwnedPath { inner: value })
        } else {
            Err(PathNotAbsoluteError)
        }
    }
}

impl AsRef<AbsolutePath> for AbsoluteOwnedPath {
    fn as_ref(&self) -> &AbsolutePath {
        unsafe { AbsolutePath::new_unchecked(&self.inner) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::Path;

    #[test]
    fn test_new() {
        let path = AbsoluteOwnedPath::new();
        assert_eq!(path.as_str(), "/");
        assert!(path.is_absolute());
    }

    #[test]
    fn test_try_from_str_valid() {
        let path: Result<AbsoluteOwnedPath, _> = "/".try_into();
        assert!(path.is_ok());
        assert_eq!(path.unwrap().as_str(), "/");

        let path: Result<AbsoluteOwnedPath, _> = "/foo".try_into();
        assert!(path.is_ok());
        assert_eq!(path.unwrap().as_str(), "/foo");

        let path: Result<AbsoluteOwnedPath, _> = "/foo/bar".try_into();
        assert!(path.is_ok());
        assert_eq!(path.unwrap().as_str(), "/foo/bar");

        let path: Result<AbsoluteOwnedPath, _> = "//foo".try_into();
        assert!(path.is_ok());
        assert_eq!(path.unwrap().as_str(), "//foo");
    }

    #[test]
    fn test_try_from_str_invalid() {
        let path: Result<AbsoluteOwnedPath, _> = "".try_into();
        assert!(path.is_err());

        let path: Result<AbsoluteOwnedPath, _> = "foo".try_into();
        assert!(path.is_err());

        let path: Result<AbsoluteOwnedPath, _> = "foo/bar".try_into();
        assert!(path.is_err());

        let path: Result<AbsoluteOwnedPath, _> = "./foo".try_into();
        assert!(path.is_err());

        let path: Result<AbsoluteOwnedPath, _> = "../foo".try_into();
        assert!(path.is_err());
    }

    #[test]
    fn test_mixed_operations() {
        let mut path = AbsoluteOwnedPath::new();
        path.push("home");
        path.push("user");
        path.push("file");
        path.append_str(".txt");
        assert_eq!(path.as_str(), "/home/user/file.txt");
    }

    #[test]
    fn test_parent_navigation() {
        let path: AbsoluteOwnedPath = "/foo/bar/baz".try_into().unwrap();
        assert_eq!(path.parent(), Some(Path::new("/foo/bar")));

        let path: AbsoluteOwnedPath = "/foo".try_into().unwrap();
        assert_eq!(path.parent(), Some(Path::new("/")));

        let path = AbsoluteOwnedPath::new();
        assert_eq!(path.parent(), None);
    }

    #[test]
    fn test_file_name() {
        let path: AbsoluteOwnedPath = "/foo/bar".try_into().unwrap();
        assert_eq!(path.file_name(), Some("bar"));

        let path: AbsoluteOwnedPath = "/foo".try_into().unwrap();
        assert_eq!(path.file_name(), Some("foo"));

        let path = AbsoluteOwnedPath::new();
        assert_eq!(path.file_name(), None);
    }

    #[test]
    fn test_always_absolute() {
        let mut path = AbsoluteOwnedPath::new();
        assert!(path.is_absolute());

        path.push("foo");
        assert!(path.is_absolute());

        path.push("bar");
        assert!(path.is_absolute());

        path.append_str("baz");
        assert!(path.is_absolute());
    }
}
