//! The main thread recurses without end. The end of its stack stops the
//! recursion, and the kernel kills the process with `SIGSEGV`.

use std::hint::black_box;

/// This function uses one 4 KiB frame for each call. The call is not a tail call, so the
/// compiler cannot change the recursion into a loop.
fn recurse(depth: usize) -> usize {
    let mut frame = [0u8; 4096];
    black_box(&mut frame);
    if black_box(true) {
        recurse(depth + 1) + usize::from(frame[depth % frame.len()])
    } else {
        depth
    }
}

fn main() {
    println!("stack-overflow-main: start");
    let depth = recurse(0);
    println!("stack-overflow-main: FAIL recursion ended: {depth}");
}
