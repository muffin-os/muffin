#![no_std]
#![no_main]

use minilib::{SigAction, Signal, args, env, println, read, sigaction};

const EXPECTED_ARGS: [&[u8]; 3] = [b"exec-target", b"alpha", b"beta"];
const HELLO: &[u8] = b"muffin says hi\n";

minilib::entry!(main);

fn main() -> i32 {
    let mut it = args();
    for expected in EXPECTED_ARGS {
        if it.next() != Some(expected) {
            return 10;
        }
    }
    if it.next().is_some() {
        return 10;
    }

    let mut it = env();
    if it.next() != Some(b"KEY=value".as_slice()) || it.next().is_some() {
        return 11;
    }

    // fd 3 was opened by the first image and never read, so it still points
    // at offset 0 of the hello file.
    let mut buf = [0u8; 64];
    let mut filled = 0usize;
    loop {
        if filled >= buf.len() {
            return 12;
        }
        let Ok(n) = read(3, &mut buf[filled..]) else {
            return 12;
        };
        if n == 0 {
            break;
        }
        filled += n;
    }
    if &buf[..filled] != HELLO {
        return 12;
    }

    let mut old = SigAction::default();
    if sigaction(Signal::Usr1, None, Some(&mut old)).is_err() || !old.handler.is_default() {
        return 13;
    }
    let mut old = SigAction::default();
    if sigaction(Signal::Usr2, None, Some(&mut old)).is_err() || !old.handler.is_ignore() {
        return 14;
    }

    println!("exec-target: after exec");
    42
}
