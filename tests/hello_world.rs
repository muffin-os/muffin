use test_support::{KernelTest, host_env};

#[test]
fn hello_world() {
    let report = KernelTest::new("hello_world", host_env!()).run();

    report.assert_line_contains("Hello, world!");
    report.assert_line_contains("hello there");
    report.assert_line_contains("hello_world::fu");
    report.assert_exit_code(0, 101);
}
