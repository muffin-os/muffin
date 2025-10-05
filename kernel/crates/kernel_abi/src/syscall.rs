macro_rules! n {
    ($($name:ident = $val:expr),*,) => {
        $(pub const $name: usize = $val;)*

        #[allow(dead_code)]
        #[must_use] pub fn syscall_name(n: usize) -> &'static str {
            match n {
                $( $val => stringify!($name), )*
                _ => "<unknown>",
            }
        }
    };
}

n! {
    SYS_EXIT = 1,
    SYS_FCNTL = 2,
    SYS_OPEN = 3,
    SYS_STAT = 4,
    SYS_FSTAT = 5,
    SYS_PTHREAD_COND_INIT = 6,
    SYS_PTHREAD_COND_WAIT = 7,
    SYS_PTHREAD_COND_SIGNAL = 8,
    SYS_PTHREAD_COND_DESTROY = 9,
    SYS_PTHREAD_SETSPECIFIC = 10,
    SYS_PTHREAD_MUTEXATTR_INIT = 11,
    SYS_PTHREAD_MUTEXATTR_DESTROY = 12,
    SYS_PTHREAD_MUTEXATTR_SETTYPE = 13,
    SYS_PTHREAD_MUTEX_INIT = 14,
    SYS_PTHREAD_MUTEX_LOCK = 15,
    SYS_PTHREAD_MUTEX_TRYLOCK = 16,
    SYS_PTHREAD_MUTEX_UNLOCK = 17,
    SYS_PTHREAD_MUTEX_DESTROY = 18,
    SYS_PTHREAD_CONDATTR_INIT = 19,
    SYS_PTHREAD_CONDATTR_SETCLOCK = 20,
    SYS_PTHREAD_CONDATTR_DESTROY = 21,
    SYS_PTHREAD_KEY_CREATE = 22,
    SYS_PTHREAD_KEY_DELETE = 23,
    SYS_POLL = 24,
    SYS_SIGNAL = 25,
    SYS_GETENV = 26,
    SYS_MALLOC = 27,
    SYS_FREE = 28,
    SYS_REALLOC = 29,
    SYS_CALLOC = 30,
    SYS_POSIX_MEMALIGN = 31,
    SYS_ABORT = 32,
    SYS_REALPATH = 33,
    SYS_STRERROR_R = 34,
    SYS_GETCWD = 35,
    SYS_READ = 36,
    SYS_WRITE = 37,
    SYS_WRITEV = 38,
    SYS_LSEEK = 39,
    SYS_CLOSE = 40,
    SYS_MMAP = 41,
}
