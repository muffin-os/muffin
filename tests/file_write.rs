//! End-to-end test for the ext2 write path.
//!
//! `/bin/file-write` runs one check group per open mode against the seeded
//! files on the disk, reports `filewrite: <group> ok` per group, and panics
//! with `filewrite: FAIL <check>` otherwise. The post-run disk must pass
//! `e2fsck`, and every file the guest wrote must read back on the host.

use test_support::{KernelTest, host_env};

const SEED: &[u8] = b"muffin seed data\n";

const SUFFIX: &[u8] = b"appended\n";

const NEW: &[u8] = b"created by file-write\n";

const BIG_LEN: usize = 300 * 1024;

const SPARSE_OFFSET: usize = 70 << 20;

// same formula as the guest in tests/bins/file-write
fn pattern(offset: usize, len: usize) -> Vec<u8> {
    (offset..offset + len)
        .map(|p| (p ^ (p >> 10) ^ (p >> 18)) as u8)
        .collect()
}

#[test]
fn file_write() {
    let report = KernelTest::new("file_write", host_env!()).run();

    for group in [
        "overwrite",
        "append",
        "dir",
        "create",
        "excl",
        "big",
        "trunc",
    ] {
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
    assert_eq!(report.debugfs_cat("/data/new.txt"), NEW);
    assert_eq!(report.debugfs_cat("/data/trunc.txt"), NEW);
    assert_eq!(report.debugfs_cat("/data/gone.txt"), b"");
    assert_eq!(report.debugfs_cat("/data/big.bin"), pattern(0, BIG_LEN));

    let sparse = report.debugfs_cat("/data/sparse.bin");
    assert_eq!(sparse.len(), SPARSE_OFFSET + 1024);
    assert!(
        sparse[..SPARSE_OFFSET].iter().all(|&b| b == 0),
        "hole must read as zeros"
    );
    assert_eq!(sparse[SPARSE_OFFSET..], pattern(SPARSE_OFFSET, 1024));
}
