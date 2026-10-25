use std::f64::consts::FRAC_PI_4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dibit {
    ZeroZero,
    ZeroOne,
    OneZero,
    OneOne,
}

impl Dibit {
    pub fn bits(self) -> [bool; 2] {
        match self {
            Self::ZeroZero => [false, false],
            Self::ZeroOne => [false, true],
            Self::OneZero => [true, false],
            Self::OneOne => [true, true],
        }
    }

    pub fn from_bits(bits: [bool; 2]) -> Self {
        match bits {
            [false, false] => Self::ZeroZero,
            [false, true] => Self::ZeroOne,
            [true, false] => Self::OneZero,
            [true, true] => Self::OneOne,
        }
    }

    // IS-136.2-A §2.1.3.3.1, Xk is the first transmitted bit.
    pub fn phase_quarters(self) -> i8 {
        match self {
            Self::ZeroZero => 1,
            Self::ZeroOne => 3,
            Self::OneZero => -1,
            Self::OneOne => -3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Symbol {
    pub i: f64,
    pub q: f64,
}

impl Symbol {
    pub const REFERENCE: Self = Self { i: 1.0, q: 0.0 };
}

#[derive(Debug, Default)]
pub struct DifferentialEncoder {
    phase: u8,
}

impl DifferentialEncoder {
    pub fn encode(&mut self, dibit: Dibit) -> Symbol {
        const CONSTELLATION_POINTS: i8 = 8;
        self.phase =
            (self.phase as i8 + dibit.phase_quarters()).rem_euclid(CONSTELLATION_POINTS) as u8;
        let (q, i) = (f64::from(self.phase) * FRAC_PI_4).sin_cos();
        Symbol { i, q }
    }
}

pub fn decode(previous: Symbol, current: Symbol) -> Option<Dibit> {
    if ![previous.i, previous.q, current.i, current.q]
        .iter()
        .all(|x| x.is_finite())
    {
        return None;
    }
    let real = current.i * previous.i + current.q * previous.q;
    let imag = current.q * previous.i - current.i * previous.q;
    if real == 0.0 && imag == 0.0 {
        return None;
    }
    Some(match (real >= 0.0, imag >= 0.0) {
        (true, true) => Dibit::ZeroZero,
        (false, true) => Dibit::ZeroOne,
        (true, false) => Dibit::OneZero,
        (false, false) => Dibit::OneOne,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_phase_table() {
        let root = std::f64::consts::FRAC_1_SQRT_2;
        for (dibit, i, q) in [
            (Dibit::ZeroZero, root, root),
            (Dibit::ZeroOne, -root, root),
            (Dibit::OneZero, root, -root),
            (Dibit::OneOne, -root, -root),
        ] {
            let symbol = DifferentialEncoder::default().encode(dibit);
            assert!((symbol.i - i).abs() < 1e-12);
            assert!((symbol.q - q).abs() < 1e-12);
            assert_eq!(decode(Symbol::REFERENCE, Symbol { i, q }), Some(dibit));
        }
    }

    #[test]
    fn preserves_phase_across_blocks() {
        let mut encoder = DifferentialEncoder::default();
        for _ in 0..8 {
            encoder.encode(Dibit::ZeroZero);
        }
        assert_eq!(encoder.phase, 0);
        assert_eq!(decode(Symbol { i: 0.0, q: 0.0 }, Symbol::REFERENCE), None);
    }
}
