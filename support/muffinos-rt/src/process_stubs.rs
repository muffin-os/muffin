// shortcut: kernel has no users, groups, sessions or chroot; upgrade when
// `kernel/abi/src/syscall.rs` adds a syscall for them.

use core::ffi::{c_char, c_int};

use libc::{gid_t, pid_t, uid_t};

enosys! {
    fn setuid(uid_t) -> c_int;
    fn setgid(gid_t) -> c_int;
    fn setpgid(pid_t, pid_t) -> c_int;
    fn setsid() -> pid_t;
    fn chroot(*const c_char) -> c_int;
}
