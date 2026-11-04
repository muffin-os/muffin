use core::arch::x86_64::_rdrand64_step;
use core::fmt::Write;

use conquer_once::spin::OnceCell;
use kernel_abi::S_IFCHR;
use kernel_devfs::{ArcLockedDevFs, DevFile, Null, Serial};
use kernel_vfs::path::AbsolutePath;
use kernel_vfs::{ReadError, Stat, StatError, WriteError};
use raw_cpuid::CpuId;
use tracing::{Level, instrument};

use crate::serial_print;

static DEVFS: OnceCell<ArcLockedDevFs> = OnceCell::uninit();

#[must_use]
pub fn devfs() -> &'static ArcLockedDevFs {
    DEVFS.get().expect("devfs should be initialized")
}

#[instrument(name = "init devfs", level = Level::TRACE)]
pub fn init() {
    let devfs = ArcLockedDevFs::new();
    {
        let mut guard = devfs.write();
        guard
            .register_file(AbsolutePath::try_new("/serial").unwrap(), || {
                Ok(Serial::<SerialWrite>::default())
            })
            .expect("should be able to register serial file");

        // shortcut: stdin reads EOF and stdout/stderr go to the serial port, until a terminal device exists
        guard
            .register_file(AbsolutePath::try_new("/stdin").unwrap(), || Ok(Null))
            .expect("should be able to register stdin");
        guard
            .register_file(AbsolutePath::try_new("/stdout").unwrap(), || {
                Ok(Serial::<SerialWrite>::default())
            })
            .expect("should be able to register stdout");
        guard
            .register_file(AbsolutePath::try_new("/stderr").unwrap(), || {
                Ok(Serial::<SerialWrite>::default())
            })
            .expect("should be able to register stderr");
        if CpuId::new()
            .get_feature_info()
            .is_some_and(|info| info.has_rdrand())
        {
            guard
                .register_file(AbsolutePath::try_new("/urandom").unwrap(), || Ok(Urandom))
                .expect("should be able to register urandom");
        }
    }
    DEVFS.init_once(|| devfs);
}

#[derive(Default)]
struct SerialWrite;

impl Write for SerialWrite {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        serial_print!("{s}");
        Ok(())
    }
}

/// Random bytes from the CPU `rdrand` instruction.
struct Urandom;

impl DevFile for Urandom {
    fn read(&mut self, buf: &mut [u8], _: usize) -> Result<usize, ReadError> {
        for chunk in buf.chunks_mut(8) {
            let word = rdrand().ok_or(ReadError::ReadFailed)?;
            chunk.copy_from_slice(&word.to_ne_bytes()[..chunk.len()]);
        }
        Ok(buf.len())
    }

    fn write(&mut self, _: &[u8], _: usize) -> Result<usize, WriteError> {
        Err(WriteError::NotWritable)
    }

    fn stat(&mut self, stat: &mut Stat) -> Result<(), StatError> {
        stat.size = 0;
        stat.mode = S_IFCHR | 0o444;
        Ok(())
    }
}

/// Intel recommends ten retries before `rdrand` counts as unavailable.
fn rdrand() -> Option<u64> {
    let mut word = 0;
    (0..10).find_map(|_| {
        // SAFETY: `init` registers `Urandom` only when cpuid reports `rdrand`.
        let ok = unsafe { _rdrand64_step(&mut word) } == 1;
        ok.then_some(word)
    })
}
