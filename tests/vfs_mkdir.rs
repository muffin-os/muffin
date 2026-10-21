//! End-to-end test for directory creation through the vfs into ext2.
//!
//! There is no mkdir syscall yet, so the test kernel creates `/probe` and a
//! file inside it when it finds the `/vfs-mkdir` marker. The post-run disk
//! must pass `e2fsck`, and the file must read back on the host.

use test_support::{KernelTest, host_env};

#[test]
fn vfs_creates_an_ext2_directory_that_holds_a_file() {
    let report = KernelTest::new("vfs_mkdir", host_env!()).run();

    report.assert_line_contains("test-kernel: vfs-mkdir ok");
    report.assert_exit_code(0, 42);
    report.assert_fsck_clean();
    assert_eq!(report.debugfs_cat("/probe/f"), b"made by the test kernel\n");
}
