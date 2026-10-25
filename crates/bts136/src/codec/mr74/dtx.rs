const HANGOVER_FRAMES: i16 = 7;
const ELAPSED_FRAMES_THRESHOLD: i16 = 30;
const MAX_EMPTY_FRAMES: i16 = 50;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceivedFrame {
    SpeechGood,
    SpeechDegraded,
    SpeechBad,
    Onset,
    SidFirst,
    SidUpdate,
    SidBad,
    NoData,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    Speech,
    #[default]
    ComfortNoise,
    Muted,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SidData {
    #[default]
    None,
    Invalid,
    Valid,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    pub previous_mode: Mode,
    pub mode: Mode,
    pub sid_data: SidData,
    pub hangover_added: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiverState {
    mode: Mode,
    since_last_sid: i16,
    elapsed_frames: i16,
    hangover_frames: i16,
    hangover_added: bool,
    sid_data: SidData,
    data_updated: bool,
}
impl Default for ReceiverState {
    fn default() -> Self {
        Self {
            mode: Mode::ComfortNoise,
            since_last_sid: 0,
            elapsed_frames: i16::MAX,
            hangover_frames: HANGOVER_FRAMES,
            hangover_added: false,
            sid_data: SidData::None,
            data_updated: false,
        }
    }
}
impl ReceiverState {
    pub(super) fn current_mode(&self) -> Mode {
        self.mode
    }

    pub(super) fn sid_age_frames(&self) -> u16 {
        self.since_last_sid as u16
    }
    pub(super) fn has_parameters(&self) -> bool {
        self.data_updated
    }
    pub(super) fn noise_muted(&mut self) {
        self.since_last_sid = 0;
    }
    pub(super) fn backward_parameters_applied(&mut self) {
        self.since_last_sid = 0;
        self.data_updated = true;
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
    /// TS 26.073 rx_dtx_handler decisions precede comfort-noise synthesis and parameter updates.
    pub fn advance(&mut self, frame: ReceivedFrame) -> Decision {
        let previous_mode = self.mode;
        let sid = matches!(
            frame,
            ReceivedFrame::SidFirst | ReceivedFrame::SidUpdate | ReceivedFrame::SidBad
        );
        let absent = matches!(
            frame,
            ReceivedFrame::NoData | ReceivedFrame::SpeechBad | ReceivedFrame::Onset
        );
        let mut mode = if sid || (previous_mode != Mode::Speech && absent) {
            Mode::ComfortNoise
        } else {
            Mode::Speech
        };
        if mode != Mode::Speech {
            if previous_mode == Mode::Muted
                && matches!(
                    frame,
                    ReceivedFrame::SidBad
                        | ReceivedFrame::SidFirst
                        | ReceivedFrame::Onset
                        | ReceivedFrame::NoData
                )
            {
                mode = Mode::Muted;
            }
            self.since_last_sid = self.since_last_sid.saturating_add(1);
            if frame != ReceivedFrame::SidUpdate && self.since_last_sid > MAX_EMPTY_FRAMES {
                mode = Mode::Muted;
            }
        } else {
            self.since_last_sid = 0;
        }
        if !self.data_updated && frame == ReceivedFrame::SidUpdate {
            self.elapsed_frames = 0;
        }
        self.elapsed_frames = self.elapsed_frames.saturating_add(1);
        self.hangover_added = false;
        let encoder_noise = sid || matches!(frame, ReceivedFrame::Onset | ReceivedFrame::NoData);
        if !encoder_noise || (frame == ReceivedFrame::NoData && mode == Mode::Speech) {
            self.hangover_frames = HANGOVER_FRAMES;
        } else if self.elapsed_frames > ELAPSED_FRAMES_THRESHOLD {
            self.hangover_added = true;
            self.elapsed_frames = 0;
            self.hangover_frames = 0;
        } else if self.hangover_frames == 0 {
            self.elapsed_frames = 0;
        } else {
            self.hangover_frames -= 1;
        }
        if mode != Mode::Speech {
            self.sid_data = match frame {
                ReceivedFrame::SidFirst | ReceivedFrame::SidBad => SidData::Invalid,
                ReceivedFrame::SidUpdate => SidData::Valid,
                _ => SidData::None,
            };
            if frame == ReceivedFrame::SidBad {
                self.hangover_added = false;
            }
        }
        self.mode = mode;
        Decision {
            previous_mode,
            mode,
            sid_data: if mode == Mode::Speech {
                SidData::None
            } else {
                self.sid_data
            },
            hangover_added: self.hangover_added,
        }
    }
    /// Call after valid SID parameters are applied. Receipt alone must not refresh their age.
    pub fn parameters_applied(&mut self) -> bool {
        if self.mode == Mode::Speech || self.sid_data != SidData::Valid {
            return false;
        }
        self.since_last_sid = 0;
        self.data_updated = true;
        true
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    const FRAMES: [ReceivedFrame; 8] = [
        ReceivedFrame::SpeechGood,
        ReceivedFrame::SpeechDegraded,
        ReceivedFrame::SpeechBad,
        ReceivedFrame::Onset,
        ReceivedFrame::SidFirst,
        ReceivedFrame::SidUpdate,
        ReceivedFrame::SidBad,
        ReceivedFrame::NoData,
    ];
    #[test]
    fn dtx_decisions_counters_and_sid_flags_match_reference_across_all_short_sequences() {
        let mut hash = 0xcbf29ce484222325u64;
        for mode in [Mode::Speech, Mode::ComfortNoise, Mode::Muted] {
            for age in [0, 49, 50, i16::MAX] {
                for updated in [false, true] {
                    for sequence in 0..4096 {
                        let mut state = ReceiverState {
                            mode,
                            since_last_sid: age,
                            data_updated: updated,
                            ..ReceiverState::default()
                        };
                        for step in 0..4 {
                            let frame = FRAMES[(sequence >> (step * 3)) & 7];
                            let decision = state.advance(frame);
                            let code = match decision.mode {
                                Mode::Speech => 0,
                                Mode::ComfortNoise => 1,
                                Mode::Muted => 2,
                            };
                            for value in [
                                code,
                                state.since_last_sid,
                                state.elapsed_frames,
                                state.hangover_frames,
                                i16::from(state.hangover_added),
                                i16::from(state.sid_data != SidData::None),
                                i16::from(state.sid_data == SidData::Valid),
                                i16::from(state.data_updated),
                            ] {
                                for byte in value.to_le_bytes() {
                                    hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                                }
                            }
                            if frame == ReceivedFrame::SidUpdate && sequence & 1 != 0 {
                                assert!(state.parameters_applied());
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(hash, 0x7b683abe88ea7a31);
    }
    #[test]
    fn stale_sid_mutes_but_valid_late_update_can_resume_noise() {
        let mut state = ReceiverState::default();
        assert!(!state.parameters_applied());
        for _ in 0..MAX_EMPTY_FRAMES {
            assert_eq!(
                state.advance(ReceivedFrame::NoData).mode,
                Mode::ComfortNoise
            );
        }
        assert_eq!(state.advance(ReceivedFrame::NoData).mode, Mode::Muted);
        assert_eq!(
            state.advance(ReceivedFrame::SidUpdate).mode,
            Mode::ComfortNoise
        );
        assert!(state.parameters_applied());
        assert_eq!(state.since_last_sid, 0);
        state.reset();
        assert_eq!(state, ReceiverState::default());
    }
}
