use core::ffi::{CStr, c_char};
use core::ptr;

/// NULL-terminated array of `KEY=value` strings. `_start` points it at the
/// envp array the kernel puts on the entry stack.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[allow(non_upper_case_globals, reason = "the C symbol name is lowercase")]
pub static mut environ: *const *const c_char = ptr::null();

/// Returns the value of `name` in `env`, or null when `env` has no such entry.
///
/// # Safety
///
/// `env` must be null or a NULL-terminated array of NUL-terminated strings.
unsafe fn lookup(env: *const *const c_char, name: &CStr) -> *mut c_char {
    let name = name.to_bytes();
    if env.is_null() || name.is_empty() || name.contains(&b'=') {
        return ptr::null_mut();
    }
    let mut entry = env;
    loop {
        let raw = unsafe { *entry };
        if raw.is_null() {
            return ptr::null_mut();
        }
        let bytes = unsafe { CStr::from_ptr(raw) }.to_bytes();
        if bytes
            .strip_prefix(name)
            .is_some_and(|rest| rest.first() == Some(&b'='))
        {
            return unsafe { raw.add(name.len() + 1) }.cast_mut();
        }
        entry = unsafe { entry.add(1) };
    }
}

/// # Safety
///
/// `name` must point to a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getenv(name: *const c_char) -> *mut c_char {
    // SAFETY: `_start` sets `environ` before any other code runs.
    unsafe { lookup(environ, CStr::from_ptr(name)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_env() -> [*const c_char; 4] {
        [
            c"HOME=/root".as_ptr(),
            c"PATHX=wrong".as_ptr(),
            c"PATH=/bin".as_ptr(),
            ptr::null(),
        ]
    }

    fn value(env: &[*const c_char], name: &CStr) -> Option<&'static str> {
        let found = unsafe { lookup(env.as_ptr(), name) };
        (!found.is_null()).then(|| unsafe { CStr::from_ptr(found) }.to_str().unwrap())
    }

    #[test]
    fn lookup_matches_whole_name_only() {
        let env = fake_env();
        assert_eq!(value(&env, c"PATH"), Some("/bin"), "PATHX is not PATH");
        assert_eq!(value(&env, c"HOME"), Some("/root"), "first entry matches");
    }

    #[test]
    fn lookup_misses_absent_and_invalid_names() {
        let env = fake_env();
        assert_eq!(value(&env, c"PAT"), None, "a prefix of a name is no match");
        assert_eq!(value(&env, c"HOME=/root"), None, "names cannot contain '='");
        assert_eq!(value(&env, c""), None, "the empty name never matches");
        assert_eq!(
            unsafe { lookup(ptr::null(), c"HOME") },
            ptr::null_mut(),
            "a null environment has no entries"
        );
    }
}
