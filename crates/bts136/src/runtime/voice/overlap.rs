use crate::{
    l3::legacy_traffic::MAX_ALIGNMENT_UNITS, phy::acquisition::channel_profile::ChannelProfile,
};
const OBSERVATIONS: usize = 3;
// TIA/EIA-136-131-C §1.4 assigns three symbols each to guard and power ramp-up.
const GUARD_SYMBOLS: f64 = 3.0;
const EDGE_MARGIN_SYMBOLS: f64 = 0.5;
const MIN_OVERLAP_SYMBOLS: f64 = 0.5;
const HALF_SYMBOLS_PER_SYMBOL: f64 = 2.0;
#[derive(Debug, Clone, Copy)]
pub(super) struct Observation {
    pub slot: u64,
    pub segment: u64,
    pub arrival_symbols: f64,
    pub confirmed_units: u8,
    pub profile: ChannelProfile,
}
impl Observation {
    fn propagation(self) -> f64 {
        self.arrival_symbols + f64::from(self.confirmed_units) / HALF_SYMBOLS_PER_SYMBOL
    }
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Decision {
    Targets {
        units: [u8; 2],
        arrival_symbols: [f64; 2],
    },
    Uncorrectable,
}
#[derive(Default)]
pub(super) struct Tracker {
    latest: [Option<Observation>; 2],
    history: [Option<[Observation; 2]>; OBSERVATIONS],
    count: usize,
    cursor: usize,
    last_pair: Option<(u64, u64)>,
    pub overlap_symbols: Option<f64>,
    pub detections: u64,
    pub armed: bool,
    pub constrained: bool,
}
impl Tracker {
    // TIA-136-133-E §4.2.1 requires a timing-control message on detected head/tail overlap.
    pub fn observe(
        &mut self,
        party: usize,
        observation: Observation,
        manual: bool,
    ) -> Option<Decision> {
        if self
            .latest
            .iter()
            .flatten()
            .any(|old| old.segment != observation.segment)
        {
            self.latest = [None; 2];
            self.history = [None; OBSERVATIONS];
            self.count = 0;
            self.cursor = 0;
            self.last_pair = None;
        }
        self.latest[party] = Some(observation);
        let [Some(a), Some(b)] = self.latest else {
            return None;
        };
        if a.slot.abs_diff(b.slot) != 1 {
            return None;
        }
        let (pair, order) = if a.slot < b.slot {
            ([a, b], [0, 1])
        } else {
            ([b, a], [1, 0])
        };
        let key = (pair[0].segment, pair[0].slot);
        if self.last_pair == Some(key) {
            return None;
        }
        self.last_pair = Some(key);
        // The common 162-symbol slot duration cancels at an adjacent-slot boundary.
        let overlap = pair[0].arrival_symbols + f64::from(pair[0].profile.latest_symbol)
            - pair[1].arrival_symbols
            - GUARD_SYMBOLS
            - f64::from(pair[1].profile.earliest_symbol);
        self.overlap_symbols = Some(overlap.max(0.0));
        if overlap > MIN_OVERLAP_SYMBOLS {
            self.detections += 1;
            self.armed = true;
        }
        if manual || !self.armed {
            self.count = 0;
            self.cursor = 0;
            return None;
        }
        if self.count != 0
            && self.history[(self.cursor + OBSERVATIONS - 1) % OBSERVATIONS].is_some_and(
                |previous| {
                    previous
                        .iter()
                        .zip(pair)
                        .any(|(old, current)| old.confirmed_units != current.confirmed_units)
                },
            )
        {
            self.count = 0;
            self.cursor = 0;
            self.history = [None; OBSERVATIONS];
        }
        self.history[self.cursor] = Some(pair);
        self.cursor = (self.cursor + 1) % OBSERVATIONS;
        self.count = (self.count + 1).min(OBSERVATIONS);
        if self.count != OBSERVATIONS {
            return None;
        }
        let propagation: [f64; 2] = std::array::from_fn(|i| {
            median(self.history.map(|pair| {
                (pair.expect("complete observation window")[i].propagation()
                    * HALF_SYMBOLS_PER_SYMBOL)
                    .round()
                    / HALF_SYMBOLS_PER_SYMBOL
            }))
        });
        let earliest: [f64; 2] = std::array::from_fn(|i| {
            median(self.history.map(|pair| {
                f64::from(
                    pair.expect("complete observation window")[i]
                        .profile
                        .earliest_symbol,
                )
            }))
        });
        let latest: [f64; 2] = std::array::from_fn(|i| {
            median(self.history.map(|pair| {
                f64::from(
                    pair.expect("complete observation window")[i]
                        .profile
                        .latest_symbol,
                )
            }))
        });
        let head_limit: [f64; 2] = std::array::from_fn(|i| {
            ((propagation[i] + GUARD_SYMBOLS + earliest[i] - EDGE_MARGIN_SYMBOLS)
                * HALF_SYMBOLS_PER_SYMBOL)
                .floor()
        });
        let maximum: [i32; 2] =
            head_limit.map(|limit| limit.clamp(0.0, f64::from(MAX_ALIGNMENT_UNITS)) as i32);
        let following_minimum = ((propagation[1] + latest[1] - GUARD_SYMBOLS + EDGE_MARGIN_SYMBOLS)
            * HALF_SYMBOLS_PER_SYMBOL)
            .ceil()
            .max(0.0) as i32;
        let gap = ((propagation[0] + latest[0] - propagation[1] - GUARD_SYMBOLS - earliest[1]
            + EDGE_MARGIN_SYMBOLS)
            * HALF_SYMBOLS_PER_SYMBOL)
            .ceil() as i32;
        let following_maximum = maximum[1].min(maximum[0] - gap);
        self.constrained =
            following_maximum < following_minimum || head_limit.iter().any(|limit| *limit < 0.0);
        let ordered = if self.constrained {
            [maximum[0] as u8, following_minimum.min(maximum[1]) as u8]
        } else {
            let following = (propagation[1] * HALF_SYMBOLS_PER_SYMBOL)
                .round()
                .clamp(f64::from(following_minimum), f64::from(following_maximum))
                as i32;
            let preceding_minimum = (following + gap).max(0);
            let preceding = (propagation[0] * HALF_SYMBOLS_PER_SYMBOL)
                .round()
                .clamp(f64::from(preceding_minimum), f64::from(maximum[0]))
                as i32;
            [preceding as u8, following as u8]
        };
        if self.constrained
            && overlap > MIN_OVERLAP_SYMBOLS
            && pair
                .iter()
                .zip(ordered)
                .all(|(observation, target)| observation.confirmed_units == target)
        {
            return Some(Decision::Uncorrectable);
        }
        let mut units = [0; 2];
        let mut arrival_symbols = [0.0; 2];
        for i in 0..2 {
            units[order[i]] = ordered[i];
            arrival_symbols[order[i]] =
                propagation[i] - f64::from(ordered[i]) / HALF_SYMBOLS_PER_SYMBOL;
        }
        Some(Decision::Targets {
            units,
            arrival_symbols,
        })
    }
}
fn median(mut values: [f64; OBSERVATIONS]) -> f64 {
    values.sort_unstable_by(f64::total_cmp);
    values[OBSERVATIONS / 2]
}
#[cfg(test)]
mod tests;

impl super::VoiceRuntime {
    pub(super) fn receive_overlap(
        &mut self,
        (call, party): (usize, super::Party),
        physical_slot: u64,
        arrival_symbols: f64,
        segment: u64,
        profile: Option<ChannelProfile>,
        now_ms: u64,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(active) = self.calls.get_mut(call) else {
            return Ok(());
        };
        let Some(profile) = profile else {
            return Ok(());
        };
        if active.legs.iter().count() != 2 {
            return Ok(());
        }
        let leg = &mut active.legs[super::index(party)];
        leg.measurements.channel_profile = Some(profile);
        leg.measurements.channel_profile_ms = Some(now_ms);
        if active.session.state() != crate::core::call_session::CallState::Connected
            || [super::Party::Caller, super::Party::Callee]
                .iter()
                .any(|party| {
                    active.session.physical_control_state(*party)
                        != Some(crate::link::traffic_order::OrderState::Acknowledged)
                })
        {
            return Ok(());
        }
        let confirmed: [Option<u8>; 2] = std::array::from_fn(|i| {
            active.legs[i]
                .measurements
                .physical_control
                .as_ref()
                .and_then(|fields| match fields.control.time_alignment {
                    Some(crate::l3::designation::TimeAlignment::OffsetUnits(units)) => Some(units),
                    _ => None,
                })
        });
        let [Some(caller), Some(callee)] = confirmed else {
            return Ok(());
        };
        let confirmed = [caller, callee];
        let manual = active.legs.iter().any(|leg| leg.timing.manual);
        let decision = active.overlap.observe(
            super::index(party),
            Observation {
                slot: physical_slot,
                segment,
                arrival_symbols,
                confirmed_units: confirmed[super::index(party)],
                profile,
            },
            manual,
        );
        match decision {
            Some(Decision::Targets {
                units,
                arrival_symbols,
            }) => {
                for party in [super::Party::Caller, super::Party::Callee] {
                    let i = super::index(party);
                    active.legs[i].timing.set_target(arrival_symbols[i]);
                    if units[i] == confirmed[i] {
                        continue;
                    }
                    active.session.request_physical_control(
                        party,
                        crate::l3::legacy_traffic::PhysicalControlRequest {
                            time_alignment: Some(
                                crate::l3::designation::TimeAlignment::OffsetUnits(units[i]),
                            ),
                            ..Default::default()
                        },
                        now_ms,
                    )?;
                    tracing::info!(call_id=active.session.handle().id().value(), ?party, current_units=confirmed[i], target_units=units[i], overlap_symbols=?active.overlap.overlap_symbols, "correcting adjacent-phone burst overlap");
                }
            }
            Some(Decision::Uncorrectable) => {
                tracing::warn!(call_id=active.session.handle().id().value(), overlap_symbols=?active.overlap.overlap_symbols, "burst overlap remains after confirmed timing limits. Releasing call");
                active
                    .session
                    .end(crate::core::call_session::EndReason::TimingOverlap, now_ms)?;
            }
            None => {}
        }
        Ok(())
    }
}
