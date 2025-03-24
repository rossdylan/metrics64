//! Reimplementations of musl libc's ldexp and frexp functions. Rust chose not to expose these from libm or implement
//! them itself. I've chosen to copy the musl implementations into rust to avoid any in release-mode.
//! The FFI that is here exists to ensure cross-compatibility by double checking the implementation against w/e is
//! linked into rust.

#[cfg(debug_assertions)]
mod ffi {
    unsafe extern "C" {
        pub fn ldexp(x: f64, n: i32) -> f64;
        pub fn frexp(n: f64, value: &mut i32) -> f64;
    }
}

/// A naive rust copy of the musl libc ldexp function. We aren't aiming for speed
/// just matching behavior and safety. I want to be able to remove the libc ffi
/// calls here.
/// Reference: https://git.musl-libc.org/cgit/musl/tree/src/math/scalbn.c
pub fn ldexp(frac: f64, exp: i32) -> f64 {
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
    #[cfg(debug_assertions)]
    {
        let unsafe_res = unsafe { ffi::ldexp(frac, exp) };
        if !(unsafe_res.is_nan() && res.is_nan()) {
            debug_assert_eq!(res, unsafe_res, "ldexp mismatch for {frac}, {exp}");
        }
    }
    res
}

pub fn frexp(value: f64) -> (f64, i32) {
    let mut safe_exp = 0;
    let safe_x = frexp_inner(value, &mut safe_exp);
    #[cfg(debug_assertions)]
    {
        let mut exp = 0;
        let x = unsafe { ffi::frexp(value, &mut exp) };
        if x.is_nan() && safe_x.is_nan() {
            debug_assert_eq!(exp, safe_exp, "frexp result mismatch for {value}");
        } else {
            debug_assert_eq!(
                (x, exp),
                (safe_x, safe_exp),
                "safe frexp doesn't match libc for {value}"
            );
        }
    }
    (safe_x, safe_exp)
}

// A naive port of the musl libc frexp method. The biggest notes here are that
// instead of using a union to transmute between u64 and f64 we use the native
// [`f64::from_bits`] and [`f64::to_bits`] methods.
// Reference: https://git.musl-libc.org/cgit/musl/tree/src/math/frexp.c
fn frexp_inner(mut x: f64, e: &mut i32) -> f64 {
    let mut i: u64 = x.to_bits();
    let ee: i32 = (i >> 52 & 0x7ff) as i32;
    if ee == 0 {
        if x != 0f64 {
            x = frexp_inner(x * 2f64.powi(64), e);
            *e -= 64;
        } else {
            *e = 0;
        }
        return x;
    } else if ee == 0x7ff {
        return x;
    }
    *e = ee - 0x3fe;
    i &= 0x800fffffffffffffu64;
    i |= 0x3fe0000000000000u64;
    f64::from_bits(i)
}
