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

/// A naive rust copy of the musl libc ldexp function
pub fn ldexp(frac: f64, exp: isize) -> f64 {
    let mut y = frac;
    let mut n = exp;

    if n > 1023 {
        y *= 2_f64.powi(1023);
        n -= 1023;
        if n > 1023 {
            y *= 2_f64.powi(1023);
            n -= 1023;
            if n > 1023 {
                n = 1023;
            }
        }
    } else if n < -1022 {
        /* make sure final n < -53 to avoid double
        rounding in the subnormal range */
        y *= 2_f64.powi(-1022) * 2_f64.powi(53);
        n += 1022 - 53;
        if n < -1022 {
            y *= 2_f64.powi(-1022) * 2_f64.powi(53);
            n += 1022 - 53;
            if n < -1022 {
                n = -1022;
            }
        }
    }
    let int: u64 = ((0x3ff + n) as u64) << 52;
    let float: f64 = f64::from_bits(int);
    let res = y * float;
    debug_assert_eq!(res, unsafe { ffi::ldexp(frac, exp as c_int) });
    res
}

//fn frexp_inner(value: f64, out: &mut isize) {
//    let int = value.to_bits() >> 52 & 07ff;
//	if (!int) {
//		if (value) {
//			 = frexp_inner(x* 2_f64.powi(64), out);
//			*e -= 64;
//		} else {
//		  *e = 0;
//		}
//		return x;
//	} else if (ee == 0x7ff) {
//		return x;
//	}
//
//	*e = ee - 0x3fe;
//	y.i &= 0x800fffffffffffffull;
//	y.i |= 0x3fe0000000000000ull;
//	return y.d;
//}

pub fn frexp(value: f64) -> (f64, isize) {
    unsafe {
        let mut exp = 0;
        let x = ffi::frexp(value, &mut exp);
        (x, exp as isize)
    }
}
