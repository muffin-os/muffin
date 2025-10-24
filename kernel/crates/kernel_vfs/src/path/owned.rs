use alloc::string::String;
use core::borrow::Borrow;
use core::fmt::Display;
use core::ops::Deref;

use thiserror::Error;

use crate::path::{FILEPATH_SEPARATOR, Path};

#[derive(Debug, Copy, Clone, Eq, PartialEq, Error)]
#[error("path is not absolute")]
pub struct PathNotAbsoluteError;

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct OwnedPath {
    inner: String,
}

impl Display for OwnedPath {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", &self.inner)
    }
}

impl Deref for OwnedPath {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        Path::new(&self.inner)
    }
}

impl AsRef<Path> for OwnedPath {
    fn as_ref(&self) -> &Path {
        self
    }
}

impl Borrow<Path> for OwnedPath {
    fn borrow(&self) -> &Path {
        self
    }
}

impl OwnedPath {
    pub fn new<S: Into<String>>(s: S) -> Self {
        Self { inner: s.into() }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.inner
    }

    /// Appends a string to the end of the path.
    ///
    /// ```rust
    /// # use kernel_vfs::path::OwnedPath;
    /// let mut path = OwnedPath::new("/foo");
    /// path.append_str(".txt");
    /// assert_eq!(path.as_str(), "/foo.txt");
    /// ```
    ///
    /// This is different from [`push_str`], which appends a string to
    /// the end of the path as a new component.
    pub fn append_str(&mut self, other: &str) {
        self.inner.push_str(other);
    }

