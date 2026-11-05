//! Userspace exerciser for AF_UNIX stream sockets through `muffin_sys`.
//!
//! The parent listens and forks. The child connects and exchanges `ping` and
//! `pong`. After the parent closes its end, the child sees EOF and then `EPIPE`.
//! Each step prints `unix-socket: <step>`. A failure prints
//! `unix-socket: FAIL <what>` and exits with code 1.

#![no_std]
#![no_main]

use core::ffi::c_int;
use core::time::Duration;

use minilib::{
    AF_UNIX, EPIPE, SOCK_STREAM, SaFlags, SigAction, SigHandler, SigSet, Signal, WaitOptions,
    accept, bind, close, connect, fork, listen, nanosleep, println, read, sigaction, socket,
    waitpid, write,
};

const PATH: &str = "/unix-socket-test";

/// Gives the other side time to park in its blocking call first.
const SETTLE: Duration = Duration::from_millis(50);

minilib::entry!(main);

macro_rules! require {
    ($what:expr, $result:expr) => {
        match $result {
            Ok(value) => value,
            Err(e) => {
                println!("unix-socket: FAIL {} {e}", $what);
                return 1;
            }
        }
    };
}

fn main() -> i32 {
    let l = require!("socket", socket(AF_UNIX as c_int, SOCK_STREAM, 0));
    require!("bind", bind(l, PATH));
    require!("listen", listen(l, 4));
    println!("unix-socket: listening");

    match require!("fork", fork()) {
        0 => child(),
        pid => parent(l, pid),
    }
}

fn child() -> i32 {
    let ignore = SigAction {
        handler: SigHandler::IGNORE,
        mask: SigSet::EMPTY,
        flags: SaFlags::default(),
        restorer: 0,
    };
    require!("sigaction", sigaction(Signal::Pipe, Some(&ignore), None));
    let _ = nanosleep(&SETTLE.into(), None);

    let c = require!("child socket", socket(AF_UNIX as c_int, SOCK_STREAM, 0));
    require!("connect", connect(c, PATH));
    println!("unix-socket: child connected");

    require!("child write", write(c, b"ping"));
    let mut buf = [0_u8; 4];
    let n = require!("child read", read(c, &mut buf));
    if n != 4 || &buf != b"pong" {
        println!("unix-socket: FAIL child expected pong");
        return 1;
    }
    println!("unix-socket: child got pong");

    let n = require!("child read eof", read(c, &mut buf));
    if n != 0 {
        println!("unix-socket: FAIL child expected eof, got {n}");
        return 1;
    }
    println!("unix-socket: child eof");

    match write(c, b"x") {
        Err(EPIPE) => println!("unix-socket: child epipe"),
        other => {
            println!("unix-socket: FAIL child expected EPIPE, got {other:?}");
            return 1;
        }
    }
    0
}

fn parent(l: c_int, child: i64) -> i32 {
    let c = require!("accept", accept(l));
    println!("unix-socket: accepted");

    let mut buf = [0_u8; 4];
    let n = require!("parent read", read(c, &mut buf));
    if n != 4 || &buf != b"ping" {
        println!("unix-socket: FAIL parent expected ping");
        return 1;
    }
    println!("unix-socket: parent got ping");

    let _ = nanosleep(&SETTLE.into(), None);
    require!("parent write", write(c, b"pong"));
    let _ = nanosleep(&SETTLE.into(), None);
    require!("parent close", close(c));

    let mut status = 0;
    require!("waitpid", waitpid(child, &mut status, WaitOptions::empty()));
    if status != 0 {
        println!("unix-socket: FAIL child status {status}");
        return 1;
    }
    println!("unix-socket: parent waited status=0");
    let _ = close(l);
    0
}
