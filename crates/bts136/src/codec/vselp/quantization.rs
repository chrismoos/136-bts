use super::{LPC_COEFFICIENTS, R0_BITS};

const ENERGY_STEP_DB: f64 = 2.0;
const ENERGY_OFFSET_DB: f64 = 66.0;
const POWER_DB_DIVISOR: f64 = 10.0;
const ENERGY_SILENCE_DB: f64 = -72.0;
pub(super) const SILENCE_ENERGY_CODE: u8 = 0;
const MIN_NONZERO_ENERGY_CODE: u8 = 1;
const MAX_ENERGY_CODE: u8 = (1 << R0_BITS) - 1;
// IS-54-B §2.1.3.3.2.5.2 retains nonzero codes at exactly -72 dB.
pub(super) fn encode_energy_db(energy_db: f64) -> u8 {
    if energy_db < ENERGY_SILENCE_DB {
        return SILENCE_ENERGY_CODE;
    }
    let target = (energy_db + ENERGY_OFFSET_DB) / ENERGY_STEP_DB;
    let mut best = MIN_NONZERO_ENERGY_CODE;
    let mut distance = (target - f64::from(best)).abs();
    for code in MIN_NONZERO_ENERGY_CODE + 1..=MAX_ENERGY_CODE {
        let candidate = (target - f64::from(code)).abs();
        if candidate < distance {
            best = code;
            distance = candidate;
        }
    }
    best
}
pub(super) fn frame_energy(code: u8) -> f64 {
    if code == 0 {
        return 0.0;
    }
    10.0f64.powf((ENERGY_STEP_DB * f64::from(code) - ENERGY_OFFSET_DB) / POWER_DB_DIVISOR)
}
pub(super) fn reflections(codes: &[u8; LPC_COEFFICIENTS]) -> [f64; LPC_COEFFICIENTS] {
    std::array::from_fn(|coefficient| reflection(coefficient, codes[coefficient]))
}
pub(super) fn reflection(coefficient: usize, code: u8) -> f64 {
    REFLECTION_TABLES[coefficient][usize::from(code)]
}
// IS-54-B §2.1.3.3.2.4.3 minimizes absolute error, with lower code used for equal distances.
pub(super) fn nearest_reflection(coefficient: usize, value: f64) -> u8 {
    nearest_code(REFLECTION_TABLES[coefficient], value)
}
fn nearest_code(table: &[f64], value: f64) -> u8 {
    let mut best = 0;
    let mut distance = (value - table[0]).abs();
    for (code, level) in table.iter().copied().enumerate().skip(1) {
        let candidate = (value - level).abs();
        if candidate < distance {
            best = code;
            distance = candidate;
        }
    }
    best as u8
}
// IS-54-B §§2.1.3.3.2.4.1 and 2.1.3.3.2.4.5, synthesis denominator is 1 minus the predictor sum.
pub(super) fn direct_form(reflections: &[f64; LPC_COEFFICIENTS]) -> [f64; LPC_COEFFICIENTS] {
    let mut predictor = [0.0; LPC_COEFFICIENTS];
    for (stage, reflection) in reflections.iter().copied().enumerate() {
        let previous = predictor;
        for coefficient in 0..stage {
            predictor[coefficient] =
                previous[coefficient] + reflection * previous[stage - 1 - coefficient];
        }
        predictor[stage] = -reflection;
    }
    predictor
}
// IS-54-B Table 2.1.3.3.2.4.3-1, decimal values retained at the published precision.
const REFLECTION_TABLES: [&[f64]; LPC_COEFFICIENTS] = [
    &[
        -0.9867044091E+00,
        -0.9810330868E+00,
        -0.9762308002E+00,
        -0.9711073637E+00,
        -0.9655630589E+00,
        -0.9597059488E+00,
        -0.9536622763E+00,
        -0.9471911192E+00,
        -0.9406521916E+00,
        -0.9339897037E+00,
        -0.9266145825E+00,
        -0.9190770388E+00,
        -0.9110740423E+00,
        -0.9032388926E+00,
        -0.8951876163E+00,
        -0.8865973353E+00,
        -0.8775991797E+00,
        -0.8679655194E+00,
        -0.8578169942E+00,
        -0.8468435407E+00,
        -0.8350597620E+00,
        -0.8232460022E+00,
        -0.8109311461E+00,
        -0.7979614735E+00,
        -0.7842678428E+00,
        -0.7699828744E+00,
        -0.7545526624E+00,
        -0.7377604842E+00,
        -0.7188396454E+00,
        -0.6990101337E+00,
        -0.6768131256E+00,
        -0.6533866525E+00,
        -0.6262212992E+00,
        -0.5971570611E+00,
        -0.5684631467E+00,
        -0.5378258228E+00,
        -0.5058867931E+00,
        -0.4740323126E+00,
        -0.4414438009E+00,
        -0.4054017663E+00,
        -0.3682330251E+00,
        -0.3293099701E+00,
        -0.2894666791E+00,
        -0.2428349406E+00,
        -0.1948891282E+00,
        -0.1466629505E+00,
        -0.9152601659E-01,
        -0.2692181431E-01,
        0.3727672249E-01,
        0.1093522757E+00,
        0.1758577228E+00,
        0.2397777289E+00,
        0.3001485765E+00,
        0.3555985391E+00,
        0.4108347893E+00,
        0.4679426551E+00,
        0.5202128887E+00,
        0.5746787190E+00,
        0.6337370872E+00,
        0.6966888309E+00,
        0.7613552213E+00,
        0.8211135268E+00,
        0.8759805560E+00,
        0.9311733246E+00,
    ],
    &[
        -0.7547348738E+00,
        -0.5826729536E+00,
        -0.4569368660E+00,
        -0.3481135964E+00,
        -0.2492762953E+00,
        -0.1585778296E+00,
        -0.7726432383E-01,
        -0.5096863955E-02,
        0.6527176499E-01,
        0.1329884380E+00,
        0.1978287548E+00,
        0.2600678802E+00,
        0.3186267912E+00,
        0.3747462034E+00,
        0.4288900495E+00,
        0.4810178876E+00,
        0.5302551389E+00,
        0.5760165453E+00,
        0.6193220615E+00,
        0.6593915224E+00,
        0.6967787147E+00,
        0.7315257788E+00,
        0.7650170326E+00,
        0.7966732979E+00,
        0.8267812133E+00,
        0.8543012142E+00,
        0.8798117638E+00,
        0.9037305117E+00,
        0.9251338840E+00,
        0.9448361397E+00,
        0.9636774063E+00,
        0.9816107750E+00,
    ],
    &[
        -0.8606231213E+00,
        -0.8046579361E+00,
        -0.7523136735E+00,
        -0.7056827545E+00,
        -0.6582847834E+00,
        -0.6130494475E+00,
        -0.5684005022E+00,
        -0.5247684717E+00,
        -0.4832728207E+00,
        -0.4436871707E+00,
        -0.4048590660E+00,
        -0.3659544587E+00,
        -0.3276270330E+00,
        -0.2901176810E+00,
        -0.2521926463E+00,
        -0.2139451057E+00,
        -0.1760748774E+00,
        -0.1370347440E+00,
        -0.9637858719E-01,
        -0.5570860580E-01,
        -0.1342663728E-01,
        0.2913235873E-01,
        0.7243801653E-01,
        0.1183170006E+00,
        0.1668847799E+00,
        0.2185972333E+00,
        0.2741918266E+00,
        0.3353714645E+00,
        0.4032742083E+00,
        0.4797808230E+00,
        0.5761802793E+00,
        0.6969622374E+00,
    ],
    &[
        -0.4505536556E+00,
        -0.2582354248E+00,
        -0.1276191175E+00,
        -0.1891620830E-01,
        0.7355944812E-01,
        0.1581759751E+00,
        0.2381238639E+00,
        0.3138437271E+00,
        0.3864109516E+00,
        0.4573097229E+00,
        0.5282919407E+00,
        0.5986876488E+00,
        0.6679506302E+00,
        0.7368153930E+00,
        0.8054329753E+00,
        0.8715001345E+00,
    ],
    &[
        -0.6570090652E+00,
        -0.5339469314E+00,
        -0.4366474450E+00,
        -0.3503953516E+00,
        -0.2729992270E+00,
        -0.2044571489E+00,
        -0.1395732164E+00,
        -0.7738068700E-01,
        -0.1441930979E-01,
        0.4928108677E-01,
        0.1149446666E+00,
        0.1853068322E+00,
        0.2629969418E+00,
        0.3558021188E+00,
        0.4658669233E+00,
        0.6091661453E+00,
    ],
    &[
        -0.3351013660E+00,
        -0.1157702208E+00,
        0.3265872598E-01,
        0.1545225680E+00,
        0.2685563564E+00,
        0.3836385608E+00,
        0.5067840815E+00,
        0.6525010467E+00,
    ],
    &[
        -0.5834863186E+00,
        -0.4363777936E+00,
        -0.3007811308E+00,
        -0.1750739664E+00,
        -0.5848890170E-01,
        0.5718512833E-01,
        0.1880214661E+00,
        0.3601035774E+00,
    ],
    &[
        -0.3832900822E+00,
        -0.2111644298E+00,
        -0.7242984325E-01,
        0.4901395738E-01,
        0.1637183726E+00,
        0.2812094688E+00,
        0.4174390733E+00,
        0.5824319720E+00,
    ],
    &[
        -0.5373110771E+00,
        -0.3927696943E+00,
        -0.2594878078E+00,
        -0.1391013712E+00,
        -0.2935140580E-01,
        0.8255130798E-01,
        0.2107033432E+00,
        0.3958446980E+00,
    ],
    &[
        -0.1508204788E+00,
        0.4652437568E-01,
        0.1958054304E+00,
        0.3621688187E+00,
    ],
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::vselp::{LPC_WIDTHS, SUBFRAMES, VselpParameters, VselpSubframe};
    #[test]
    fn all_196_quantizer_levels_are_ordered_finite_and_within_stability_bounds() {
        assert_eq!(
            REFLECTION_TABLES
                .iter()
                .map(|table| table.len())
                .sum::<usize>(),
            196
        );
        for (table, width) in REFLECTION_TABLES.into_iter().zip(LPC_WIDTHS) {
            assert_eq!(table.len(), 1 << width);
            assert!(
                table
                    .iter()
                    .all(|value| value.is_finite() && value.abs() < 1.0)
            );
            assert!(table.windows(2).all(|pair| pair[0] < pair[1]));
        }
    }
    #[test]
    fn reflection_lookup_matches_fixed_codeword_vector_including_ocr_sensitive_decimal() {
        let codes = [42, 19, 13, 9, 6, 5, 3, 1, 7, 2];
        assert_eq!(
            reflections(&codes),
            [
                -0.2894666791,
                0.6593915224,
                -0.2901176810,
                0.4573097229,
                -0.1395732164,
                0.3836385608,
                -0.1750739664,
                -0.2111644298,
                0.3958446980,
                0.1958054304
            ]
        );
        let mut codes = [0; LPC_COEFFICIENTS];
        codes[7] = 2;
        assert_eq!(reflections(&codes)[7], -0.07242984325);
    }
    #[test]
    fn frame_energy_zero_is_silence_and_nonzero_codes_have_two_db_spacing() {
        assert_eq!(frame_energy(0), 0.0);
        assert!((frame_energy(1) - 3.981071705534969e-7).abs() < 1e-18);
        assert!((frame_energy(31) - 0.3981071705534972).abs() < 1e-14);
        let ratio = 10.0f64.powf(0.2);
        for code in 1..31 {
            assert!((frame_energy(code + 1) / frame_energy(code) - ratio).abs() < 1e-12);
        }
        let parameters = VselpParameters::try_new(
            21,
            [0; LPC_COEFFICIENTS],
            [VselpSubframe::default(); SUBFRAMES],
        )
        .unwrap();
        assert!(
            (parameters.frame_energy_full_scale_squared() - 0.003981071705534973).abs() < 1e-15
        );
    }
    #[test]
    fn predictor_sign_matches_first_and_second_order_inverse_lattice_equations() {
        let mut reflections = [0.0; LPC_COEFFICIENTS];
        reflections[0] = 0.5;
        reflections[1] = -0.25;
        let predictor = direct_form(&reflections);
        assert_eq!(predictor[0], -0.375);
        assert_eq!(predictor[1], 0.25);
        assert!(predictor[2..].iter().all(|value| *value == 0.0));
        reflections[0] = -0.5;
        reflections[1] = 0.0;
        assert_eq!(direct_form(&reflections)[0], 0.5);
    }
}
#[test]
fn nearest_quantization_recovers_every_published_level_and_both_sides_of_each_gap() {
    for (coefficient, table) in REFLECTION_TABLES.into_iter().enumerate() {
        for (code, value) in table.iter().copied().enumerate() {
            assert_eq!(nearest_reflection(coefficient, value), code as u8);
        }
        for (code, pair) in table.windows(2).enumerate() {
            assert_eq!(
                nearest_reflection(coefficient, pair[0] * 0.75 + pair[1] * 0.25),
                code as u8
            );
            assert_eq!(
                nearest_reflection(coefficient, pair[0] * 0.25 + pair[1] * 0.75),
                (code + 1) as u8
            );
        }
        assert_eq!(nearest_reflection(coefficient, -1.0), 0);
        assert_eq!(
            nearest_reflection(coefficient, 1.0),
            (table.len() - 1) as u8
        );
    }
    assert_eq!(nearest_code(&[-0.5, 0.5], 0.0), 0);
}
#[test]
fn energy_encoding_uses_the_silence_threshold_saturation_and_lower_code_for_exact_ties() {
    assert_eq!(encode_energy_db(-72.000001), 0);
    assert_eq!(encode_energy_db(-72.0), 1);
    assert_eq!(encode_energy_db(-70.0), 1);
    assert_eq!(encode_energy_db(0.0), 31);
    assert_eq!(encode_energy_db(600.0), 31);
    for code in 1..=31 {
        let db = f64::from(code) * 2.0 - 66.0;
        assert_eq!(encode_energy_db(db), code);
        if code < 31 {
            assert_eq!(encode_energy_db(db + 1.0), code);
            assert_eq!(encode_energy_db(db + 1.000001), code + 1);
            assert_eq!(encode_energy_db(db + 0.999999), code);
        }
    }
}
