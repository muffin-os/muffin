fn main() {
    std::panic::set_hook(Box::new(|info| {
        eprintln!("{info}");
        eprintln!("{}", std::backtrace::Backtrace::force_capture());
    }));

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
