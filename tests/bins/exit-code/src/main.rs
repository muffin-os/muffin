use std::process::ExitCode;

fn main() -> ExitCode {
    println!("exitcode: exiting 42");
    ExitCode::from(42)
}
