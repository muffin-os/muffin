use core::ffi::c_int;
use core::time::Duration;

use kernel_abi::{ClockId, EINTR, Errno, Timespec};
use libc::{clockid_t, timespec};

use crate::errno::ret;

fn to_kernel(time: &timespec) -> Timespec {
    Timespec {
        tv_sec: time.tv_sec,
        tv_nsec: time.tv_nsec,
    }
}

fn to_c(time: Timespec) -> timespec {
    timespec {
        tv_sec: time.tv_sec,
        tv_nsec: time.tv_nsec,
    }
}

fn now(clock: ClockId) -> Result<Timespec, Errno> {
    let mut now = Timespec::default();
    muffin_sys::clock_gettime(clock, &mut now)?;
    Ok(now)
}

/// Sleeps until `clock` reads `deadline` or later. Fails with `EINVAL` for a
/// deadline outside the POSIX `timespec` range.
pub(crate) fn sleep_until(clock: ClockId, deadline: &timespec) -> Result<(), Errno> {
    let deadline = Duration::try_from(to_kernel(deadline))?;
    loop {
        let left = match deadline.checked_sub(Duration::try_from(now(clock)?)?) {
            Some(left) if !left.is_zero() => left,
            _ => return Ok(()),
        };
        // A signal ends the sleep early; the loop sleeps for the rest.
        match muffin_sys::nanosleep(&Timespec::from(left), None) {
            Ok(()) | Err(EINTR) => {}
            Err(errno) => return Err(errno),
        }
    }
}

/// # Safety
///
/// `out` must be valid for one `timespec` write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn clock_gettime(clock: clockid_t, out: *mut timespec) -> c_int {
    let now = ClockId::try_from(clock).and_then(now);
    ret(now.map(|now| unsafe { out.write(to_c(now)) }).map(|()| 0))
}

/// # Safety
///
/// `request` must be valid for one read, and `remain` must be null or valid
/// for one write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nanosleep(request: *const timespec, remain: *mut timespec) -> c_int {
    let mut left = Timespec::default();
    let result = muffin_sys::nanosleep(&to_kernel(unsafe { &*request }), Some(&mut left));
    if !remain.is_null() {
        unsafe { remain.write(to_c(left)) };
    }
    ret(result.map(|()| 0))
}
