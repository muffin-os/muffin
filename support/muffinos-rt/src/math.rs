//! The C math functions that std and LLVM libcalls expect from the system `libm`.
//!
//! `compiler_builtins` exports these symbols only on non-unix targets, and
//! muffin is `cfg(unix)`, so the `libm` crate supplies them here. `sqrt`,
//! `floor`, `fma` and the other functions that `compiler_builtins` exports on
//! every target are absent on purpose: a second definition fails the link.

macro_rules! libm_exports {
    ($(fn $name:ident($($arg:ident: $ty:ty),*) -> $ret:ty;)*) => {
        $(
            #[unsafe(no_mangle)]
            pub extern "C" fn $name($($arg: $ty),*) -> $ret {
                libm::$name($($arg),*)
            }
        )*
    };
}

libm_exports! {
    fn acosf(x: f32) -> f32;
    fn acoshf(x: f32) -> f32;
    fn asinf(x: f32) -> f32;
    fn asinhf(x: f32) -> f32;
    fn atan2f(y: f32, x: f32) -> f32;
    fn atanf(x: f32) -> f32;
    fn cosf(x: f32) -> f32;
    fn coshf(x: f32) -> f32;
    fn erfcf(x: f32) -> f32;
    fn erff(x: f32) -> f32;
    fn exp2f(x: f32) -> f32;
    fn expf(x: f32) -> f32;
    fn expm1f(x: f32) -> f32;
    fn hypotf(x: f32, y: f32) -> f32;
    fn ldexpf(x: f32, n: i32) -> f32;
    fn log10f(x: f32) -> f32;
    fn log1pf(x: f32) -> f32;
    fn log2f(x: f32) -> f32;
    fn logf(x: f32) -> f32;
    fn powf(x: f32, y: f32) -> f32;
    fn sinf(x: f32) -> f32;
    fn sinhf(x: f32) -> f32;
    fn tanf(x: f32) -> f32;
    fn tanhf(x: f32) -> f32;
    fn tgammaf(x: f32) -> f32;

    fn acos(x: f64) -> f64;
    fn acosh(x: f64) -> f64;
    fn asin(x: f64) -> f64;
    fn asinh(x: f64) -> f64;
    fn atan(x: f64) -> f64;
    fn atan2(y: f64, x: f64) -> f64;
    fn cos(x: f64) -> f64;
    fn cosh(x: f64) -> f64;
    fn erf(x: f64) -> f64;
    fn erfc(x: f64) -> f64;
    fn exp(x: f64) -> f64;
    fn exp2(x: f64) -> f64;
    fn expm1(x: f64) -> f64;
    fn hypot(x: f64, y: f64) -> f64;
    fn ldexp(x: f64, n: i32) -> f64;
    fn log(x: f64) -> f64;
    fn log10(x: f64) -> f64;
    fn log1p(x: f64) -> f64;
    fn log2(x: f64) -> f64;
    fn pow(x: f64, y: f64) -> f64;
    fn sin(x: f64) -> f64;
    fn sinh(x: f64) -> f64;
    fn tan(x: f64) -> f64;
    fn tanh(x: f64) -> f64;
    fn tgamma(x: f64) -> f64;
}

#[unsafe(no_mangle)]
pub extern "C" fn lgammaf_r(x: f32, sign: &mut i32) -> f32 {
    let (value, value_sign) = libm::lgammaf_r(x);
    *sign = value_sign;
    value
}

#[unsafe(no_mangle)]
pub extern "C" fn lgamma_r(x: f64, sign: &mut i32) -> f64 {
    let (value, value_sign) = libm::lgamma_r(x);
    *sign = value_sign;
    value
}
