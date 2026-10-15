//! `f32_round` of the Bend compiler: a decimal text rounded once to the
//! nearest f32, with an exact comparison at midpoints.

use std::cmp::Ordering;

/// An unsigned integer of any size, little-endian in 32-bit limbs.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Big(Vec<u32>);

impl Big {
    fn from_u64(n: u64) -> Self {
        let mut big = Self(vec![
            u32::try_from(n & 0xffff_ffff).unwrap_or(0),
            u32::try_from(n >> 32).unwrap_or(0),
        ]);
        big.trim();
        big
    }

    fn trim(&mut self) {
        while self.0.last() == Some(&0) {
            self.0.pop();
        }
    }

    fn mul_small(&mut self, m: u32) {
        let mut carry = 0u64;
        for limb in &mut self.0 {
            let product = u64::from(*limb) * u64::from(m) + carry;
            *limb = u32::try_from(product & 0xffff_ffff).unwrap_or(0);
            carry = product >> 32;
        }
        if carry > 0 {
            self.0.push(u32::try_from(carry).unwrap_or(0));
        }
        self.trim();
    }

    fn add_small(&mut self, a: u32) {
        let mut carry = u64::from(a);
        for limb in &mut self.0 {
            if carry == 0 {
                return;
            }
            let sum = u64::from(*limb) + carry;
            *limb = u32::try_from(sum & 0xffff_ffff).unwrap_or(0);
            carry = sum >> 32;
        }
        if carry > 0 {
            self.0.push(u32::try_from(carry).unwrap_or(0));
        }
    }

    fn from_decimal(digits: &str) -> Self {
        let mut big = Self(Vec::new());
        for d in digits.bytes() {
            big.mul_small(10);
            big.add_small(u32::from(d - b'0'));
        }
        big
    }

    fn shl(&mut self, bits: u32) {
        for _ in 0..bits {
            self.mul_small(2);
        }
    }

    fn mul_pow10(&mut self, n: u32) {
        for _ in 0..n {
            self.mul_small(10);
        }
    }

    /// An f64 that holds an integer, exactly.
    fn from_f64_integer(v: f64) -> Self {
        let bits = v.to_bits();
        let exponent = i32::try_from((bits >> 52) & 0x7ff).unwrap_or(0);
        let fraction = bits & ((1 << 52) - 1);
        if exponent == 0 {
            return Self::from_u64(0);
        }
        let mantissa = fraction | (1 << 52);
        let shift = exponent - 1075;
        if shift >= 0 {
            let mut big = Self::from_u64(mantissa);
            big.shl(shift.unsigned_abs());
            big
        } else {
            Self::from_u64(mantissa >> shift.unsigned_abs().min(63))
        }
    }

    fn cmp(&self, other: &Self) -> Ordering {
        self.0
            .len()
            .cmp(&other.0.len())
            .then_with(|| self.0.iter().rev().cmp(other.0.iter().rev()))
    }
}

/// The nearest f32 to `x`, ties to even, as IEEE 754 and `Math.fround` round.
pub(super) fn to_f32(x: f64) -> f32 {
    let bits = x.to_bits();
    let sign = u32::from(bits >> 63 == 1) << 31;
    let exponent = i32::try_from((bits >> 52) & 0x7ff).unwrap_or(0);
    let fraction = bits & ((1 << 52) - 1);
    if exponent == 0x7ff {
        return f32::from_bits(
            sign | if fraction == 0 {
                0x7f80_0000
            } else {
                0x7fc0_0000
            },
        );
    }
    if exponent == 0 {
        return f32::from_bits(sign);
    }
    let mantissa = fraction | (1 << 52);
    let power = exponent - 1023;
    let shift = if power < -126 {
        29 + (-126 - power)
    } else {
        29
    };
    if shift > 60 {
        return f32::from_bits(sign);
    }
    let shift = shift.unsigned_abs();
    let mut kept = mantissa >> shift;
    let rest = mantissa & ((1 << shift) - 1);
    let half = 1 << (shift - 1);
    if rest > half || (rest == half && kept & 1 == 1) {
        kept += 1;
    }
    if power < -126 {
        return f32::from_bits(sign | u32::try_from(kept).unwrap_or(0));
    }
    let mut biased = power + 127;
    if kept == 1 << 24 {
        kept >>= 1;
        biased += 1;
    }
    if biased >= 0xff {
        return f32::from_bits(sign | 0x7f80_0000);
    }
    let field = u32::try_from(biased).unwrap_or(0) << 23;
    f32::from_bits(sign | field | (u32::try_from(kept).unwrap_or(0) & 0x7f_ffff))
}

