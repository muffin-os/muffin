macro_rules! n {
    ($($name:ident = $val:expr),*,) => {
        $(pub const $name: usize = $val;)*

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
    SYS_KILL = 42,
    SYS_SIGACTION = 43,
    SYS_SIGPROCMASK = 44,
    SYS_SIGPENDING = 45,
    SYS_SIGRETURN = 46,
    SYS_GETPID = 47,
    SYS_IOCTL = 48,
    SYS_FSYNC = 49,
    SYS_CLOCK_GETTIME = 50,
    SYS_EXE_PATH = 51,
    SYS_NANOSLEEP = 52,
    SYS_EXECVE = 53,
    SYS_POWEROFF = 54,
    SYS_FORK = 55,
    SYS_GETPPID = 56,
    SYS_WAITPID = 57,
    SYS_GETDENTS = 58,
    SYS_SOCKET = 59,
    SYS_BIND = 60,
    SYS_LISTEN = 61,
    SYS_ACCEPT = 62,
    SYS_CONNECT = 63,
    SYS_SHUTDOWN = 64,
    SYS_FTRUNCATE = 65,
    SYS_UNLINK = 66,
    SYS_MUNMAP = 67,
    SYS_SENDMSG = 68,
    SYS_RECVMSG = 69,
    SYS_THREAD_CREATE = 70,
    SYS_THREAD_EXIT = 71,
    SYS_THREAD_JOIN = 72,
    SYS_THREAD_SELF = 73,
    SYS_ADDR_WAIT = 74,
    SYS_ADDR_WAKE = 75,
    SYS_MPROTECT = 76,
}
