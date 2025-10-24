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
    use alloc::string::ToString;

    use super::*;
    use crate::path::Path;

    #[test]
    fn test_new() {
        let path = AbsoluteOwnedPath::new();
        assert_eq!(path.as_str(), "/");
        assert!(path.is_absolute());
    }

    #[test]
    fn test_default() {
        let path = AbsoluteOwnedPath::default();
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
    fn test_try_from_owned_path_valid() {
        let owned = OwnedPath::new("/");
        let path: Result<AbsoluteOwnedPath, _> = owned.try_into();
        assert!(path.is_ok());
        assert_eq!(path.unwrap().as_str(), "/");

        let owned = OwnedPath::new("/foo/bar");
        let path: Result<AbsoluteOwnedPath, _> = owned.try_into();
        assert!(path.is_ok());
        assert_eq!(path.unwrap().as_str(), "/foo/bar");
    }

    #[test]
    fn test_try_from_owned_path_invalid() {
        let owned = OwnedPath::new("");
        let path: Result<AbsoluteOwnedPath, _> = owned.try_into();
        assert!(path.is_err());

        let owned = OwnedPath::new("foo");
        let path: Result<AbsoluteOwnedPath, _> = owned.try_into();
        assert!(path.is_err());

        let owned = OwnedPath::new("foo/bar");
        let path: Result<AbsoluteOwnedPath, _> = owned.try_into();
        assert!(path.is_err());
    }

    #[test]
    fn test_deref() {
        let abs_path = AbsoluteOwnedPath::new();
        let owned: &OwnedPath = &*abs_path;
        assert_eq!(owned.as_str(), "/");

        // Should have access to OwnedPath methods
        assert!(abs_path.is_absolute());
        assert_eq!(abs_path.file_name(), None);
    }

    #[test]
    fn test_deref_mut() {
        let mut abs_path = AbsoluteOwnedPath::new();
        abs_path.push("foo");
        assert_eq!(abs_path.as_str(), "/foo");

        abs_path.push("bar");
        assert_eq!(abs_path.as_str(), "/foo/bar");

        abs_path.append_str(".txt");
        assert_eq!(abs_path.as_str(), "/foo/bar.txt");
    }

    #[test]
    fn test_as_ref_absolute_path() {
        let abs_owned = AbsoluteOwnedPath::new();
        let abs_path: &AbsolutePath = abs_owned.as_ref();
        assert_eq!(&abs_path.to_string(), "/");
    }

    #[test]
    fn test_borrow() {
        use core::borrow::Borrow;

        let abs_owned: AbsoluteOwnedPath = "/foo/bar".try_into().unwrap();
        let borrowed: &AbsolutePath = abs_owned.borrow();
        assert_eq!(&borrowed.to_string(), "/foo/bar");
    }

    #[test]
    fn test_clone() {
        let path: AbsoluteOwnedPath = "/foo/bar".try_into().unwrap();
        let cloned = path.clone();
        assert_eq!(path, cloned);
        assert_eq!(cloned.as_str(), "/foo/bar");
    }

    #[test]
    fn test_eq() {
        let path1: AbsoluteOwnedPath = "/foo/bar".try_into().unwrap();
        let path2: AbsoluteOwnedPath = "/foo/bar".try_into().unwrap();
        let path3: AbsoluteOwnedPath = "/foo/baz".try_into().unwrap();

        assert_eq!(path1, path2);
        assert_ne!(path1, path3);
    }

    #[test]
    fn test_ord() {
        let path1: AbsoluteOwnedPath = "/a".try_into().unwrap();
        let path2: AbsoluteOwnedPath = "/b".try_into().unwrap();
        let path3: AbsoluteOwnedPath = "/c".try_into().unwrap();

        assert!(path1 < path2);
        assert!(path2 < path3);
        assert!(path1 < path3);
    }

    #[test]
    fn test_push_operations() {
        let mut path = AbsoluteOwnedPath::new();
        path.push("home");
        assert_eq!(path.as_str(), "/home");

        path.push("user");
        assert_eq!(path.as_str(), "/home/user");

        path.push("documents");
        assert_eq!(path.as_str(), "/home/user/documents");
    }

    #[test]
    fn test_append_str_operations() {
        let mut path = AbsoluteOwnedPath::new();
        path.push("file");
        path.append_str(".txt");
        assert_eq!(path.as_str(), "/file.txt");

        let mut path: AbsoluteOwnedPath = "/foo".try_into().unwrap();
        path.append_str("bar");
        assert_eq!(path.as_str(), "/foobar");
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
        assert_eq!(path.parent(), None);

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
        // All operations should maintain absolute path invariant
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
