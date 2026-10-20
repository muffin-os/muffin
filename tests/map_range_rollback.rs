//! End-to-end test for `AddressSpace::map_range` rolling back on failure.
//!
//! Callers free the backing frames when `map_range` fails. The test kernel
//! makes a mid-range mapping fail and reports how many pages before the
//! failure point are still mapped. Any such page would alias a frame that is
//! about to be handed out again.

use test_support::{KernelTest, host_env};

#[test]
fn failed_map_range_leaves_no_page_mapped() {
    let report = KernelTest::new("map_range_rollback", host_env!()).run();

    report.assert_line_contains("map-range rollback still_mapped=0");
    report.assert_exit_code(0, 42);
}
