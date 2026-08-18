macro_rules! limit {
    ($min:ident = $min_val:expr ; $actual:ident = $val:expr) => {
        const $min: usize = $min_val;
        pub const $actual: usize = $val;
        const _: () = {
            assert!($actual >= $min);
        };
    };
}

limit!(_POSIX_ARG_MAX = 4096 ; ARG_MAX = 32 * 4096);
limit!(_POSIX_PATH_MAX = 256 ; PATH_MAX = 4096);
