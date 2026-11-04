use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("realtime clock is after 1970")
        .as_secs();
    let (hours, minutes, seconds) = (secs / 3600 % 24, secs / 60 % 60, secs % 60);
    println!("Hello, world! It is {hours:02}:{minutes:02}:{seconds:02} UTC.");

    foo()
}

fn foo() {
    bar()
}

fn bar() {
    sna()
}

fn sna() {
    fu()
}

fn fu() {
    panic!("hello there");
}
