//! End-to-end test for the kernel heap's stage 2 extension.
//!
//! The extension is mapped with 2 MiB pages, so an off-by-one at its end maps
//! a whole page past the heap, and the extension has to be reserved in the
//! higher-half manager or another reservation can land inside the heap.

use test_support::{KernelTest, host_env};

#[test]
fn heap_extension_ends_at_heap_size_and_is_reserved() {
    let report = KernelTest::new("heap_bounds", host_env!()).run();

    report.assert_line_contains("heap bounds past_end_mapped=false last_page_reserved=true");
    report.assert_exit_code(0, 42);
}
