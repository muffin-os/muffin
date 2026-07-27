//! End-to-end test for the VFS and ext2 file read path.
//!
//! Boots the generic `test-kernel` under QEMU with `/bin/file-read` in the
//! `/spawn` manifest and a known file baked into the disk image. The binary
//! opens `/data/hello.txt`, reads it to EOF, and compares the content. The
//! test asserts on its serial output and exit code.
//!
//! Runs are skipped on hosts without QEMU so local checkouts stay green.
//! CI installs `qemu-system-x86`.

use test_support::{KernelTest, host_env};

#[test]
fn file_read() {
    let report = KernelTest::new("file_read", host_env!())
        .program(
            "bin/file-read",
            env!("CARGO_BIN_FILE_FILE_READ_TEST_file-read-test"),
        )
        .file("data/hello.txt", b"muffin says hi\n".to_vec())
        .spawn("/bin/file-read")
        .run();

    report.assert_line_contains("fileio: content ok");
    report.assert_exit_code(0, 0);
}
