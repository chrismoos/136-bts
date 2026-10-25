use super::VselpParameters;
use crate::phy::speech::CrcStatus;

const ATTENUATION_CODE_STEP: u8 = 2;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Clear,
    RepeatOne,
    RepeatTwo,
    AttenuateOne,
    AttenuateTwo,
    AttenuateThree,
    Muted,
    Recovering,
}
#[derive(Debug, Clone, PartialEq)]
pub(super) struct BadFrameMasker {
    state: State,
    last_good: VselpParameters,
}
impl BadFrameMasker {
    pub(super) fn new(last_good: VselpParameters) -> Self {
        Self {
            state: State::Clear,
            last_good,
        }
    }
    pub(super) fn trusted_frame(&self) -> &VselpParameters {
        &self.last_good
    }
    // IS-54-B §2.2.2.2.3.2 retains received excitation and requires two good CRCs after state six.
    pub(super) fn apply(&mut self, received: &VselpParameters, crc: CrcStatus) -> VselpParameters {
        self.state = match (self.state, crc) {
            (State::Muted, CrcStatus::Valid) => State::Recovering,
            (State::Recovering, CrcStatus::Valid) => State::Clear,
            (_, CrcStatus::Valid) => State::Clear,
            (State::Clear, CrcStatus::Failed) => State::RepeatOne,
            (State::RepeatOne, CrcStatus::Failed) => State::RepeatTwo,
            (State::RepeatTwo, CrcStatus::Failed) => State::AttenuateOne,
            (State::AttenuateOne, CrcStatus::Failed) => State::AttenuateTwo,
            (State::AttenuateTwo, CrcStatus::Failed) => State::AttenuateThree,
            (_, CrcStatus::Failed) => State::Muted,
        };
        if self.state == State::Clear {
            self.last_good = received.clone();
            return received.clone();
        }
        let mut masked = received.clone();
        masked.lpc = self.last_good.lpc;
        masked.r0 = self.last_good.r0;
        let attenuation_steps = match self.state {
            State::AttenuateOne => 1,
            State::AttenuateTwo => 2,
            State::AttenuateThree => 3,
            State::Muted | State::Recovering => {
                masked.r0 = 0;
                0
            }
            _ => 0,
        };
        for _ in 0..attenuation_steps {
            if masked.r0 > ATTENUATION_CODE_STEP {
                masked.r0 -= ATTENUATION_CODE_STEP;
            }
        }
        masked
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::vselp::{LPC_COEFFICIENTS, SUBFRAMES, VselpSubframe};
    fn frame(r0: u8, code: u8) -> VselpParameters {
        VselpParameters::try_new(
            r0,
            [code; LPC_COEFFICIENTS],
            [VselpSubframe::try_new(code, code, code, code).unwrap(); SUBFRAMES],
        )
        .unwrap()
    }
    #[test]
    fn consecutive_failures_repeat_attenuate_and_mute_only_energy_and_predictors() {
        let good = frame(20, 1);
        let bad = frame(31, 2);
        let mut masker = BadFrameMasker::new(good.clone());
        for expected_r0 in [20, 20, 18, 16, 14, 0, 0] {
            let masked = masker.apply(&bad, CrcStatus::Failed);
            assert_eq!(masked.r0(), expected_r0);
            assert_eq!(masked.lpc(), good.lpc());
            assert_eq!(masked.subframes(), bad.subframes());
        }
    }
    #[test]
    fn attenuation_stops_at_low_nonzero_codes_until_the_sixth_failure() {
        for (r0, expected) in [
            (0, [0, 0, 0]),
            (1, [1, 1, 1]),
            (2, [2, 2, 2]),
            (3, [1, 1, 1]),
            (4, [2, 2, 2]),
            (5, [3, 1, 1]),
            (6, [4, 2, 2]),
        ] {
            let mut masker = BadFrameMasker::new(frame(r0, 1));
            let bad = frame(31, 2);
            for _ in 0..2 {
                masker.apply(&bad, CrcStatus::Failed);
            }
            for r0 in expected {
                assert_eq!(masker.apply(&bad, CrcStatus::Failed).r0(), r0);
            }
            assert_eq!(masker.apply(&bad, CrcStatus::Failed).r0(), 0);
        }
    }
    #[test]
    fn muted_recovery_requires_two_consecutive_good_frames_and_preserves_last_trusted_predictor() {
        let good = frame(20, 1);
        let bad = frame(31, 2);
        let new_good = frame(10, 3);
        let mut masker = BadFrameMasker::new(good.clone());
        for _ in 0..6 {
            masker.apply(&bad, CrcStatus::Failed);
        }
        let recovering = masker.apply(&new_good, CrcStatus::Valid);
        assert_eq!(recovering.r0(), 0);
        assert_eq!(recovering.lpc(), good.lpc());
        masker.apply(&bad, CrcStatus::Failed);
        assert_eq!(masker.apply(&new_good, CrcStatus::Valid), recovering);
        assert_eq!(masker.apply(&new_good, CrcStatus::Valid), new_good);
        let next_bad = masker.apply(&bad, CrcStatus::Failed);
        assert_eq!(next_bad.r0(), new_good.r0());
        assert_eq!(next_bad.lpc(), new_good.lpc());
    }
    #[test]
    fn a_good_frame_before_muting_immediately_resets_failure_count() {
        let good = frame(20, 1);
        let bad = frame(31, 2);
        for failures in 1..=5 {
            let mut masker = BadFrameMasker::new(good.clone());
            for _ in 0..failures {
                masker.apply(&bad, CrcStatus::Failed);
            }
            assert_eq!(masker.apply(&good, CrcStatus::Valid), good);
            assert_eq!(masker.apply(&bad, CrcStatus::Failed).r0(), good.r0());
        }
    }
}
