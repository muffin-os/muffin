//! End-to-end test for the ext2 write path.
//!
//! `/bin/file-write` runs one check group per open mode against the seeded
//! files on the disk, reports `filewrite: <group> ok` per group, and panics
//! with `filewrite: FAIL <check>` otherwise. The post-run disk must pass
//! `e2fsck`, and every file the guest wrote must read back on the host.

use test_support::{KernelTest, host_env};

const SEED: &[u8] = b"muffin seed data\n";

const SUFFIX: &[u8] = b"appended\n";

#[test]
fn file_write() {
    let report = KernelTest::new("file_write", host_env!()).run();

    for group in ["overwrite", "append", "dir"] {
        report.assert_line_contains(&format!("filewrite: {group} ok"));
    }
    report.assert_no_line_contains("filewrite: FAIL");
    report.assert_exit_code(0, 0);
    report.assert_fsck_clean();

    let mut overwritten = SEED.to_vec();
    overwritten[3..5].copy_from_slice(b"XY");
    assert_eq!(report.debugfs_cat("/data/over.txt"), overwritten);
    assert_eq!(
        report.debugfs_cat("/data/seed.txt"),
        [SEED, SUFFIX].concat()
    );
}
