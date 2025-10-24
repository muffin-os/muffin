use alloc::borrow::{Cow, ToOwned};
use core::fmt::{Display, Formatter};
use core::ops::Deref;
use core::ptr;

pub use absolute::*;
pub use absolute_owned::*;
pub use filenames::*;
pub use owned::*;

mod absolute;
mod absolute_owned;
mod filenames;
mod owned;

pub const FILEPATH_SEPARATOR: char = '/';
pub const ROOT: &AbsolutePath = unsafe { &*(ptr::from_ref::<str>("/") as *const AbsolutePath) };

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(transparent)]
pub struct Path {
    inner: str,
}

impl Display for Path {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", &self.inner)
    }
}

impl AsRef<Path> for &Path {
    fn as_ref(&self) -> &Path {
        self
    }
}

impl AsRef<Path> for &str {
    fn as_ref(&self) -> &Path {
        Path::new(self)
    }
}

impl AsRef<str> for &Path {
    fn as_ref(&self) -> &str {
        &self.inner
    }
}

impl Deref for Path {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl Path {
    pub fn new<S: AsRef<str> + ?Sized>(s: &S) -> &Path {
        unsafe { &*(ptr::from_ref::<str>(s.as_ref()) as *const Path) }
    }

    #[must_use]
    pub fn filenames(&self) -> Filenames<'_> {
        Filenames::new(self)
    }

    #[must_use]
    pub fn is_absolute(&self) -> bool {
        self.starts_with(FILEPATH_SEPARATOR)
    }

    #[must_use]
    pub fn is_relative(&self) -> bool {
        !self.is_absolute()
    }

    #[must_use]
    pub fn file_name(&self) -> Option<&str> {
        self.filenames().next_back()
    }

    #[must_use]
    pub fn parent(&self) -> Option<&Path> {
        // Handle empty path
        if self.inner.is_empty() {
            return None;
        }

        let mut chars = self.char_indices();

        // Find the last non-separator character
        let last_non_sep = chars.rfind(|&(_, c)| c != FILEPATH_SEPARATOR)?;

        let last_non_sep_pos = last_non_sep.0;

        // Find the last separator before the last component
        let sep_pos = self.inner[..last_non_sep_pos + 1].rfind(FILEPATH_SEPARATOR);

        if let Some(sep) = sep_pos {
            // Find the last non-separator before this separator
            let parent_end = self.inner[..sep]
                .rfind(|c| c != FILEPATH_SEPARATOR)
                .map(|pos| pos + 1);

            if let Some(end) = parent_end {
                Some(Path::new(&self.inner[..end]))
            } else {
                // Parent is all separators before the component (e.g., "/" for "/foo")
                None
            }
        } else {
            // No separator found - single component relative path
            // Dot paths like "." and ".." are their own parent
            // But regular paths like "foo" have no parent
            if self.inner == *"." || self.inner == *".." {
                Some(self)
            } else {
                None
            }
        }
    }

    #[must_use]
    pub fn make_absolute(&self) -> Cow<'_, AbsolutePath> {
        if let Ok(path) = AbsolutePath::try_new(self) {
            Cow::Borrowed(path)
        } else {
            let mut p = AbsoluteOwnedPath::new();
            // Don't push empty paths - AbsoluteOwnedPath::new() already gives us "/"
            if !self.is_empty() {
                p.push(self);
            }
            Cow::Owned(p)
        }
    }
}

impl ToOwned for Path {
    type Owned = OwnedPath;

