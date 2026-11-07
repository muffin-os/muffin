use std::path::{Component, Path, PathBuf};

/// Resolves `path` against the absolute directory `cwd` without access to
/// the file system. `.` is skipped, and `..` removes the last component;
/// `..` at the root stays at the root. When `cwd` holds no `.` and no `..`,
/// the result holds none either.
#[must_use]
pub fn resolve(cwd: &Path, path: &str) -> PathBuf {
    let mut resolved = cwd.to_path_buf();
    for component in Path::new(path).components() {
        match component {
            Component::RootDir => resolved = PathBuf::from("/"),
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Normal(name) => resolved.push(name),
            Component::CurDir | Component::Prefix(_) => {}
        }
    }
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn given_relative_path_when_resolved_then_joins_cwd() {
        assert_eq!(
            resolve(Path::new("/etc"), "services"),
            Path::new("/etc/services")
        );
    }

    #[test]
    fn given_absolute_path_when_resolved_then_ignores_cwd() {
        assert_eq!(resolve(Path::new("/etc"), "/bin/./x"), Path::new("/bin/x"));
    }

    #[test]
    fn given_parent_components_when_resolved_then_stops_at_root() {
        assert_eq!(resolve(Path::new("/etc"), ".."), Path::new("/"));
        assert_eq!(
            resolve(Path::new("/etc"), "../../bin/../var"),
            Path::new("/var")
        );
    }
}
