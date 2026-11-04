// shortcut: `environ` points into the entry stack and cannot grow, so the
// environment is read-only; upgrade when std needs `env::set_var`.

use core::ffi::{c_char, c_int};

enosys! {
    fn setenv(*const c_char, *const c_char, c_int) -> c_int;
    fn unsetenv(*const c_char) -> c_int;
}