    fn to_owned(&self) -> Self::Owned {
        Self::Owned::new(self.inner.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use alloc::borrow::Cow;

    use crate::path::Path;

    #[test]
    fn test_make_absolute() {
        for (path, expected) in [
            ("", Cow::Owned("/".try_into().unwrap())),
            ("/", Cow::Borrowed("/".try_into().unwrap())),
            ("//", Cow::Borrowed("//".try_into().unwrap())),
            ("foo", Cow::Owned("/foo".try_into().unwrap())),
            ("/foo", Cow::Borrowed("/foo".try_into().unwrap())),
            ("foo/bar", Cow::Owned("/foo/bar".try_into().unwrap())),
            ("/foo/bar", Cow::Borrowed("/foo/bar".try_into().unwrap())),
            ("//foo/bar", Cow::Borrowed("//foo/bar".try_into().unwrap())),
            (
                "///foo/bar",
                Cow::Borrowed("///foo/bar".try_into().unwrap()),
            ),
        ] {
            assert_eq!(Path::new(path).make_absolute(), expected);
        }
    }

    #[test]
    fn test_parent() {
        for (path, parent) in [
            ("/", None),
            ("//", None),
            ("///", None),
            ("", None),
            ("/foo/bar/baz", Some("/foo/bar")),
            ("/foo/bar", Some("/foo")),
            ("/foo//bar", Some("/foo")),
            ("///foo/bar", Some("///foo")),
            ("foo", None),
            ("/foo", None),
            ("//foo", None),
            ("foo/", None),
            ("/foo/", None),
            ("/foo/bar/baz/", Some("/foo/bar")),
            ("/foo/bar/baz//", Some("/foo/bar")),
            ("/foo/bar/baz///", Some("/foo/bar")),
            ("/foo/bar//baz///", Some("/foo/bar")),
            ("/foo/bar///baz///", Some("/foo/bar")),
            ("///foo///bar///baz///", Some("///foo///bar")),
        ] {
            assert_eq!(Path::new(path).parent(), parent.map(Path::new));
        }
    }

    #[test]
    fn test_file_name() {
        assert_eq!(Path::new("").file_name(), None);
        assert_eq!(Path::new("/").file_name(), None);
        assert_eq!(Path::new("//").file_name(), None);
        assert_eq!(Path::new("foo").file_name(), Some("foo"));
        assert_eq!(Path::new("/foo").file_name(), Some("foo"));
        assert_eq!(Path::new("//foo").file_name(), Some("foo"));
        assert_eq!(Path::new("foo/").file_name(), Some("foo"));
        assert_eq!(Path::new("/foo/").file_name(), Some("foo"));
        assert_eq!(Path::new("/foo//bar/").file_name(), Some("bar"));
    }

    #[test]
    fn test_is_absolute() {
        assert!(!Path::new("").is_absolute());

        assert!(Path::new("/").is_absolute());
        assert!(Path::new("//").is_absolute());
        assert!(Path::new("///").is_absolute());

        assert!(!Path::new(" ").is_absolute());
        assert!(!Path::new(" /").is_absolute());

        assert!(!Path::new("foo").is_absolute());
        assert!(Path::new("/foo/bar").is_absolute());
        assert!(!Path::new("foo/bar").is_absolute());
    }

    #[test]
    fn test_is_relative() {
        // Basic relative paths
        assert!(Path::new("").is_relative());
        assert!(Path::new("foo").is_relative());
        assert!(Path::new("foo/bar").is_relative());
        assert!(Path::new("foo/bar/baz").is_relative());
        assert!(Path::new("./foo").is_relative());
        assert!(Path::new("../foo").is_relative());

        // Paths with spaces
        assert!(Path::new(" ").is_relative());
        assert!(Path::new(" /").is_relative());
        assert!(Path::new("foo ").is_relative());
        assert!(Path::new(" foo").is_relative());

        // Basic absolute paths (not relative)
        assert!(!Path::new("/").is_relative());
        assert!(!Path::new("//").is_relative());
        assert!(!Path::new("///").is_relative());
        assert!(!Path::new("/foo").is_relative());
        assert!(!Path::new("/foo/bar").is_relative());
        assert!(!Path::new("//foo/bar").is_relative());
    }

    #[test]
    fn test_file_name_edge_cases() {
        // Multiple trailing slashes
        assert_eq!(Path::new("/foo/bar//").file_name(), Some("bar"));
        assert_eq!(Path::new("/foo/bar///").file_name(), Some("bar"));
        assert_eq!(Path::new("/foo/bar////").file_name(), Some("bar"));

        // Paths with spaces
        assert_eq!(Path::new("foo bar").file_name(), Some("foo bar"));
        assert_eq!(Path::new("/foo/bar baz").file_name(), Some("bar baz"));
        assert_eq!(Path::new("/ foo").file_name(), Some(" foo"));
        assert_eq!(Path::new("/foo/ bar").file_name(), Some(" bar"));

        // Single component paths
        assert_eq!(Path::new("file.txt").file_name(), Some("file.txt"));
        assert_eq!(Path::new("dir").file_name(), Some("dir"));

        // Paths with dots
        assert_eq!(Path::new(".").file_name(), Some("."));
        assert_eq!(Path::new("..").file_name(), Some(".."));
        assert_eq!(Path::new("/.").file_name(), Some("."));
        assert_eq!(Path::new("/..").file_name(), Some(".."));
        assert_eq!(Path::new("/foo/.").file_name(), Some("."));
        assert_eq!(Path::new("/foo/..").file_name(), Some(".."));

        // Hidden files
        assert_eq!(Path::new(".hidden").file_name(), Some(".hidden"));
        assert_eq!(Path::new("/foo/.hidden").file_name(), Some(".hidden"));
    }

    #[test]
    fn test_parent_edge_cases() {
        // Paths with dots
        assert_eq!(Path::new(".").parent(), Some(Path::new(".")));
        assert_eq!(Path::new("..").parent(), Some(Path::new("..")));
        assert_eq!(Path::new("./foo").parent(), Some(Path::new(".")));
        assert_eq!(Path::new("../foo").parent(), Some(Path::new("..")));
        assert_eq!(Path::new("/.").parent(), None);
        assert_eq!(Path::new("/..").parent(), None);

        // Deeply nested paths
        assert_eq!(
            Path::new("/a/b/c/d/e/f").parent(),
            Some(Path::new("/a/b/c/d/e"))
        );
        assert_eq!(
            Path::new("/a/b/c/d/e").parent(),
            Some(Path::new("/a/b/c/d"))
        );

        // Relative paths with components
        assert_eq!(Path::new("foo/bar").parent(), Some(Path::new("foo")));
        assert_eq!(
            Path::new("foo/bar/baz").parent(),
            Some(Path::new("foo/bar"))
        );
    }

    #[test]
    fn test_make_absolute_edge_cases() {
        // Paths with dots
        assert_eq!(
            Path::new(".").make_absolute(),
            Cow::Owned("/.".try_into().unwrap())
        );
        assert_eq!(
            Path::new("..").make_absolute(),
            Cow::Owned("/..".try_into().unwrap())
        );
        assert_eq!(
            Path::new("./foo").make_absolute(),
            Cow::Owned("/./foo".try_into().unwrap())
        );
        assert_eq!(
            Path::new("../foo").make_absolute(),
            Cow::Owned("/../foo".try_into().unwrap())
        );

        // Paths with spaces
        assert_eq!(
            Path::new(" ").make_absolute(),
            Cow::Owned("/ ".try_into().unwrap())
        );
        assert_eq!(
            Path::new("foo bar").make_absolute(),
            Cow::Owned("/foo bar".try_into().unwrap())
        );

        // Already absolute paths remain borrowed
        assert_eq!(
            Path::new("/").make_absolute(),
            Cow::Borrowed("/".try_into().unwrap())
        );
        assert_eq!(
            Path::new("/foo/bar/baz").make_absolute(),
            Cow::Borrowed("/foo/bar/baz".try_into().unwrap())
        );

        // Multiple leading slashes
        assert_eq!(
            Path::new("////foo").make_absolute(),
            Cow::Borrowed("////foo".try_into().unwrap())
        );
    }

    #[test]
    fn test_path_new() {
        // Basic paths
        let path = Path::new("foo");
        assert_eq!(&**path, "foo");

        let path = Path::new("/foo/bar");
        assert_eq!(&**path, "/foo/bar");

        // Empty path
        let path = Path::new("");
        assert_eq!(&**path, "");

        // Path with special characters
        let path = Path::new("/foo-bar_baz.txt");
        assert_eq!(&**path, "/foo-bar_baz.txt");
    }

    #[test]
    fn test_path_deref() {
        let path = Path::new("/foo/bar");
        // Deref to str
        assert_eq!(&**path, "/foo/bar");
        // String methods should work
        assert!(path.starts_with('/'));
        assert!(path.ends_with("bar"));
        assert_eq!(path.len(), 8);
    }

    #[test]
    fn test_path_display() {
        use alloc::format;

        let path = Path::new("/foo/bar");
        assert_eq!(format!("{}", path), "/foo/bar");

        let path = Path::new("");
        assert_eq!(format!("{}", path), "");

        let path = Path::new("relative/path");
        assert_eq!(format!("{}", path), "relative/path");
    }

    #[test]
    fn test_path_as_ref() {
        let path = Path::new("/foo/bar");
        let path_ref: &Path = path.as_ref();
        assert_eq!(path_ref, path);

        // Test converting from &str to &Path using as_ref
        let str_ref: &str = "/foo/bar";
        let path_from_str: &Path = (&str_ref).as_ref();
        assert_eq!(path_from_str, Path::new("/foo/bar"));
    }

    #[test]
    fn test_path_to_owned() {
        use alloc::borrow::ToOwned;

        let path = Path::new("/foo/bar");
        let owned = path.to_owned();
        assert_eq!(owned.as_str(), "/foo/bar");

        let path = Path::new("");
        let owned = path.to_owned();
        assert_eq!(owned.as_str(), "");
    }
}
