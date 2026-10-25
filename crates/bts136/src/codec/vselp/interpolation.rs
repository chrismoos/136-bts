use super::{LPC_COEFFICIENTS, PredictorOrigin, SUBFRAMES, SubframePredictor};

const CURRENT_WEIGHTS: [f64; SUBFRAMES] = [0.25, 0.5, 0.75, 1.0];
const SECOND_SUBFRAME: usize = 1;
const FOURTH_SUBFRAME: usize = SUBFRAMES - 1;
// IS-54-B §2.1.3.3.2.4.6 rejects any reflection coefficient with magnitude at least one.
pub(super) fn is_stable(predictor: &[f64; LPC_COEFFICIENTS]) -> bool {
    reflection_coefficients(predictor).is_some()
}
pub(super) fn reflection_coefficients(
    predictor: &[f64; LPC_COEFFICIENTS],
) -> Option<[f64; LPC_COEFFICIENTS]> {
    if predictor.iter().any(|value| !value.is_finite()) {
        return None;
    }
    let mut remaining = *predictor;
    let mut reflections = [0.0; LPC_COEFFICIENTS];
    for stage in (0..LPC_COEFFICIENTS).rev() {
        let reflection = -remaining[stage];
        if !reflection.is_finite() || reflection.abs() >= 1.0 {
            return None;
        }
        reflections[stage] = reflection;
        let divisor = 1.0 - reflection * reflection;
        let current = remaining;
        for coefficient in 0..stage {
            remaining[coefficient] =
                (current[coefficient] - reflection * current[stage - 1 - coefficient]) / divisor;
        }
    }
    Some(reflections)
}
// IS-54-B §§2.1.3.3.2.4.6 and 2.1.3.3.2.5.3, predictor and energy interpolation are distinct.
pub(super) fn interpolate(
    previous: &[f64; LPC_COEFFICIENTS],
    current: &[f64; LPC_COEFFICIENTS],
    previous_energy: f64,
    current_energy: f64,
) -> [SubframePredictor; SUBFRAMES] {
    std::array::from_fn(|subframe| {
        let energy_full_scale_squared = match subframe {
            0 => previous_energy,
            SECOND_SUBFRAME => (previous_energy * current_energy).sqrt(),
            _ => current_energy,
        };
        if subframe == FOURTH_SUBFRAME {
            return SubframePredictor {
                coefficients: *current,
                energy_full_scale_squared,
                origin: PredictorOrigin::CurrentFrame,
            };
        }
        let weight = CURRENT_WEIGHTS[subframe];
        let interpolated = std::array::from_fn(|coefficient| {
            (1.0 - weight) * previous[coefficient] + weight * current[coefficient]
        });
        let (coefficients, origin) = if is_stable(&interpolated) {
            (interpolated, PredictorOrigin::Interpolated)
        } else if subframe == 0
            || (subframe == SECOND_SUBFRAME && previous_energy >= current_energy)
        {
            (*previous, PredictorOrigin::PreviousFrame)
        } else {
            (*current, PredictorOrigin::CurrentFrame)
        };
        SubframePredictor {
            coefficients,
            energy_full_scale_squared,
            origin,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::vselp::{VselpParameters, VselpSubframe};
    fn pair(previous_r0: u8, current_r0: u8) -> [VselpParameters; 2] {
        [
            VselpParameters::try_new(
                previous_r0,
                [1, 30, 29, 2, 4, 5, 3, 6, 3, 3],
                [VselpSubframe::default(); SUBFRAMES],
            )
            .unwrap(),
            VselpParameters::try_new(
                current_r0,
                [58, 29, 15, 13, 15, 4, 3, 2, 5, 3],
                [VselpSubframe::default(); SUBFRAMES],
            )
            .unwrap(),
        ]
    }
    #[test]
    fn stable_first_order_predictors_use_all_four_weights_and_geometric_energy() {
        let mut previous = [0.0; LPC_COEFFICIENTS];
        previous[0] = 0.5;
        let mut current = [0.0; LPC_COEFFICIENTS];
        current[0] = -0.5;
        let result = interpolate(&previous, &current, 0.01, 1.0);
        assert_eq!(
            result.map(|predictor| predictor.coefficients[0]),
            [0.25, 0.0, -0.25, -0.5]
        );
        assert_eq!(
            result.map(|predictor| predictor.energy_full_scale_squared),
            [0.01, 0.1, 1.0, 1.0]
        );
        assert_eq!(
            result.map(|predictor| predictor.origin),
            [
                PredictorOrigin::Interpolated,
                PredictorOrigin::Interpolated,
                PredictorOrigin::Interpolated,
                PredictorOrigin::CurrentFrame
            ]
        );
    }
    #[test]
    fn unstable_interpolation_selects_previous_first_current_third_and_higher_energy_second() {
        let [previous, current] = pair(1, 31);
        let previous_coefficients = previous.predictor_coefficients();
        let current_coefficients = current.predictor_coefficients();
        assert!(is_stable(&previous_coefficients));
        assert!(is_stable(&current_coefficients));
        let result = current.subframe_predictors(&previous);
        assert_eq!(
            result.map(|predictor| predictor.origin),
            [
                PredictorOrigin::PreviousFrame,
                PredictorOrigin::CurrentFrame,
                PredictorOrigin::CurrentFrame,
                PredictorOrigin::CurrentFrame
            ]
        );
        assert_eq!(result[0].coefficients, previous_coefficients);
        for subframe in &result[1..] {
            assert_eq!(subframe.coefficients, current_coefficients);
        }
        let [previous, current] = pair(31, 1);
        assert_eq!(
            current.subframe_predictors(&previous)[1].origin,
            PredictorOrigin::PreviousFrame
        );
    }
    #[test]
    fn energy_tie_selects_previous_fallback_and_silence_interpolates_to_zero() {
        let [previous, current] = pair(21, 21);
        let predictors = current.subframe_predictors(&previous);
        assert_eq!(predictors[1].origin, PredictorOrigin::PreviousFrame);
        assert_eq!(
            predictors[1].coefficients,
            previous.predictor_coefficients()
        );
        let [previous, current] = pair(0, 31);
        let energies = current
            .subframe_predictors(&previous)
            .map(|predictor| predictor.energy_full_scale_squared);
        assert_eq!(energies[0], 0.0);
        assert_eq!(energies[1], 0.0);
        assert_eq!(energies[2], current.frame_energy_full_scale_squared());
        assert_eq!(energies[3], current.frame_energy_full_scale_squared());
    }
    #[test]
    fn stability_rejects_unit_poles_and_nonfinite_values_without_margin_override() {
        let mut predictor = [0.0; LPC_COEFFICIENTS];
        assert!(is_stable(&predictor));
        predictor[0] = 1.0;
        assert!(!is_stable(&predictor));
        predictor[0] = -1.0;
        assert!(!is_stable(&predictor));
        predictor[0] = 1.0 - f64::EPSILON;
        assert!(is_stable(&predictor));
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            predictor[4] = bad;
            assert!(!is_stable(&predictor));
        }
    }
}
