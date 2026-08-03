use jiff::Timestamp;

use crate::BOOT_TIME_SECONDS;
use crate::hpet::hpet;

pub trait TimestampExt {
    fn now() -> Self;
}

impl TimestampExt for Timestamp {
    fn now() -> Self {
        let ns = hpet().read().elapsed_ns();
        let secs = BOOT_TIME_SECONDS.get().unwrap();
        let secs = secs + (ns / 1_000_000_000);
        Timestamp::new(
            i64::try_from(secs).expect("shouldn't have more seconds than i64::MAX"),
            (ns % 1_000_000_000) as i32,
        )
        .unwrap()
    }
}
