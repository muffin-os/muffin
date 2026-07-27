use conquer_once::spin::Lazy;
use spin::Mutex;
use uart_16550::SerialPort;

static SERIAL1: Lazy<Mutex<SerialPort>> = Lazy::new(|| {
    let mut serial_port = unsafe { SerialPort::new(0x3F8) };
    serial_port.init();
    Mutex::new(serial_port)
});

/// Runs `f` while holding the serial lock with interrupts disabled.
///
/// One lock acquisition covers a whole log record so that no deadlock can occur
/// when we want to print something in an interrupt handler.
pub(crate) fn with_serial<R>(f: impl FnOnce(&mut SerialPort) -> R) -> R {
    use x86_64::instructions::interrupts;

    interrupts::without_interrupts(|| f(&mut SERIAL1.lock()))
}

#[doc(hidden)]
pub fn internal_print(args: core::fmt::Arguments) {
    use core::fmt::Write;

    with_serial(|serial| serial.write_fmt(args).expect("Printing to serial failed"));
}

/// Prints to the host through the serial interface.
#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => ($crate::serial::internal_print(format_args!($($arg)*)));
}

/// Prints to the host through the serial interface, appending a newline.
#[macro_export]
macro_rules! serial_println {
    () => ($crate::serial_print!("\n"));
    ($fmt:expr) => ($crate::serial_print!(concat!($fmt, "\n")));
    ($fmt:expr, $($arg:tt)*) => ($crate::serial_print!(
        concat!($fmt, "\n"), $($arg)*));
}
