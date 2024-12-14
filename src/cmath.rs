//! Re-export Ldexp and Frexp from c so we can match the go implementation
//! Alas, I am committing some crimes. Rust deprecated and removed these methods
//! on f64 years ago, but it doesn't appear there are easy to use alternatives
//! so I'm re-exporting them from c. I think this will match whatever the other
//! otel implementations use, but who knows. The rust one just does a software
//! implementation, but I'm not actually sure libm backs these with something
//! hw anyway.
//! Don't use these, they are only here for the log-exponential histogram stuff
use libc::c_int;

mod ffi {
    use libc::{c_double, c_int};

    extern "C" {
        pub fn ldexp(x: c_double, n: c_int) -> c_double;
        pub fn frexp(n: c_double, value: &mut c_int) -> c_double;
    }
}

pub fn ldexp(frac: f64, exp: isize) -> f64 {
    unsafe { ffi::ldexp(frac, exp as c_int) }
}

pub fn frexp(value: f64) -> (f64, isize) {
    unsafe {
        let mut exp = 0;
        let x = ffi::frexp(value, &mut exp);
        (x, exp as isize)
    }
}