    /// Appends a string to the end of the path as a new component.
    ///
    /// ```rust
    /// # use kernel_vfs::path::OwnedPath;
    /// let mut path = OwnedPath::new("/foo");
    /// path.push("bar");
    /// assert_eq!(path.as_str(), "/foo/bar");
    /// ```
    ///
    /// If the path is empty, pushing a new component will make
    /// the path absolute.
    /// ```rust
    /// # use kernel_vfs::path::OwnedPath;
    /// let mut path = OwnedPath::new("");
    /// path.push("foo");
    /// assert_eq!(path.as_str(), "/foo");
    /// ```
    pub fn push<P>(&mut self, other: P)
    where
        P: AsRef<Path>,
    {
        let other = other.as_ref();
        if self.inner.ends_with(FILEPATH_SEPARATOR) {
            self.inner.push_str(other);
        } else {
            self.inner.push(FILEPATH_SEPARATOR);
            self.inner.push_str(other);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_append_str() {
        let mut path = OwnedPath::new("");
        path.append_str("foo");
        assert_eq!(path.as_str(), "foo");
        path.append_str("bar");
        assert_eq!(path.as_str(), "foobar");
        path.append_str("/");
        assert_eq!(path.as_str(), "foobar/");
        path.append_str("baz");
        assert_eq!(path.as_str(), "foobar/baz");
        path.append_str(".txt");
        assert_eq!(path.as_str(), "foobar/baz.txt");
    }

    #[test]
    fn test_push() {
        let mut path = OwnedPath::new("");
        path.push("foo");
        assert_eq!(path.as_str(), "/foo");
        path.push("bar");
        assert_eq!(path.as_str(), "/foo/bar");
        path.push(".txt");
        assert_eq!(path.as_str(), "/foo/bar/.txt");
    }

    #[test]
    fn test_append_str_edge_cases() {
        // Empty append
        let mut path = OwnedPath::new("foo");
        path.append_str("");
        assert_eq!(path.as_str(), "foo");

        // Multiple slashes
        let mut path = OwnedPath::new("foo");
        path.append_str("///bar");
        assert_eq!(path.as_str(), "foo///bar");

        // Special characters
        let mut path = OwnedPath::new("foo");
        path.append_str("-bar_baz.txt");
        assert_eq!(path.as_str(), "foo-bar_baz.txt");

        // Spaces
        let mut path = OwnedPath::new("foo");
        path.append_str(" bar");
        assert_eq!(path.as_str(), "foo bar");
    }

    #[test]
    fn test_push_edge_cases() {
        // Push to absolute path
        let mut path = OwnedPath::new("/");
        path.push("foo");
        assert_eq!(path.as_str(), "/foo");

        // Push to path ending with slash
        let mut path = OwnedPath::new("/foo/");
        path.push("bar");
        assert_eq!(path.as_str(), "/foo/bar");

        // Push empty string
        let mut path = OwnedPath::new("/foo");
        path.push("");
        assert_eq!(path.as_str(), "/foo/");

        // Push path with leading slash
        let mut path = OwnedPath::new("/foo");
        path.push("/bar");
        assert_eq!(path.as_str(), "/foo//bar");

        // Push path with trailing slash
        let mut path = OwnedPath::new("/foo");
        path.push("bar/");
        assert_eq!(path.as_str(), "/foo/bar/");

        // Push to relative path (makes it absolute)
        let mut path = OwnedPath::new("foo");
        path.push("bar");
        assert_eq!(path.as_str(), "foo/bar");

        // Push multiple components
        let mut path = OwnedPath::new("");
        path.push("foo");
        path.push("bar");
        path.push("baz");
        assert_eq!(path.as_str(), "/foo/bar/baz");

        // Push with dots
        let mut path = OwnedPath::new("/foo");
        path.push(".");
        assert_eq!(path.as_str(), "/foo/.");

        let mut path = OwnedPath::new("/foo");
        path.push("..");
        assert_eq!(path.as_str(), "/foo/..");

        // Push hidden file
        let mut path = OwnedPath::new("/foo");
        path.push(".hidden");
        assert_eq!(path.as_str(), "/foo/.hidden");
    }

    #[test]
    fn test_new() {
        // From &str
        let path = OwnedPath::new("/foo/bar");
        assert_eq!(path.as_str(), "/foo/bar");

        // From String
        let path = OwnedPath::new(alloc::string::String::from("/foo/bar"));
        assert_eq!(path.as_str(), "/foo/bar");

        // Empty
        let path = OwnedPath::new("");
        assert_eq!(path.as_str(), "");

        // With special characters
        let path = OwnedPath::new("/foo-bar_baz.txt");
        assert_eq!(path.as_str(), "/foo-bar_baz.txt");
    }

    #[test]
    fn test_deref() {
        let owned = OwnedPath::new("/foo/bar");
        let path: &Path = &*owned;
        assert_eq!(&**path, "/foo/bar");

        // Should have access to Path methods
        assert!(owned.is_absolute());
        assert_eq!(owned.file_name(), Some("bar"));
        assert_eq!(owned.parent(), Some(Path::new("/foo")));
    }

    #[test]
    fn test_as_ref_path() {
        let owned = OwnedPath::new("/foo/bar");
        let path: &Path = owned.as_ref();
        assert_eq!(&**path, "/foo/bar");
    }

    #[test]
    fn test_borrow() {
        use core::borrow::Borrow;

        let owned = OwnedPath::new("/foo/bar");
        let borrowed: &Path = owned.borrow();
        assert_eq!(&**borrowed, "/foo/bar");
    }

    #[test]
    fn test_display() {
        use alloc::format;

        let path = OwnedPath::new("/foo/bar");
        assert_eq!(format!("{}", path), "/foo/bar");

        let path = OwnedPath::new("");
        assert_eq!(format!("{}", path), "");

        let path = OwnedPath::new("relative");
        assert_eq!(format!("{}", path), "relative");
    }

    #[test]
    fn test_clone() {
        let path = OwnedPath::new("/foo/bar");
        let cloned = path.clone();
        assert_eq!(path, cloned);
        assert_eq!(cloned.as_str(), "/foo/bar");
    }

    #[test]
    fn test_eq() {
        let path1 = OwnedPath::new("/foo/bar");
        let path2 = OwnedPath::new("/foo/bar");
        let path3 = OwnedPath::new("/foo/baz");

        assert_eq!(path1, path2);
        assert_ne!(path1, path3);
    }

    #[test]
    fn test_ord() {
        let path1 = OwnedPath::new("/a");
        let path2 = OwnedPath::new("/b");
        let path3 = OwnedPath::new("/c");

        assert!(path1 < path2);
        assert!(path2 < path3);
        assert!(path1 < path3);
    }

    #[test]
    fn test_mixed_operations() {
        // Mix append_str and push
        let mut path = OwnedPath::new("/foo");
        path.push("bar");
        path.append_str(".txt");
        assert_eq!(path.as_str(), "/foo/bar.txt");

        // Note: when path ends with separator and we push, it doesn't add another separator
        let mut path = OwnedPath::new("");
        path.push("foo");
        path.append_str("/");
        path.push("bar");
        // After push("foo"), path is "/foo", append_str("/") makes "/foo/", push("bar") makes "/foo/bar"
        assert_eq!(path.as_str(), "/foo/bar");

        // Build complex path
        let mut path = OwnedPath::new("/home");
        path.push("user");
        path.push("documents");
        path.push("file");
        path.append_str(".txt");
        assert_eq!(path.as_str(), "/home/user/documents/file.txt");
    }

    #[test]
    fn test_push_path_reference() {
        let mut owned = OwnedPath::new("/foo");
        let to_push = Path::new("bar");
        owned.push(to_push);
        assert_eq!(owned.as_str(), "/foo/bar");

        let mut owned = OwnedPath::new("/foo");
        owned.push("bar/baz");
        assert_eq!(owned.as_str(), "/foo/bar/baz");
    }
}
