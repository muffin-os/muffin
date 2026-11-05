//! Runs `/bin/tests/gpu-nodes` on a virgl GPU.

use test_support::{KernelTest, host_env};

#[test]
fn gpu_nodes() {
    let report = KernelTest::new("gpu_nodes", host_env!())
        .virgl_gpu(800, 600)
        .run();
    report.assert_no_line_contains("FAIL");
    report.assert_line_contains("gpu-nodes: ok");
    report.assert_exit_code(0, 0);
}
