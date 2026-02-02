use crate::ProcessId;

pub type SigSet = u64;

pub type SignalNumber = u8;

#[repr(C)]
#[derive(Debug, Copy, Clone, Default)]
pub struct SigInfo {
    pub signo: i32,
    pub code: i32,
    pub errno: i32,

    pub info: SigInfoField,
}

#[derive(Debug, Default, Copy, Clone)]
pub enum SigInfoField {
    #[default]
    None,
    Kill {
        pid: ProcessId,
        uid: u32,
    },
    Fault {
        addr: usize,
        trap: i32,
    },
    Timer {
        id: i32,
        val: u64,
    },
    Child {
        pid: ProcessId,
        status: i32,
        uid: u32,
    },
}

#[repr(transparent)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SigHandler(usize);

impl Default for SigHandler {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl SigHandler {
    pub const DEFAULT: Self = Self(0);
    pub const IGNORE: Self = Self(1);
}

#[repr(transparent)]
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SaFlags(u32);

impl SaFlags {
    pub const RESTART: Self = Self(1 << 0);
    pub const SIGINFO: Self = Self(1 << 1);
    pub const NODEFER: Self = Self(1 << 2);
    pub const RESETHAND: Self = Self(1 << 3);
}

#[repr(C)]
#[derive(Debug, Copy, Clone, Default)]
pub struct SigAction {
    pub handler: SigHandler,
    pub mask: SigSet,
    pub flags: SaFlags,
}

macro_rules! signo {
    ($($name:ident = $val:expr),*,) => {
        $(pub const $name: i32 = $val;)*

        #[allow(dead_code)]
        #[must_use] pub fn signal_name(v: i32) -> &'static str {
            match v {
                $( $val => stringify!($name), )*
                _ => "<unknown>",
            }
        }
    };
}

signo! {
    SIGABRT = 1,
    SIGALRM = 2,
    SIGBUS = 3,
    SIGCHLD = 4,
    SIGCONT = 5,
    SIGPFE = 6,
    SIGHUP = 7,
    SIGILL = 8,
    SIGINT = 9,
    SIGKILL = 10,
    SIGPIPE = 11,
    SIGQUIT = 12,
    SIGSEGV = 13,
    SIGSTOP = 14,
    SIGTERM = 15,
    SIGTSTP = 16,
    SIGTTIN = 17,
    SIGTTOU = 18,
    SIGUSR1 = 19,
    SIGUSR2 = 20,
    SIGWINCH = 21,
    SIGSYS = 22,
    SIGTRAP = 23,
    SIGURG = 24,
    SIGVTALRM = 25,
    SIGXCPU = 26,
    SIGXFSZ = 27,
}
