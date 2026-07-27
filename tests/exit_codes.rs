//! End-to-end test for multi-process fan-out and exit code reporting.
//!
//! Boots the generic `test-kernel` under QEMU with two different binaries in
//! the `/spawn` manifest, proving several processes can be booted in a single
//! QEMU instance. The test asserts each process reports its own exit code, 42
//! from `/bin/exit-code` and 0 from `/bin/file-read`.
//!
//! Runs are skipped on hosts without QEMU so local checkouts stay green.
//! CI installs `qemu-system-x86`.

use test_support::{KernelTest, host_env};

#[test]
fn process_exit_codes() {
    let report = KernelTest::new("exit_codes", host_env!())
        .program(
            "bin/exit-code",
            env!("CARGO_BIN_FILE_EXIT_CODE_TEST_exit-code-test"),
        )
        .program(
            "bin/file-read",
            env!("CARGO_BIN_FILE_FILE_READ_TEST_file-read-test"),
        )
        .file("data/hello.txt", b"muffin says hi\n".to_vec())
        .spawn("/bin/exit-code")
        .spawn("/bin/file-read")
        .run();

    report.assert_exit_code(0, 42);
    report.assert_exit_code(1, 0);
}
