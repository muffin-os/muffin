use jiff::Timestamp;

use crate::BOOT_TIME_SECONDS;
use crate::hpet::hpet;

pub trait TimestampExt {
    fn now() -> Self;
}

impl TimestampExt for Timestamp {
    fn now() -> Self {
        let since_boot = hpet().read().now().since_boot();
        let secs = BOOT_TIME_SECONDS.get().unwrap() + since_boot.as_secs();
        Timestamp::new(
            i64::try_from(secs).expect("shouldn't have more seconds than i64::MAX"),
            since_boot.subsec_nanos().cast_signed(),
        )
        .unwrap()
    }
}
