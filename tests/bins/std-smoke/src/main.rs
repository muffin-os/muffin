//! Userspace exerciser for the Rust standard library on muffin.
//!
//! Each step prints `std-smoke: <step> ok`. The first failure prints
//! `std-smoke: FAIL <step>: <error>` and exits 1 at once.

use std::collections::HashMap;
use std::error::Error;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::{env, hint, panic, thread};

type Step = fn() -> Result<(), Box<dyn Error>>;

const STEPS: &[(&str, Step)] = &[
    ("args", args),
    ("env", environment),
    ("read", read),
    ("write", write),
    ("readdir", readdir),
    ("metadata", metadata),
    ("time", time),
    ("random", random),
    ("math", math),
    ("exe", exe),
    ("process", process),
    ("child_env", child_env),
    ("panic", catch_panic),
];

fn main() {
    if env::var_os("STD_SMOKE_CHILD").is_some() {
        if let Err(e) = child() {
            println!("std-smoke: FAIL child: {e}");
            std::process::exit(1);
        }
        return;
    }
    for (name, step) in STEPS {
        if let Err(e) = step() {
            println!("std-smoke: FAIL {name}: {e}");
            std::process::exit(1);
        }
        println!("std-smoke: {name} ok");
    }
    println!("std-smoke: done");
}

fn check(ok: bool, what: &str) -> Result<(), Box<dyn Error>> {
    if ok { Ok(()) } else { Err(what.into()) }
}

fn args() -> Result<(), Box<dyn Error>> {
    check(env::args().count() >= 1, "no argv[0]")
}

fn environment() -> Result<(), Box<dyn Error>> {
    check(env::vars().count() == 0, "envp not empty")?;
    check(
        env::var("PATH") == Err(env::VarError::NotPresent),
        "PATH present",
    )
}

fn read() -> Result<(), Box<dyn Error>> {
    let text = fs::read_to_string("/data/hello.txt")?;
    check(text == "muffin says hi\n", "unexpected content")
}

fn write() -> Result<(), Box<dyn Error>> {
    let path = "/var/tmp/std-smoke.txt";
    fs::write(path, "abc")?;
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(1))?;
    let mut tail = String::new();
    file.read_to_string(&mut tail)?;
    check(tail == "bc", "seek+read mismatch")?;
    check(file.metadata()?.len() == 3, "metadata len mismatch")
}

fn readdir() -> Result<(), Box<dyn Error>> {
    for entry in fs::read_dir("/data")? {
        if entry?.file_name() == "hello.txt" {
            return Ok(());
        }
    }
    Err("hello.txt not listed".into())
}

fn metadata() -> Result<(), Box<dyn Error>> {
    check(fs::metadata("/data")?.is_dir(), "/data is not a directory")?;
    check(
        fs::metadata("/data/hello.txt")?.is_file(),
        "hello.txt is not a file",
    )
}

fn time() -> Result<(), Box<dyn Error>> {
    let since_epoch = SystemTime::now().duration_since(UNIX_EPOCH)?;
    check(since_epoch.as_secs() > 1_000_000_000, "clock before 2001")?;
    let start = Instant::now();
    thread::sleep(Duration::from_millis(10));
    check(
        start.elapsed() >= Duration::from_millis(10),
        "sleep too short",
    )
}

fn random() -> Result<(), Box<dyn Error>> {
    let mut a = [0_u8; 16];
    let mut b = [0_u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut a)?;
    File::open("/dev/urandom")?.read_exact(&mut b)?;
    check(a != b, "two reads returned the same bytes")?;
    check(a != [0; 16], "read returned only zeros")?;
    // RandomState::new runs std's fill_bytes.
    let map: HashMap<u8, u8> = (0..16).map(|k| (k, a[usize::from(k)])).collect();
    check(map.len() == 16, "hashmap lost keys")
}

/// The float methods below call `sinf`, `exp`, `powf`, `atan2` and `tanhf`,
/// which `muffinos-rt` exports from the `libm` crate. `black_box`
/// stops LLVM from folding the calls at compile time.
fn math() -> Result<(), Box<dyn Error>> {
    let close = |got: f64, want: f64| (got - want).abs() < 1e-5;
    let half_pi = hint::black_box(std::f32::consts::FRAC_PI_2);
    let (sin, cos) = half_pi.sin_cos();
    check(close(sin.into(), 1.0) && close(cos.into(), 0.0), "sin_cos")?;
    check(
        close(hint::black_box(1.0_f64).exp(), std::f64::consts::E),
        "exp",
    )?;
    check(
        close(hint::black_box(2.0_f32).powf(10.0).into(), 1024.0),
        "powf",
    )?;
    check(
        close(
            hint::black_box(1.0_f64).atan2(1.0),
            std::f64::consts::FRAC_PI_4,
        ),
        "atan2",
    )?;
    check(
        close(hint::black_box(0.5_f32).tanh().into(), 0.462_117_2),
        "tanh",
    )
}

fn exe() -> Result<(), Box<dyn Error>> {
    let path = env::current_exe()?;
    check(
        path.to_string_lossy().ends_with("std-smoke"),
        &format!("current_exe is {path:?}"),
    )
}

fn process() -> Result<(), Box<dyn Error>> {
    for (program, code) in [("/bin/true", 0), ("/bin/false", 1)] {
        let status = Command::new(program).status()?;
        check(status.code() == Some(code), program)?;
    }
    Ok(())
}

/// Re-executes this binary with one variable set. The child checks that it
/// sees exactly that variable, then re-executes itself without `env` and the
/// grandchild checks that it inherited the same environment.
fn child_env() -> Result<(), Box<dyn Error>> {
    let status = Command::new(env::current_exe()?)
        .env("STD_SMOKE_CHILD", "env")
        .status()?;
    check(status.success(), "child failed")
}

fn child() -> Result<(), Box<dyn Error>> {
    let vars: Vec<(String, String)> = env::vars().collect();
    check(
        vars == [("STD_SMOKE_CHILD".to_string(), "env".to_string())],
        &format!("envp is {vars:?}"),
    )?;
    let level = env::args().nth(1).unwrap_or_else(|| "env".into());
    println!("std-smoke: child {level} ok");
    if level == "env" {
        let status = Command::new(env::current_exe()?).arg("inherit").status()?;
        check(status.success(), "grandchild failed")?;
    }
    Ok(())
}

fn catch_panic() -> Result<(), Box<dyn Error>> {
    let caught = panic::catch_unwind(|| panic!("boom")).is_err();
    check(caught, "panic not caught")
}