fn fround(x: f64) -> f64 {
    f64::from(to_f32(x))
}

fn same(a: f64, b: f64) -> bool {
    a.partial_cmp(&b) == Some(Ordering::Equal)
}

fn sign(value: f64) -> f64 {
    if same(value, 0.0) {
        value
    } else {
        value.signum()
    }
}

/// `f32_round(s)` for a literal `\d+\.\d+([eE][+-]?\d+)?`.
pub(super) fn f32_round(literal: &str) -> f64 {
    let value: f64 = literal.parse().unwrap_or(f64::NAN);
    let magnitude = value.abs();
    let nearest = fround(magnitude);
    let other = 2.0 * magnitude - nearest.min(2f64.powi(128));
    if same(other, nearest) || !same(fround(other), other) || same(other, f64::INFINITY) {
        return sign(value) * nearest;
    }
    let mut scale = 0;
    while !same((magnitude * 2f64.powi(scale)) % 1.0, 0.0) {
        scale += 1;
    }
    let lower = literal.to_ascii_lowercase();
    let (digits, exponent) = lower.split_once('e').unwrap_or((&lower, "0"));
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    let exponent: i64 = exponent.parse().unwrap_or(0);
    let tens = exponent - i64::try_from(fraction.len()).unwrap_or(0);
    let mut text = Big::from_decimal(&format!("{whole}{fraction}"));
    text.shl(scale.unsigned_abs());
    text.mul_pow10(u32::try_from(tens.max(0)).unwrap_or(0));
    let mut exact = Big::from_f64_integer(magnitude * 2f64.powi(scale));
    exact.mul_pow10(u32::try_from((-tens).max(0)).unwrap_or(0));
    let order = text.cmp(&exact);
    let pick = if order == Ordering::Equal || (order == Ordering::Greater) != (other > nearest) {
        nearest
    } else {
        other
    };
    sign(value) * pick
}

#[cfg(test)]
mod tests {
    use super::{f32_round, to_f32};

    #[test]
    fn rounds_plain_values_like_fround() {
        assert_eq!(f32_round("1.5").to_bits(), 1.5f64.to_bits());
        assert_eq!(f32_round("0.1").to_bits(), f64::from(0.1f32).to_bits());
        assert!(f32_round("1.0e39").is_infinite());
    }

    #[test]
    fn rounds_a_double_rounding_midpoint_by_the_exact_text() {
        let low = f32::from_bits(0x3f80_0000);
        let high = f32::from_bits(0x3f80_0001);
        let midpoint = f64::midpoint(f64::from(low), f64::from(high));
        let above = format!("{midpoint:.30}1");
        assert_eq!(f32_round(&above).to_bits(), f64::from(high).to_bits());
        let exact = format!("{midpoint:.30}");
        assert_eq!(f32_round(&exact).to_bits(), f64::from(low).to_bits());
    }

    #[test]
    fn converts_to_f32_with_ties_to_even() {
        let cases: [(u64, u32); 10] = [
            (0x3ff0_0000_0000_0000, 0x3f80_0000),
            (0x3ff0_0000_1000_0000, 0x3f80_0000),
            (0x3ff0_0000_3000_0000, 0x3f80_0002),
            (0x3ff0_0000_1000_0001, 0x3f80_0001),
            (0x47ef_ffff_f000_0000, 0x7f80_0000),
            (0x47ef_ffff_e000_0000, 0x7f7f_ffff),
            (0x36a0_0000_0000_0000, 0x0000_0001),
            (0x3690_0000_0000_0000, 0x0000_0000),
            (0x3690_0000_0000_0001, 0x0000_0001),
            (0xbff0_0000_0000_0000, 0xbf80_0000),
        ];
        for (input, output) in cases {
            assert_eq!(to_f32(f64::from_bits(input)).to_bits(), output, "{input:x}");
        }
        assert!(to_f32(f64::NAN).is_nan());
        assert_eq!(to_f32(f64::INFINITY).to_bits(), 0x7f80_0000);
    }
}
