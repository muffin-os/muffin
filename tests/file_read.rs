//! End-to-end test for the VFS and ext2 file read path.
//!
//! `/bin/file-read` reports `content ok` only if `/data/hello.txt`, baked
//! into the disk image, reads to EOF with the expected bytes, and `seek ok`
//! only if lseek repositions the read.

use test_support::{KernelTest, host_env};

#[test]
fn file_read() {
    let report = KernelTest::new("file_read", host_env!()).run();

    report.assert_line_contains("fileio: content ok");
    report.assert_line_contains("fileio: seek ok");
    report.assert_exit_code(0, 0);
    report.assert_fsck_clean();
    assert_eq!(report.debugfs_cat("/data/hello.txt"), b"muffin says hi\n");
}
