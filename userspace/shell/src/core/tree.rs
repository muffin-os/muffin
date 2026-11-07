use std::io;
use std::path::Path;

/// One entry of a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
}

#[derive(Default)]
struct Counts {
    dirs: usize,
    files: usize,
}

/// Returns the lines of `tree` for the directory `root`. The first line is
/// `root`, then one line for each entry below it, in the order that `list`
/// gives, and a last line with the number of directories and files. A
/// directory that `list` cannot read shows its error as its only child.
pub fn tree(root: &Path, list: &mut impl FnMut(&Path) -> io::Result<Vec<Entry>>) -> Vec<String> {
    let mut lines = vec![root.display().to_string()];
    let mut counts = Counts::default();
    branch(root, "", list, &mut lines, &mut counts);
    lines.push(format!(
        "{} directories, {} files",
        counts.dirs, counts.files
    ));
    lines
}

fn branch(
    dir: &Path,
    indent: &str,
    list: &mut impl FnMut(&Path) -> io::Result<Vec<Entry>>,
    lines: &mut Vec<String>,
    counts: &mut Counts,
) {
    let entries = match list(dir) {
        Ok(entries) => entries,
        Err(e) => {
            lines.push(format!("{indent}`-- [{e}]"));
            return;
        }
    };
    for (index, entry) in entries.iter().enumerate() {
        let is_last = index + 1 == entries.len();
        let (stem, child_indent) = if is_last {
            ("`-- ", "    ")
        } else {
            ("|-- ", "|   ")
        };
        lines.push(format!("{indent}{stem}{}", entry.name));
        if entry.is_dir {
            counts.dirs += 1;
            let child_indent = format!("{indent}{child_indent}");
            branch(&dir.join(&entry.name), &child_indent, list, lines, counts);
        } else {
            counts.files += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn entry(name: &str, is_dir: bool) -> Entry {
        Entry {
            name: name.to_owned(),
            is_dir,
        }
    }

    #[test]
    fn given_nested_dirs_when_tree_then_draws_branches_and_counts() {
        let dirs = BTreeMap::from([
            (
                "/",
                vec![entry("bin", true), entry("etc", true), entry("x", false)],
            ),
            ("/bin", vec![entry("sh", false)]),
            ("/etc", vec![entry("a", false), entry("b", false)]),
        ]);
        let mut list = |dir: &Path| Ok(dirs[dir.to_str().unwrap()].clone());

        let lines = tree(Path::new("/"), &mut list);

        assert_eq!(
            lines,
            [
                "/",
                "|-- bin",
                "|   `-- sh",
                "|-- etc",
                "|   |-- a",
                "|   `-- b",
                "`-- x",
                "2 directories, 4 files",
            ]
        );
    }

    #[test]
    fn given_unreadable_dir_when_tree_then_shows_error_as_child() {
        let mut list = |dir: &Path| {
            if dir == Path::new("/") {
                Ok(vec![entry("locked", true)])
            } else {
                Err(io::Error::other("denied"))
            }
        };

        let lines = tree(Path::new("/"), &mut list);

        assert_eq!(
            lines,
            [
                "/",
                "`-- locked",
                "    `-- [denied]",
                "1 directories, 0 files"
            ]
        );
    }
}
