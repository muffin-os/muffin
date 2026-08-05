//! End-to-end test for the VFS and ext2 file read path.
//!
//! Boots the generic `test-kernel` under QEMU with `/bin/file-read` in the
//! `/spawn` manifest and a known file baked into the disk image. The binary
//! opens `/data/hello.txt`, reads it to EOF, and compares the content. The
//! test asserts on its serial output and exit code.

use test_support::{KernelTest, host_env};

#[test]
fn file_read() {
    let report = KernelTest::new("file_read", host_env!()).run();

    report.assert_line_contains("fileio: content ok");
    report.assert_line_contains("fileio: seek ok");
    report.assert_exit_code(0, 0);
}
