use super::*;
use crate::link::{
    feedback::{ObservedFeedback, Reception},
    spach::{
        SingleMessage,
        arq::{
            self, ArqFrame,
            inactivity::StatusResult,
            procedure::{ArqProcedure, ReceiveEvent, StatusTransmission, TimerEvent},
        },
    },
};
use std::collections::{HashMap, HashSet, VecDeque};
const OBSERVE_SLOTS: usize = 12;
const WINDOW_SLOTS: usize = 24;
const STATUS_LEAD_BLOCKS: u64 = 4;
const FEEDBACK_BLOCK_OFFSET: u64 = 2;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    Normal,
    DropContinue,
    LoseStatus,
    Silent,
    Concurrent,
}
struct Endpoint {
    procedure: ArqProcedure,
    echo: Option<u8>,
    registration_applied: bool,
}
struct PendingStatus {
    identity: MobileIdentity,
    transmission: StatusTransmission,
    feedback_block: u64,
}
pub(super) struct Handsets {
    mode: Mode,
    peers: HashMap<MobileIdentity, Endpoint>,
    pending: VecDeque<PendingStatus>,
    last_block: Option<u64>,
    last_status_block: Option<u64>,
    pub messages: Vec<SingleMessage>,
    pub applications: usize,
    pub echoes: HashSet<u8>,
    pub started_identities: HashSet<MobileIdentity>,
    pub last_data_blocks: HashMap<MobileIdentity, u64>,
    pub statuses: usize,
    pub confirmed: usize,
    pub max_interruption: usize,
    pub first_begin_block: Option<u64>,
    spach_frames: usize,
    last_echo_frame: HashMap<u8, usize>,
    dropped_continue: bool,
    pub dropped_status: bool,
}
fn hal(error: impl std::fmt::Display) -> HalError {
    error.to_string().into()
}
impl Handsets {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            peers: [1, 2]
                .into_iter()
                .map(|value| {
                    let identity = MobileIdentity::Min34(value);
                    (
                        identity,
                        Endpoint {
                            procedure: ArqProcedure::try_new(identity).unwrap(),
                            echo: None,
                            registration_applied: false,
                        },
                    )
                })
                .collect(),
            pending: VecDeque::new(),
            last_block: None,
            last_status_block: None,
            messages: Vec::new(),
            applications: 0,
            echoes: HashSet::new(),
            started_identities: HashSet::new(),
            last_data_blocks: HashMap::new(),
            statuses: 0,
            confirmed: 0,
            max_interruption: 0,
            first_begin_block: None,
            spach_frames: 0,
            last_echo_frame: HashMap::new(),
            dropped_continue: false,
            dropped_status: false,
        }
    }
    fn schedule_status(
        &mut self,
        state: &mut Backend,
        identity: MobileIdentity,
        transmission: StatusTransmission,
    ) -> Result<(), HalError> {
        let block = (state.slot_count as u64).div_ceil(CONTROL_SLOT_PERIOD) + STATUS_LEAD_BLOCKS;
        let block = self
            .last_status_block
            .map_or(block, |previous| block.max(previous + 1));
        let mut block = block;
        while state.packets.iter().any(|(existing, _)| *existing == block) {
            block += 1;
        }
        self.last_status_block = Some(block);
        let drop =
            self.mode == Mode::Silent || self.mode == Mode::LoseStatus && !self.dropped_status;
        if drop {
            self.dropped_status = true;
        }
        if !drop {
            let bits = rach::encode_arq_status(BurstFormat::Normal, &transmission.request.status)
                .map_err(hal)?;
            state.packets.push((block, burst_packet(&bits)));
            self.statuses += 1;
        }
        self.pending.push_back(PendingStatus {
            identity,
            transmission,
            feedback_block: block + FEEDBACK_BLOCK_OFFSET,
        });
        Ok(())
    }
    pub fn observe(&mut self, state: &mut Backend) -> Result<(), HalError> {
        if !state.slot_count.is_multiple_of(OBSERVE_SLOTS) {
            return Ok(());
        }
        let offset = state
            .tx
            .len()
            .saturating_sub(WINDOW_SLOTS * SYMBOLS_PER_SLOT * SAMPLES_PER_SYMBOL);
        let samples: Vec<_> = state.tx[offset..]
            .iter()
            .map(|value| Symbol {
                i: value.re as f64,
                q: value.im as f64,
            })
            .collect();
        let mut bursts = Vec::new();
        let channels: &[LogicalChannel] = if self.pending.is_empty()
            && !self.peers.values().any(|peer| peer.procedure.active())
        {
            &[LogicalChannel::Spach]
        } else {
            &[
                LogicalChannel::Spach,
                LogicalChannel::Fbcch,
                LogicalChannel::Ebcch,
            ]
        };
        for &channel in channels {
            bursts.extend(
                decode_forward_control(&samples, RATE as f64, channel, 1)
                    .map_err(hal)?
                    .into_iter()
                    .map(|burst| (channel, burst)),
            );
        }
        bursts.sort_by(|first, second| {
            first
                .1
                .slot_start_sample
                .total_cmp(&second.1.slot_start_sample)
        });
        for (channel, burst) in bursts {
            let first =
                offset as f64 + burst.slot_start_sample - (FILTER_SPAN * SAMPLES_PER_SYMBOL) as f64;
            if first < 0.0 {
                continue;
            }
            let physical = (first / (SYMBOLS_PER_SLOT * SAMPLES_PER_SYMBOL) as f64).round() as u64;
            let block = physical / CONTROL_SLOT_PERIOD;
            if self.last_block.is_some_and(|last| block <= last) {
                continue;
            }
            self.last_block = Some(block);
            if channel == LogicalChannel::Spach {
                self.spach_frames += 1;
            }
            let feedback = ObservedFeedback::from(&burst.fields.feedback);
            let mut index = 0;
            while index < self.pending.len() {
                if self.pending[index].feedback_block <= block {
                    let pending = self.pending.remove(index).unwrap();
                    let received = pending.feedback_block == block
                        && feedback.reception() == Reception::Received
                        && feedback
                            .matches_partial_echo(pending.transmission.request.status.partial_echo)
                            .map_err(hal)?;
                    if received {
                        self.confirmed += 1;
                    }
                    self.peers
                        .get_mut(&pending.identity)
                        .unwrap()
                        .procedure
                        .status_sent(
                            block,
                            pending.transmission,
                            if received {
                                StatusResult::Sent
                            } else {
                                StatusResult::Failed
                            },
                        )
                        .map_err(hal)?;
                } else {
                    index += 1;
                }
            }
            if channel == LogicalChannel::Spach
                && let Ok(frame) = arq::decode(&burst.fields.block.information)
            {
                let echo = match &frame {
                    ArqFrame::Begin { header, .. } | ArqFrame::Continue { header, .. } => {
                        header.partial_echo
                    }
                };
                if let Some(previous) = self.last_echo_frame.insert(echo, self.spach_frames) {
                    self.max_interruption =
                        self.max_interruption.max(self.spach_frames - previous - 1);
                }
                let identity = match &frame {
                    ArqFrame::Begin {
                        identity, header, ..
                    } => {
                        if let Some(peer) = self.peers.get_mut(identity) {
                            self.first_begin_block.get_or_insert(block);
                            self.started_identities.insert(*identity);
                            peer.echo = Some(header.partial_echo);
                            self.echoes.insert(header.partial_echo);
                            Some(*identity)
                        } else {
                            None
                        }
                    }
                    ArqFrame::Continue { header, .. } => {
                        self.peers.iter().find_map(|(identity, peer)| {
                            (peer.echo == Some(header.partial_echo) && peer.procedure.active())
                                .then_some(*identity)
                        })
                    }
                };
                let drop = self.mode == Mode::DropContinue
                    && !self.dropped_continue
                    && matches!(
                        frame,
                        ArqFrame::Continue {
                            frame_number: 0,
                            ..
                        }
                    );
                if drop {
                    self.dropped_continue = true;
                }
                if let Some(identity) = identity.filter(|_| !drop) {
                    self.last_data_blocks.insert(identity, block);
                    let event = self
                        .peers
                        .get_mut(&identity)
                        .unwrap()
                        .procedure
                        .receive(block, &burst.fields.block.information)
                        .map_err(hal)?;
                    if let ReceiveEvent::Accepted { message, status } = event {
                        if let Some(message) = message {
                            let peer = self.peers.get_mut(&identity).unwrap();
                            if !peer.registration_applied {
                                self.applications += 1;
                                peer.registration_applied = true;
                            }
                            self.messages.push(message);
                        }
                        if let Some(status) = status {
                            self.schedule_status(state, identity, status)?;
                        }
                    }
                }
            }
            let mut statuses = Vec::new();
            for (identity, peer) in &mut self.peers {
                if let TimerEvent::SendStatus(status) =
                    peer.procedure.advance(block).map_err(hal)?
                {
                    statuses.push((*identity, status));
                }
            }
            for (identity, status) in statuses {
                self.schedule_status(state, identity, status)?;
            }
        }
        Ok(())
    }
}

