use crate::{EINVAL, Errno, ProcessId};

pub type SigSet = u64;

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct SigInfo {
    pub signo: Signal,
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

    #[must_use]
    pub const fn new(addr: usize) -> Self {
        Self(addr)
    }

    #[must_use]
    pub const fn addr(self) -> usize {
        self.0
    }

    #[must_use]
    pub fn is_default(self) -> bool {
        self == Self::DEFAULT
    }

    #[must_use]
    pub fn is_ignore(self) -> bool {
        self == Self::IGNORE
    }
}

#[repr(transparent)]
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SaFlags(u32);

impl SaFlags {
    pub const RESTART: Self = Self(1 << 0);
    pub const SIGINFO: Self = Self(1 << 1);
    pub const NODEFER: Self = Self(1 << 2);
    pub const RESETHAND: Self = Self(1 << 3);

    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone, Default, PartialEq, Eq)]
pub struct SigAction {
    pub handler: SigHandler,
    pub mask: SigSet,
    pub flags: SaFlags,
    pub restorer: usize,
}

macro_rules! signo {
    ($($(#[$m:meta])* $name:ident = $val:literal => $str:literal),*,) => {
        #[repr(i32)]
        #[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd)]
        pub enum Signal {
            $(
                $($m)*
                $name = $val,
            )*
        }

        impl Signal {
            pub const COUNT: usize = [$($val),*].len();

            #[must_use]
            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$name => $str,)*
                }
            }
        }

        impl TryFrom<i32> for Signal {
            type Error = Errno;

            fn try_from(value: i32) -> Result<Self, Self::Error> {
                match value {
                    $($val => Ok(Self::$name),)*
                    _ => Err(EINVAL),
                }
            }
        }
    };
}

signo! {
    Abort = 1 => "SIGABRT",
    Alarm = 2 => "SIGALRM",
    Bus = 3 => "SIGBUS",
    Child = 4 => "SIGCHLD",
    Continue = 5 => "SIGCONT",
    Fpe = 6 => "SIGFPE",
    Hangup = 7 => "SIGHUP",
    IllegalInstruction = 8 => "SIGILL",
    Interrupt = 9 => "SIGINT",
    Kill = 10 => "SIGKILL",
    Pipe = 11 => "SIGPIPE",
    Quit = 12 => "SIGQUIT",
    Segfault = 13 => "SIGSEGV",
    Stop = 14 => "SIGSTOP",
    Terminate = 15 => "SIGTERM",
    TerminalStop = 16 => "SIGTSTP",
    TerminalInput = 17 => "SIGTTIN",
    TerminalOutput = 18 => "SIGTTOU",
    Usr1 = 19 => "SIGUSR1",
    Usr2 = 20 => "SIGUSR2",
    BadSyscall = 22 => "SIGSYS",
    Trap = 23 => "SIGTRAP",
    Urgent = 24 => "SIGURG",
    VirtualTimerAlarm = 25 => "SIGVTALRM",
    ExceededCpuLimit = 26 => "SIGXCPU",
    ExceededFileSizeLimit = 27 => "SIGXFSZ",
}

impl Signal {
    #[must_use]
    pub const fn number(self) -> i32 {
        self as i32
    }

    #[must_use]
    pub const fn bit(self) -> SigSet {
        1 << (self as i32 - 1)
    }
}

pub const STOP_SIGNALS_MASK: SigSet = Signal::Stop.bit()
    | Signal::TerminalStop.bit()
    | Signal::TerminalInput.bit()
    | Signal::TerminalOutput.bit();

#[repr(usize)]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum SigMaskHow {
    Block = 0,
    Unblock = 1,
    SetMask = 2,
}

impl TryFrom<usize> for SigMaskHow {
    type Error = Errno;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Block),
            1 => Ok(Self::Unblock),
            2 => Ok(Self::SetMask),
            _ => Err(EINVAL),
        }
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum DefaultAction {
    Terminate,
    Ignore,
    Stop,
}

#[must_use]
pub fn default_action(signo: Signal) -> DefaultAction {
    match signo {
        Signal::Child | Signal::Urgent | Signal::Continue => DefaultAction::Ignore,
        Signal::Stop | Signal::TerminalStop | Signal::TerminalInput | Signal::TerminalOutput => {
            DefaultAction::Stop
        }
        Signal::Abort
        | Signal::Alarm
        | Signal::Bus
        | Signal::Fpe
        | Signal::Hangup
        | Signal::IllegalInstruction
        | Signal::Interrupt
        | Signal::Kill
        | Signal::Pipe
        | Signal::Quit
        | Signal::Segfault
        | Signal::Terminate
        | Signal::Usr1
        | Signal::Usr2
        | Signal::BadSyscall
        | Signal::Trap
        | Signal::VirtualTimerAlarm
        | Signal::ExceededCpuLimit
        | Signal::ExceededFileSizeLimit => DefaultAction::Terminate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_try_from_bounds() {
        for n in 1..=27 {
            let signal = Signal::try_from(n);
            assert!(signal.is_ok(), "signal {n} must parse");
            assert_eq!(signal.unwrap().number(), n, "roundtrip mismatch for {n}");
        }
        for n in [i32::MIN, -1, 0, 28, 64, 65, i32::MAX] {
            assert_eq!(
                Signal::try_from(n),
                Err(EINVAL),
                "signal {n} must be rejected"
            );
        }
    }

    #[test]
    fn signal_bit_convention() {
        assert_eq!(Signal::Abort.bit(), 1, "lowest signal occupies bit 0");
        assert_eq!(Signal::Kill.bit(), 1 << 9, "bit is signo - 1");
    }

    #[test]
    fn signal_names() {
        assert_eq!(Signal::Fpe.name(), "SIGFPE", "SIGPFE typo must be fixed");
        assert_eq!(Signal::ExceededFileSizeLimit.name(), "SIGXFSZ", "last variant name mismatch");
    }
}
