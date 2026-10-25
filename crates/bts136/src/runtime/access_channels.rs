use crate::{
    link::{
        feedback::{AccessFeedback, Availability, Reception},
        rach::{Outcome, RachLinkError, RachReceiver},
    },
    phy::{control::SharedFeedback, rach::ReverseControlFields},
};
use std::{error::Error as StdError, fmt};
const SUBCHANNELS: usize = 6;
const BLOCK_MS: u64 = 20;
// TIA/EIA-136-122-B Table 14 and §3.3.1.3 allow at most three repeats on the same subchannel.
const MAX_REPETITIONS: u8 = 3;
const MAX_BURST_OCCURRENCES: u64 = MAX_REPETITIONS as u64 + 1;
const MAX_PENDING_GAP_BLOCKS: u64 = SUBCHANNELS as u64 * MAX_BURST_OCCURRENCES;
const RESPONSE_BLOCK_OFFSET: u64 = 2;
const REVERSE_OFFSET_SYMBOLS: f64 = 207.0;
const SYMBOL_RATE_HZ: f64 = crate::phy::pulse::SYMBOL_RATE_HZ;
const SYMBOLS_PER_BLOCK: f64 = SYMBOL_RATE_HZ * BLOCK_MS as f64 / 1000.0;
const ALIGNMENT_TOLERANCE_SYMBOLS: f64 = 8.0;
#[derive(Debug)]
pub enum Error {
    Limits,
    Alignment,
    Timing {
        relative_symbols: f64,
        nearest_block: u64,
        error_symbols: f64,
        tolerance_symbols: f64,
    },
    RepeatedBlock {
        block: u64,
        previous_block: u64,
    },
    LateFeedback,
    Link(RachLinkError),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "access subchannel failed: {self:?}")
    }
}
impl StdError for Error {}
#[derive(Default)]
struct Channel {
    receiver: RachReceiver,
    pending: bool,
    last_block: Option<u64>,
    completed_block: Option<u64>,
    response: Option<(u64, AccessFeedback)>,
}
impl Channel {
    // TIA/EIA-136-122-B §§3.3.1.3–4 bound pending occurrences and completed final-burst repeats separately.
    fn expire(&mut self, block: u64, maximum_pending_gap: u64) {
        let maximum_gap = if self.pending {
            maximum_pending_gap
        } else {
            SUBCHANNELS as u64
        };
        let expired = match self.completed_block {
            Some(completed) => {
                block.saturating_sub(completed) > maximum_pending_gap - SUBCHANNELS as u64
            }
            None => self
                .last_block
                .is_some_and(|previous| block.saturating_sub(previous) > maximum_gap),
        };
        if expired {
            self.receiver.discontinuity();
            self.pending = false;
            self.completed_block = None;
            self.response = None;
        }
    }
}
pub struct AccessChannels {
    channels: [Channel; SUBCHANNELS],
    maximum_pending_gap_blocks: u64,
}
impl Default for AccessChannels {
    fn default() -> Self {
        Self {
            channels: std::array::from_fn(|_| Channel::default()),
            maximum_pending_gap_blocks: MAX_PENDING_GAP_BLOCKS,
        }
    }
}
impl AccessChannels {
    pub(crate) fn retains(&self, identity: crate::link::rach::MobileIdentity) -> bool {
        self.channels
            .iter()
            .any(|channel| channel.receiver.pending_identity() == Some(identity))
    }
    pub fn try_new(max_repetitions: u8) -> Result<Self, Error> {
        if max_repetitions > MAX_REPETITIONS {
            return Err(Error::Limits);
        }
        Ok(Self {
            maximum_pending_gap_blocks: SUBCHANNELS as u64 * (u64::from(max_repetitions) + 1),
            ..Self::default()
        })
    }
    // IS-136.2-A §1.2.1 places the forward boundary 207 symbols after its numbered reverse boundary.
    // IS-136.1-A §4.11.2 and Figure 4-15 associate reverse block n with forward block n+2.
    pub fn receive(
        &mut self,
        relative_symbols: f64,
        fields: &ReverseControlFields,
        next_forward_slot: u64,
    ) -> Result<Outcome, Error> {
        let position = (relative_symbols + REVERSE_OFFSET_SYMBOLS) / SYMBOLS_PER_BLOCK;
        let block = position.round();
        if !position.is_finite() || block < 0.0 {
            return Err(Error::Alignment);
        }
        let error_symbols = (position - block) * SYMBOLS_PER_BLOCK;
        let block = block as u64;
        let feedback_margin_slots = block
            .saturating_add(RESPONSE_BLOCK_OFFSET)
            .saturating_mul(3) as i64
            - next_forward_slot as i64;
        tracing::debug!(
            block,
            error_symbols,
            tolerance_symbols = ALIGNMENT_TOLERANCE_SYMBOLS,
            feedback_margin_slots,
            "Reverse access timing measured"
        );
        if error_symbols.abs() > ALIGNMENT_TOLERANCE_SYMBOLS {
            return Err(Error::Timing {
                relative_symbols,
                nearest_block: block,
                error_symbols,
                tolerance_symbols: ALIGNMENT_TOLERANCE_SYMBOLS,
            });
        }
        let response = block
            .checked_add(RESPONSE_BLOCK_OFFSET)
            .ok_or(Error::Alignment)?;
        if response.checked_mul(3).ok_or(Error::Alignment)? < next_forward_slot {
            return Err(Error::LateFeedback);
        }
        let channel = &mut self.channels[block as usize % SUBCHANNELS];
        if let Some(previous_block) = channel.last_block
            && block <= previous_block
        {
            return Err(Error::RepeatedBlock {
                block,
                previous_block,
            });
        }
        channel.expire(block, self.maximum_pending_gap_blocks);
        channel.last_block = Some(block);
        let outcome = match channel
            .receiver
            .push(fields.format, &fields.block.information)
        {
            Ok(outcome) => outcome,
            Err(error) => {
                channel.pending = false;
                channel.completed_block = None;
                return Err(Error::Link(error));
            }
        };
        let identity = match &outcome {
            Outcome::Pending(id) | Outcome::Duplicate(id) => Some(*id),
            Outcome::Complete(set) => Some(set.identity),
            Outcome::ArqStatus(_) => None,
        };
        let partial_echo = identity.map_or_else(
            || match &outcome {
                Outcome::ArqStatus(status) => status.partial_echo,
                _ => 0,
            },
            |id| id.partial_echo(),
        );
        match &outcome {
            Outcome::Pending(_) => {
                channel.pending = true;
                channel.completed_block = None;
            }
            Outcome::Complete(_) => {
                channel.pending = false;
                channel.completed_block = channel.receiver.completed_multiburst().then_some(block);
            }
            Outcome::ArqStatus(_) => channel.pending = false,
            Outcome::Duplicate(_) => {}
        }
        channel.response = Some((
            response,
            AccessFeedback {
                availability: if channel.pending {
                    Availability::Busy
                } else {
                    Availability::Idle
                },
                reception: Reception::Received,
                partial_echo,
            },
        ));
        Ok(outcome)
    }
    pub fn feedback(&mut self, physical_slot: u64) -> SharedFeedback {
        let block = physical_slot / 3;
        let channel = &mut self.channels
            [(block as usize + SUBCHANNELS - RESPONSE_BLOCK_OFFSET as usize) % SUBCHANNELS];
        if let Some(reverse_block) = block.checked_sub(RESPONSE_BLOCK_OFFSET) {
            channel.expire(reverse_block, self.maximum_pending_gap_blocks);
        }
        let missing = AccessFeedback {
            availability: if channel.pending {
                Availability::Busy
            } else {
                Availability::Idle
            },
            reception: Reception::NotReceived,
            partial_echo: 0,
        };
        let value = match channel.response {
            Some((due, value)) if due == block => {
                channel.response = None;
                value
            }
            Some((due, _)) if due < block => {
                channel.response = None;
                missing
            }
            _ => missing,
        };
        value
            .encode()
            .expect("partial echo comes from a seven-bit identity field")
    }
    pub fn discontinuity(&mut self) {
        self.channels = std::array::from_fn(|_| Channel::default());
    }
}

#[cfg(test)]
mod tests;
