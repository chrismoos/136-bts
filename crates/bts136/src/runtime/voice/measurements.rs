use std::collections::VecDeque;
// IS-54-B §§2.7.3.1.1–2 place FACCH across two slots and SACCH across a word plus diagonal delay.
const FACCH_SLOTS: usize = 2;
const SACCH_SLOTS: usize = crate::phy::sacch::WORD_SLOTS + crate::phy::traffic::SACCH_BITS - 1;
pub(super) struct ReportOwners<T> {
    transmitted: Option<(T, u64)>,
    previous: Option<(u64, u64)>,
    slots: VecDeque<Option<T>>,
}
impl<T> Default for ReportOwners<T> {
    fn default() -> Self {
        Self {
            transmitted: None,
            previous: None,
            slots: VecDeque::with_capacity(SACCH_SLOTS),
        }
    }
}
impl<T: PartialEq> ReportOwners<T> {
    pub(super) fn transmitted(&mut self, owner: T, interval: u64) {
        if self
            .transmitted
            .as_ref()
            .is_none_or(|(current, _)| *current != owner)
        {
            self.transmitted = Some((owner, interval));
        }
    }
    pub(super) fn push(&mut self, interval: u64, segment: u64, owner: Option<T>) {
        if self.previous.is_some_and(|(last, previous_segment)| {
            last.checked_add(1) != Some(interval) || previous_segment != segment
        }) {
            self.slots.clear();
        }
        if self.slots.len() == SACCH_SLOTS {
            self.slots.pop_front();
        }
        let owner = owner.filter(|owner| {
            self.transmitted
                .as_ref()
                .is_some_and(|(transmitted, after)| owner == transmitted && interval > *after)
        });
        self.slots.push_back(owner);
        self.previous = Some((interval, segment));
    }
    pub(super) fn facch(&self) -> Option<&T> {
        self.owner(FACCH_SLOTS)
    }
    pub(super) fn sacch(&self) -> Option<&T> {
        self.owner(SACCH_SLOTS)
    }
    fn owner(&self, slots: usize) -> Option<&T> {
        if self.slots.len() < slots {
            return None;
        }
        let current = self.slots.back()?.as_ref()?;
        self.slots
            .iter()
            .rev()
            .take(slots)
            .all(|owner| owner.as_ref() == Some(current))
            .then_some(current)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn each_channel_requires_its_complete_physical_window() {
        let mut owners = ReportOwners::default();
        owners.transmitted(1, 0);
        for interval in 0..SACCH_SLOTS {
            owners.push(interval as u64 + 1, 0, Some(1));
            assert_eq!(owners.facch(), (interval + 1 >= FACCH_SLOTS).then_some(&1));
            assert_eq!(owners.sacch(), (interval + 1 >= SACCH_SLOTS).then_some(&1));
        }
        owners.transmitted(2, SACCH_SLOTS as u64);
        owners.push(SACCH_SLOTS as u64 + 1, 0, Some(2));
        assert_eq!(owners.facch(), None);
        assert_eq!(owners.sacch(), None);
        for offset in 1..SACCH_SLOTS {
            owners.push((SACCH_SLOTS + offset + 1) as u64, 0, Some(2));
            assert_eq!(owners.facch(), Some(&2));
            assert_eq!(owners.sacch(), (offset + 1 >= SACCH_SLOTS).then_some(&2));
        }
    }
    #[test]
    fn stop_and_unsubmitted_requests_cannot_own_buffered_reports() {
        let mut owners = ReportOwners::default();
        owners.transmitted(1, 0);
        for interval in 0..SACCH_SLOTS {
            owners.push(interval as u64 + 1, 0, Some(1));
        }
        owners.push(SACCH_SLOTS as u64 + 1, 0, None);
        assert_eq!(owners.facch(), None);
        assert_eq!(owners.sacch(), None);
        owners.transmitted(2, SACCH_SLOTS as u64 + 1);
        owners.push((SACCH_SLOTS + 2) as u64, 0, Some(2));
        assert_eq!(owners.facch(), None);
        assert_eq!(owners.sacch(), None);
    }
    #[test]
    fn missing_slots_and_sample_segments_break_ownership_continuity() {
        for segment_change in [true, false] {
            let mut owners = ReportOwners::default();
            owners.transmitted(1, 0);
            for interval in 0..SACCH_SLOTS {
                owners.push(interval as u64 + 1, 0, Some(1));
            }
            let next = SACCH_SLOTS as u64 + 1 + u64::from(!segment_change);
            owners.push(next, u64::from(segment_change), Some(1));
            assert_eq!(owners.facch(), None);
            assert_eq!(owners.sacch(), None);
        }
    }
    #[test]
    fn queued_and_buffered_pretransmission_slots_cannot_own_reports_and_retries_preserve_boundary()
    {
        let mut owners = ReportOwners::default();
        for interval in 0..SACCH_SLOTS {
            owners.push(interval as u64, 0, Some(1));
        }
        assert_eq!(owners.facch(), None);
        assert_eq!(owners.sacch(), None);
        owners.transmitted(1, 100);
        for interval in 80..=100 {
            owners.push(interval, 0, Some(1));
            assert_eq!(owners.facch(), None);
            assert_eq!(owners.sacch(), None);
        }
        for offset in 1..=SACCH_SLOTS {
            owners.push(100 + offset as u64, 0, Some(1));
        }
        assert_eq!(owners.sacch(), Some(&1));
        owners.transmitted(1, 200);
        owners.push(100 + SACCH_SLOTS as u64 + 1, 0, Some(1));
        assert_eq!(owners.sacch(), Some(&1));
    }
    #[test]
    fn coded_reports_use_the_owner_of_all_contributing_slots() {
        use crate::{
            codec::vselp::VselpParameters,
            l3::{
                broadcast::ProtocolVersion,
                legacy_traffic::{
                    ExtendedChannelQualityReport, ExtendedCurrentQuality, NeighborGroup,
                    TrafficMessage,
                },
                relay::Direction,
            },
            link::{
                bearer_rx::BearerRx,
                traffic::{self, SignalingWord},
                voice::TransmitFrame,
            },
            phy::{sacch::SacchEncoder, sync::SyncWord, traffic as physical},
        };
        const DVCC: u8 = 10;
        let mut reports = vec![ExtendedChannelQualityReport::First {
            current: ExtendedCurrentQuality {
                ber_code: 1,
                measurement_code: 2,
                rssi_code: 3,
            },
            neighbor_rssi_codes: [1, 2, 3, 4],
        }];
        reports.extend(
            [
                NeighborGroup::Second,
                NeighborGroup::Third,
                NeighborGroup::Fourth,
            ]
            .map(|group| ExtendedChannelQualityReport::Later {
                group,
                neighbor_rssi_codes: [1, 2, 3, 4, 5, 6],
            }),
        );
        for report in reports {
            for transition in [None, Some(4), Some(SACCH_SLOTS - 1)] {
                let message = TrafficMessage::ExtendedChannelQuality(report.clone());
                let words = traffic::encode(&message).unwrap();
                assert_eq!(words.len(), 1);
                let mut encoder = SacchEncoder::default();
                let mut stream = encoder
                    .encode_word(false, &words[0].contents, DVCC)
                    .to_vec();
                for _ in 0..3 {
                    stream.extend(encoder.encode_word(
                        false,
                        &SignalingWord::filler().contents,
                        DVCC,
                    ));
                }
                let speech = TransmitFrame::speech(&VselpParameters::default());
                let mut previous = speech.clone();
                let mut receiver = BearerRx::with_version(
                    DVCC,
                    VselpParameters::default(),
                    Direction::Reverse,
                    ProtocolVersion::Pv2,
                );
                let mut owners = ReportOwners::default();
                owners.transmitted(1, 0);
                let mut facch = vec![];
                let mut sacch = vec![];
                for (index, field) in stream.into_iter().enumerate() {
                    let interval = index as u64 + 1;
                    if transition == Some(index) {
                        owners.transmitted(2, interval - 1);
                    }
                    let owner = if transition.is_some_and(|at| index >= at) {
                        2
                    } else {
                        1
                    };
                    owners.push(interval, 0, Some(owner));
                    let current = if index == 3 {
                        TransmitFrame::facch(&words[0], DVCC)
                    } else {
                        speech.clone()
                    };
                    let bits = physical::encode_forward(
                        SyncWord::One,
                        &field,
                        &TransmitFrame::interleave(&previous, &current),
                        DVCC,
                        0,
                    )
                    .unwrap();
                    let fields = physical::decode_forward(&bits).unwrap().fields;
                    let result = receiver.push(interval, 0, Some(&fields)).unwrap();
                    if result.facch_message.unwrap() == Some(message.clone()) {
                        facch.push((index, owners.facch().copied()));
                    }
                    if result.sacch_message.unwrap() == Some(message.clone()) {
                        sacch.push((index, owners.sacch().copied()));
                    }
                    previous = current;
                }
                assert_eq!(
                    facch,
                    [(4, if transition == Some(4) { None } else { Some(1) })]
                );
                assert_eq!(
                    sacch,
                    [(
                        SACCH_SLOTS - 1,
                        if transition.is_some() { None } else { Some(1) }
                    )]
                );
            }
        }
    }
}