#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn registration_arq_recovers_erased_continue_with_selective_status() {
    registration_arq_loop(
        None,
        false,
        ProtocolVersion::Pv5,
        Some(extended_registration_report()),
        Mode::DropContinue,
    );
}
#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn registration_arq_restarts_after_lost_status_without_reapplying_registration() {
    registration_arq_loop(
        None,
        false,
        ProtocolVersion::Pv5,
        Some(extended_registration_report()),
        Mode::LoseStatus,
    );
}
#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn registration_arq_expires_without_counting_an_unconfirmed_reply() {
    registration_arq_loop(
        None,
        false,
        ProtocolVersion::Pv5,
        Some(extended_registration_report()),
        Mode::Silent,
    );
}

#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn concurrent_arq_registrations_keep_status_maps_and_completion_owners_separate() {
    registration_arq_loop(
        None,
        false,
        ProtocolVersion::Pv5,
        Some(extended_registration_report()),
        Mode::Concurrent,
    );
}

#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn unencodable_reply_does_not_destroy_a_queued_registration_or_allocate_arq() {
    use crate::l3::{
        address::{AddressInfo, AddressValue},
        registration::AcceptOptions,
    };
    let identity = MobileIdentity::Min34(1);
    let reply = crate::core::registry::RegistrationReply {
        identity,
        message: RegistrationResponse::Accept,
        options: Default::default(),
    };
    let mut replies = ReplyQueue::try_new(1).unwrap();
    let mut arq = crate::runtime::spach_arq::Sender::try_new(16).unwrap();
    let mut backlog = Backlog::default();
    queue_registration_reply(&mut replies, &mut arq, &mut backlog, reply.clone(), 0).unwrap();
    let invalid = crate::core::registry::RegistrationReply {
        options: AcceptOptions {
            directory_address: Some(AddressInfo {
                number_type: 0,
                numbering_plan: 1,
                address: AddressValue::Tbcd("1".repeat(600)),
            }),
            ..Default::default()
        },
        ..reply
    };
    assert!(queue_registration_reply(&mut replies, &mut arq, &mut backlog, invalid, 20).is_err());
    assert_eq!(replies.pending_replies(), 1);
    assert_eq!(arq.pending(), 0);
}
