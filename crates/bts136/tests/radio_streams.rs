use bts136::{
    config::Config,
    radio::{
        PreparedRadio,
        receiver::{self, Continuity, ReadOutcome},
        timed_tx,
    },
};
use num_complex::Complex32;
#[test]
fn noop_factory_handoff_preserves_applied_rates_and_supports_owned_activation_reads_and_shutdown() {
    let config = Config::from_yaml(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/two-phones.yaml"
    )))
    .unwrap();
    let prepared = PreparedRadio::open(&config).unwrap();
    let settings = prepared.settings();
    let mut streams = prepared
        .into_streams(
            timed_tx::Limits {
                max_queued_blocks: 4,
                max_block_samples: 1296,
            },
            receiver::Limits {
                max_block_samples: 64,
                timeout_us: 1000,
            },
        )
        .unwrap();
    assert_eq!(streams.settings, settings);
    let observed = streams.tx.prepared_observation().unwrap();
    assert_eq!(observed.state, timed_tx::State::Prepared);
    assert!(streams.tx.stop_acknowledged());
    let epoch = observed.hardware_tick + settings.tx_tick_rate_hz / 10;
    streams
        .tx
        .start(epoch, epoch + settings.tx_tick_rate_hz / 2)
        .unwrap();
    streams.rx.start(None).unwrap();
    let mut buffer = [Complex32::new(f32::NAN, f32::NAN); 64];
    let ReadOutcome::Block(first) = streams.rx.read(&mut buffer).unwrap() else {
        panic!("expected first noop block")
    };
    assert_eq!(first.samples_read, 64);
    assert_eq!(first.first_sample, 0);
    assert_eq!(first.continuity, Continuity::First);
    assert!(
        buffer
            .iter()
            .all(|sample| *sample == Complex32::new(0.0, 0.0))
    );
    let ReadOutcome::Block(second) = streams.rx.read(&mut buffer).unwrap() else {
        panic!("expected second noop block")
    };
    assert_eq!(second.first_sample, 64);
    assert_eq!(second.continuity, Continuity::Continuous);
    assert_eq!(second.segment, 0);
    streams.tx.stop().unwrap();
    streams.rx.stop().unwrap();
    assert!(streams.tx.stop_acknowledged());
    assert!(streams.rx.stop_acknowledged());
    assert_eq!(streams.tx.state(), timed_tx::State::Stopped);
    assert_eq!(streams.rx.state(), receiver::State::Stopped);
}

#[derive(Default)]
struct Device {
    tx_active: bool,
    rx_active: bool,
    fail_read: bool,
    fail_write: bool,
    fail_tx_stop: bool,
    fail_rx_stop: bool,
    tick: u64,
    tx_stops: usize,
    rx_stops: usize,
}
use sdr_hal::{RadioRx, RadioTx, RxReadResult, error::Error as HalError};
use std::sync::{Arc, Mutex};
struct Tx(Arc<Mutex<Device>>);
struct Rx(Arc<Mutex<Device>>);
impl RadioTx for Tx {
    fn tx_sample_rate_hz(&self) -> f64 {
        194_400.0
    }
    fn tick_rate(&self) -> u64 {
        1_000_000
    }
    fn supports_timed_tx(&self) -> bool {
        true
    }
    fn get_hardware_time(&self) -> Result<u64, HalError> {
        Ok(self.0.lock().unwrap().tick)
    }
    fn transmit(&mut self, _: &[Complex32]) -> Result<(), HalError> {
        Err("timed only".into())
    }
    fn transmit_at(&mut self, _: &[Complex32], _: Option<u64>) -> Result<(), HalError> {
        if self.0.lock().unwrap().fail_write {
            Err("write failed".into())
        } else {
            Ok(())
        }
    }
    fn enable_transmit(&mut self, enable: bool) -> Result<(), HalError> {
        let mut device = self.0.lock().unwrap();
        if !enable {
            device.tx_stops += 1;
            if device.fail_tx_stop {
                return Err("TX stop failed".into());
            }
        }
        device.tx_active = enable;
        Ok(())
    }
}
impl RadioRx for Rx {
    fn rx_sample_rate_hz(&self) -> f64 {
        194_400.0
    }
    fn tick_rate(&self) -> u64 {
        1_000_000
    }
    fn get_hardware_time(&self) -> Result<u64, HalError> {
        Ok(self.0.lock().unwrap().tick)
    }
    fn rx_activate(&mut self, _: Option<u64>) -> Result<(), HalError> {
        self.0.lock().unwrap().rx_active = true;
        Ok(())
    }
    fn stop(&mut self) -> Result<(), HalError> {
        let mut device = self.0.lock().unwrap();
        device.rx_stops += 1;
        if device.fail_rx_stop {
            return Err("RX stop failed".into());
        }
        device.rx_active = false;
        Ok(())
    }
    fn rx_read(&mut self, _: &mut [Complex32], _: i64) -> Result<RxReadResult, HalError> {
        if self.0.lock().unwrap().fail_read {
            Err("read failed".into())
        } else {
            Ok(RxReadResult {
                samples_read: 0,
                time_ticks: 0,
                overflow: false,
            })
        }
    }
}
fn duplex(device: Arc<Mutex<Device>>) -> bts136::radio::duplex::Duplex {
    use bts136::radio::{RadioSettings, RadioStreams, duplex::Duplex};
    let settings = RadioSettings {
        applied_tx_sample_rate_hz: 194400,
        applied_rx_sample_rate_hz: 200000,
        tx_tick_rate_hz: 1_000_000,
        rx_tick_rate_hz: 1_000_000,
    };
    let tx = timed_tx::TimedTx::try_new(
        Box::new(Tx(device.clone())),
        194400,
        timed_tx::Limits {
            max_queued_blocks: 2,
            max_block_samples: 1296,
        },
    )
    .unwrap();
    let rx = receiver::Receiver::try_new(
        Box::new(Rx(device)),
        200000,
        receiver::Limits {
            max_block_samples: 64,
            timeout_us: 1000,
        },
    )
    .unwrap();
    Duplex::try_new(RadioStreams { tx, rx, settings }, 8).unwrap()
}
#[test]
fn fatal_receive_or_transmit_failure_stops_both_streams() {
    use bts136::radio::duplex::State;
    for receive_failure in [true, false] {
        let device = Arc::new(Mutex::new(Device::default()));
        let mut radio = duplex(device.clone());
        radio.start(2000, 2, None).unwrap();
        assert!(device.lock().unwrap().tx_active && device.lock().unwrap().rx_active);
        if receive_failure {
            device.lock().unwrap().fail_read = true;
            assert!(radio.read(&mut [Complex32::new(0.0, 0.0); 8]).is_err());
        } else {
            device.lock().unwrap().fail_write = true;
            assert!(
                radio
                    .submit_slot(0, &[false; bts136::phy::traffic::SLOT_BITS])
                    .is_err()
            );
        }
        assert_eq!(radio.state(), State::Failed);
        assert!(radio.stop_acknowledged());
        assert!(!device.lock().unwrap().tx_active && !device.lock().unwrap().rx_active);
    }
}
#[test]
fn cleanup_attempts_both_halves_and_keeps_both_failures() {
    let device = Arc::new(Mutex::new(Device::default()));
    let mut radio = duplex(device.clone());
    radio.start(2000, 2, None).unwrap();
    {
        let mut d = device.lock().unwrap();
        d.fail_tx_stop = true;
        d.fail_rx_stop = true;
        d.fail_read = true;
    }
    let error = radio.read(&mut [Complex32::new(0.0, 0.0); 8]).unwrap_err();
    assert!(error.tx_stop_error.is_some() && error.rx_stop_error.is_some());
    assert!(!radio.stop_acknowledged());
    {
        let mut d = device.lock().unwrap();
        d.fail_tx_stop = false;
        d.fail_rx_stop = false;
    }
    radio.stop().unwrap();
    assert!(radio.stop_acknowledged());
    assert!(!device.lock().unwrap().tx_active && !device.lock().unwrap().rx_active);
}
#[test]
fn local_buffer_errors_keep_streams_running_and_drop_stops_both() {
    use bts136::radio::duplex::State;
    let device = Arc::new(Mutex::new(Device::default()));
    let mut radio = duplex(device.clone());
    radio.start(2000, 2, None).unwrap();
    assert!(radio.read(&mut []).is_err());
    assert!(
        radio
            .submit_slot(1, &[false; bts136::phy::traffic::SLOT_BITS])
            .is_err()
    );
    assert_eq!(radio.state(), State::Running);
    assert!(matches!(
        radio.read(&mut [Complex32::new(0.0, 0.0); 8]).unwrap(),
        ReadOutcome::Timeout { .. }
    ));
    drop(radio);
    assert!(!device.lock().unwrap().tx_active && !device.lock().unwrap().rx_active);
}
#[test]
fn invalid_tx_start_and_normal_deadline_stop_paired_rx() {
    use bts136::radio::duplex::State;
    let device = Arc::new(Mutex::new(Device::default()));
    let mut radio = duplex(device.clone());
    assert!(radio.start(0, 2, None).is_err());
    assert_eq!(radio.state(), State::Failed);
    assert!(!device.lock().unwrap().tx_active && !device.lock().unwrap().rx_active);
    let mut radio = duplex(device.clone());
    radio.start(2000, 1, None).unwrap();
    radio
        .submit_slot(0, &[false; bts136::phy::traffic::SLOT_BITS])
        .unwrap();
    let tail = radio.finish().unwrap();
    device.lock().unwrap().tick = tail.end_tick();
    let mut elapsed = Vec::with_capacity(2);
    radio.poll(&mut elapsed).unwrap();
    assert_eq!(elapsed.len(), 2);
    assert_eq!(radio.state(), State::Stopped);
    assert!(!device.lock().unwrap().tx_active && !device.lock().unwrap().rx_active);
}

#[test]
fn carrier_completion_tokens_wait_for_pulse_tail_and_preserve_backpressure_ownership() {
    use bts136::radio::carrier::{Carrier, Error};
    let device = Arc::new(Mutex::new(Device::default()));
    let mut carrier = Carrier::try_new(duplex(device.clone()), 2).unwrap();
    carrier.start(2000, 3, None).unwrap();
    let bits = [false; bts136::phy::traffic::SLOT_BITS];
    let first = carrier.submit_slot(0, &bits, "page").unwrap();
    let second = carrier.submit_slot(1, &bits, "assignment").unwrap();
    let rejected = carrier.submit_slot(2, &bits, "sms").unwrap_err();
    assert!(matches!(rejected.error, Error::PendingCapacity));
    assert_eq!(rejected.completion, "sms");
    let mut elapsed = Vec::with_capacity(4);
    let mut completed = Vec::with_capacity(2);
    device.lock().unwrap().tick = first.end_tick();
    carrier.poll(&mut elapsed, &mut completed).unwrap();
    assert!(completed.is_empty());
    assert_eq!(carrier.pending_slots(), 2);
    let modulator = bts136::phy::forward::ForwardModulator::try_new(8).unwrap();
    let timeline =
        bts136::radio::tx_timeline::TxTimeline::try_new(2000, 1_000_000, &modulator).unwrap();
    let first_complete_tick = timeline.end_tick_with_tail(1).unwrap();
    assert!(first_complete_tick < second.end_tick());
    device.lock().unwrap().tick = first_complete_tick;
    carrier.poll(&mut elapsed, &mut completed).unwrap();
    assert_eq!(completed, ["page"]);
    carrier.submit_slot(2, &bits, rejected.completion).unwrap();
    assert!(carrier.finish().is_err());
    device.lock().unwrap().tick = second.end_tick();
    carrier.poll(&mut elapsed, &mut completed).unwrap();
    assert!(completed.is_empty());
    let tail = carrier.finish().unwrap();
    device.lock().unwrap().tick = tail.end_tick();
    carrier.poll(&mut elapsed, &mut completed).unwrap();
    assert_eq!(completed, ["assignment", "sms"]);
    assert_eq!(carrier.pending_slots(), 0);
    assert!(!device.lock().unwrap().tx_active && !device.lock().unwrap().rx_active);
}

#[test]
fn carrier_failure_returns_pending_tokens_as_cancellations_without_completion() {
    use bts136::radio::carrier::Carrier;
    let device = Arc::new(Mutex::new(Device::default()));
    let mut carrier = Carrier::try_new(duplex(device.clone()), 2).unwrap();
    carrier.start(2000, 3, None).unwrap();
    let bits = [false; bts136::phy::traffic::SLOT_BITS];
    carrier.submit_slot(0, &bits, "page").unwrap();
    let mut cancelled = Vec::with_capacity(2);
    assert!(carrier.take_cancelled(&mut cancelled).is_err());
    device.lock().unwrap().fail_write = true;
    let rejected = carrier.submit_slot(1, &bits, "assignment").unwrap_err();
    assert_eq!(rejected.completion, "assignment");
    assert_eq!(carrier.pending_slots(), 1);
    assert!(!device.lock().unwrap().tx_active && !device.lock().unwrap().rx_active);
    carrier.take_cancelled(&mut cancelled).unwrap();
    assert_eq!(cancelled, ["page"]);
    carrier.take_cancelled(&mut cancelled).unwrap();
    assert!(cancelled.is_empty());
}

#[test]
fn paging_radio_dispatches_matching_page_only_after_complete_slot_tail() {
    use bts136::{
        core::{
            call_paging::{CallPaging, PagingError, PagingOutcome},
            registry::{LocalRegistry, ProvisionedSubscriber},
        },
        l3::{
            address::{AddressInfo, AddressValue},
            broadcast::{DcchStructure, ProtocolVersion, SlotConfiguration},
            designation::AssignedSlots,
            origination::Origination,
            page_response::{PageResponse, ServiceCode},
            registration::{AccessMessage, RegistrationType},
        },
        link::{
            rach::{MessageSet, MobileIdentity},
            spach::paging::{PageBody, PageFrame, PagingFlags},
        },
        phy::{
            channel::Band,
            control::{self, LogicalChannel, SharedFeedback},
            sync::SyncWord,
        },
        resources::TrafficResources,
        runtime::paging::PagingRadio,
        scheduler::paging::PagingAllocation,
    };
    let mut registry = LocalRegistry::try_new(
        (1..=2)
            .map(|id| ProvisionedSubscriber {
                identity: MobileIdentity::Min34(id),
                number: format!("100{id}"),
            })
            .collect(),
    )
    .unwrap();
    let registration = AccessMessage::Registration {
        registration_type: RegistrationType::PowerUp,
        scm: 21,
        protocol_version: ProtocolVersion::Is136RevA,
    }
    .encode()
    .unwrap();
    for id in 1..=2 {
        registry
            .register(
                &MessageSet {
                    identity: MobileIdentity::Min34(id),
                    messages: vec![registration.clone()],
                },
                0,
            )
            .unwrap();
    }
    let mut calls = CallPaging::new(
        TrafficResources::try_new(Band::Cellular800, 1, &[AssignedSlots::Full14]).unwrap(),
    );
    let origination = Origination {
        protocol_version: ProtocolVersion::Is136RevA,
        emergency_call: false,
        called_party: AddressInfo {
            number_type: 0,
            numbering_plan: 1,
            address: AddressValue::Tbcd("1002".into()),
        },
        last_try: false,
        scm: 21,
        service_code: ServiceCode::DigitalSpeech,
    }
    .encode()
    .unwrap();
    let request = calls
        .begin(
            &registry,
            &MessageSet {
                identity: MobileIdentity::Min34(1),
                messages: vec![origination],
            },
            0,
            1000,
        )
        .unwrap();
    let response = MessageSet {
        identity: request.recipient,
        messages: vec![
            PageResponse {
                protocol_version: ProtocolVersion::Is136RevA,
                last_try: false,
                scm: 21,
                service_code: ServiceCode::DigitalSpeech,
            }
            .encode()
            .unwrap(),
        ],
    };
    let structure = DcchStructure {
        fbcch_slots: 3,
        ebcch_slots: 1,
        sbcch_slots: 0,
        reserved_slots: 0,
        hyperframe_counter: 0,
        secondary_superframe: false,
        slot_configuration: SlotConfiguration::OneFullRate,
        dvcc: 1,
        max_supported_pfc: 1,
        pch_displacement: 0,
        pfm_direction: false,
        non_pch_subchannel_code: 0,
    };
    let allocation = PagingAllocation::try_new(&structure).unwrap();
    let phase = allocation.phase(request.recipient).unwrap();
    let page_index = u64::from(phase) * 3;
    let page = PageFrame {
        flags: PagingFlags::default(),
        body: PageBody::TripleMin34([Some(2), None, None]),
    };
    let feedback = SharedFeedback {
        leading: [false; 12],
        trailing: [false; 10],
    };
    let page_bits = control::encode_forward(
        SyncWord::One,
        phase,
        &feedback,
        &page.encode().unwrap(),
        LogicalChannel::Spach,
        1,
    )
    .unwrap();
    let wrong_page = PageFrame {
        flags: PagingFlags::default(),
        body: PageBody::TripleMin34([Some(1), None, None]),
    };
    let wrong_bits = control::encode_forward(
        SyncWord::One,
        phase,
        &feedback,
        &wrong_page.encode().unwrap(),
        LogicalChannel::Spach,
        1,
    )
    .unwrap();
    let device = Arc::new(Mutex::new(Device::default()));
    let mut radio = PagingRadio::try_new(duplex(device.clone()), calls, allocation, 1, 3).unwrap();
    radio.start(2000, page_index + 1, None).unwrap();
    assert!(
        radio
            .submit_slot(page_index, &wrong_bits, Some(&request.handle))
            .is_err()
    );
    assert!(
        radio
            .submit_slot(page_index + 1, &page_bits, Some(&request.handle))
            .is_err()
    );
    let mut elapsed = Vec::with_capacity(2);
    let mut events = Vec::with_capacity(3);
    let mut receipts: Vec<timed_tx::Receipt> = Vec::new();
    for index in 0..=page_index {
        if index >= 2 {
            device.lock().unwrap().tick = receipts[index as usize - 2].end_tick();
            radio.poll(&mut elapsed, index, &mut events).unwrap();
            assert!(events.is_empty());
        }
        let receipt = if index == page_index {
            radio
                .submit_slot(index, &page_bits, Some(&request.handle))
                .unwrap()
        } else {
            radio
                .submit_slot(index, &[false; bts136::phy::traffic::SLOT_BITS], None)
                .unwrap()
        };
        receipts.push(receipt);
    }
    assert!(matches!(
        radio.calls_mut().response(&request.handle, &response, 100),
        Err(PagingError::NotTransmitted)
    ));
    device.lock().unwrap().tick = receipts[page_index as usize - 1].end_tick();
    radio.poll(&mut elapsed, 101, &mut events).unwrap();
    let tail = radio.finish().unwrap();
    device.lock().unwrap().tick = receipts[page_index as usize].end_tick();
    radio.poll(&mut elapsed, 102, &mut events).unwrap();
    assert!(events.is_empty());
    assert!(matches!(
        radio.calls_mut().response(&request.handle, &response, 103),
        Err(PagingError::NotTransmitted)
    ));
    device.lock().unwrap().tick = tail.end_tick();
    radio.poll(&mut elapsed, 104, &mut events).unwrap();
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0].result, Ok(None)));
    assert!(matches!(
        radio.calls_mut().response(&request.handle, &response, 105),
        Ok(PagingOutcome::Ready { .. })
    ));
    assert!(!device.lock().unwrap().tx_active && !device.lock().unwrap().rx_active);
}

#[test]
fn registration_reply_queue_preserves_rejected_work_and_expires_unsent_responses() {
    use bts136::{
        core::registry::RegistrationReply,
        l3::{designation::AssignedSlots, registration::RegistrationResponse},
        link::{
            broadcast::BroadcastChanges,
            feedback::{AccessFeedback, Availability, Reception},
            rach::MobileIdentity,
        },
        runtime::{
            registration::{Error, ReplyQueue},
            serving,
        },
        scheduler::broadcast::{BroadcastSuperframe, PHYSICAL_SLOTS},
    };
    let config = bts136::config::Config::from_yaml(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/two-phones.yaml"
    )))
    .unwrap();
    let snapshot = BroadcastSuperframe::try_new(
        AssignedSlots::Full14,
        &serving::messages(&config),
        BroadcastChanges {
            fbcch: false,
            ebcch: false,
        },
    )
    .unwrap();
    let device = Arc::new(Mutex::new(Device::default()));
    let mut carrier: bts136::radio::carrier::Carrier<Option<RegistrationReply>> =
        bts136::radio::carrier::Carrier::try_new(duplex(device), 1).unwrap();
    let reply = RegistrationReply {
        identity: MobileIdentity::Min34(1),
        message: RegistrationResponse::Accept,
        options: Default::default(),
    };
    let feedback = AccessFeedback {
        availability: Availability::Idle,
        reception: Reception::NotReceived,
        partial_echo: 0,
    }
    .encode()
    .unwrap();
    let mut queue = ReplyQueue::try_new(1).unwrap();
    queue.enqueue(reply.clone(), 100).unwrap();
    assert!(matches!(queue.enqueue(reply, 100), Err(Error::QueueFull)));
    assert!(
        queue
            .submit_next(&mut carrier, &snapshot, 0, &feedback, 0)
            .unwrap()
            .is_none()
    );
    assert_eq!(queue.pending_replies(), 1);
    let spach = (0..PHYSICAL_SLOTS)
        .find(|slot| {
            snapshot.channel(*slot).unwrap() == Some(bts136::phy::control::LogicalChannel::Spach)
        })
        .unwrap();
    assert!(matches!(
        queue.submit_next(&mut carrier, &snapshot, spach, &feedback, 0),
        Err(Error::Carrier(_))
    ));
    assert_eq!(queue.pending_replies(), 1);
    assert!(
        queue
            .submit_next(&mut carrier, &snapshot, spach, &feedback, 100)
            .unwrap()
            .is_none()
    );
    assert_eq!(queue.pending_replies(), 0);
}
#[test]
fn timestamped_carrier_completion_preserves_scheduled_tail_time_through_delayed_polling() {
    use bts136::radio::carrier::Carrier;
    let device = Arc::new(Mutex::new(Device::default()));
    let mut carrier = Carrier::try_new(duplex(device.clone()), 2).unwrap();
    carrier.start(2000, 2, None).unwrap();
    let bits = [false; bts136::phy::traffic::SLOT_BITS];
    let first = carrier.submit_slot(0, &bits, "first").unwrap();
    carrier.submit_slot_with(1, &bits, || "second").unwrap();
    let timeline = bts136::radio::tx_timeline::TxTimeline::try_new(
        2000,
        1_000_000,
        &bts136::phy::forward::ForwardModulator::try_new(8).unwrap(),
    )
    .unwrap();
    assert_eq!(
        carrier.pending_end_tick(),
        Some(timeline.end_tick_with_tail(2).unwrap())
    );
    let mut elapsed = Vec::with_capacity(4);
    let mut completed = Vec::with_capacity(2);
    device.lock().unwrap().tick = first.end_tick();
    carrier
        .poll_completed(&mut elapsed, &mut completed)
        .unwrap();
    assert!(completed.is_empty());
    let tail = carrier.finish().unwrap();
    device.lock().unwrap().tick = tail.end_tick() + 100_000;
    let observation = carrier
        .poll_completed(&mut elapsed, &mut completed)
        .unwrap();
    assert_eq!(completed.len(), 2);
    for (index, result) in completed.iter().enumerate() {
        assert_eq!(result.slot_index, index as u64);
        assert_eq!(
            result.scheduled_end_tick,
            timeline.end_tick_with_tail(index as u64 + 1).unwrap()
        );
        assert!(result.scheduled_end_tick < observation.hardware_tick);
    }
    assert_eq!(
        completed
            .iter()
            .map(|item| item.completion)
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
    assert_eq!(carrier.pending_end_tick(), None);
    assert!(carrier.stop_acknowledged());
}
#[test]
fn timestamped_and_legacy_carrier_polling_share_ownership_without_duplicate_completions() {
    use bts136::radio::carrier::Carrier;
    let device = Arc::new(Mutex::new(Device::default()));
    let mut carrier = Carrier::try_new(duplex(device.clone()), 2).unwrap();
    carrier.start(2000, 3, None).unwrap();
    let bits = [false; bts136::phy::traffic::SLOT_BITS];
    carrier.submit_slot(0, &bits, "stamped").unwrap();
    let second = carrier.submit_slot(1, &bits, "legacy").unwrap();
    let timeline = bts136::radio::tx_timeline::TxTimeline::try_new(
        2000,
        1_000_000,
        &bts136::phy::forward::ForwardModulator::try_new(8).unwrap(),
    )
    .unwrap();
    let mut elapsed = Vec::with_capacity(4);
    let mut stamped = Vec::with_capacity(2);
    let first_end = timeline.end_tick_with_tail(1).unwrap();
    device.lock().unwrap().tick = first_end - 1;
    carrier.poll_completed(&mut elapsed, &mut stamped).unwrap();
    assert!(stamped.is_empty());
    device.lock().unwrap().tick = first_end;
    carrier.poll_completed(&mut elapsed, &mut stamped).unwrap();
    assert_eq!(stamped.len(), 1);
    assert_eq!(stamped[0].completion, "stamped");
    assert_eq!(stamped[0].scheduled_end_tick, first_end);
    carrier.submit_slot(2, &bits, "last").unwrap();
    device.lock().unwrap().tick = second.end_tick();
    let mut legacy = Vec::with_capacity(2);
    carrier.poll(&mut elapsed, &mut legacy).unwrap();
    assert!(legacy.is_empty());
    let tail = carrier.finish().unwrap();
    device.lock().unwrap().tick = tail.end_tick();
    carrier.poll(&mut elapsed, &mut legacy).unwrap();
    assert_eq!(legacy, ["legacy", "last"]);
    assert_eq!(carrier.pending_end_tick(), None);
}
#[test]
fn timestamped_output_capacity_and_failed_submissions_preserve_pending_owner() {
    use bts136::radio::carrier::{Carrier, Error};
    let device = Arc::new(Mutex::new(Device::default()));
    let mut carrier = Carrier::try_new(duplex(device.clone()), 2).unwrap();
    carrier.start(2000, 3, None).unwrap();
    let bits = [false; bts136::phy::traffic::SLOT_BITS];
    let mut constructed = false;
    assert!(matches!(
        carrier.submit_slot_with(u64::MAX, &bits, || {
            constructed = true;
            "invalid"
        }),
        Err(Error::Timeline(_))
    ));
    assert!(!constructed);
    assert_eq!(carrier.pending_end_tick(), None);
    carrier.submit_slot(0, &bits, "retained").unwrap();
    let expected_end = carrier.pending_end_tick();
    let mut elapsed = Vec::with_capacity(4);
    let mut too_small = Vec::with_capacity(1);
    assert!(matches!(
        carrier.poll_completed(&mut elapsed, &mut too_small),
        Err(Error::OutputCapacity)
    ));
    assert_eq!(carrier.pending_slots(), 1);
    assert_eq!(carrier.pending_end_tick(), expected_end);
    device.lock().unwrap().fail_write = true;
    assert_eq!(
        carrier
            .submit_slot(1, &bits, "rejected")
            .unwrap_err()
            .completion,
        "rejected"
    );
    let mut completed = Vec::with_capacity(2);
    assert!(
        carrier
            .poll_completed(&mut elapsed, &mut completed)
            .is_err()
    );
    assert!(completed.is_empty());
    let mut canceled = Vec::with_capacity(2);
    carrier.take_cancelled(&mut canceled).unwrap();
    assert_eq!(canceled, ["retained"]);
    assert_eq!(carrier.pending_end_tick(), None);
}
#[test]
fn carrier_queue_fence_waits_for_captured_tail_without_waiting_for_later_unrelated_work() {
    use bts136::radio::carrier::{Carrier, Error};
    let device = Arc::new(Mutex::new(Device::default()));
    let mut carrier = Carrier::try_new(duplex(device.clone()), 2).unwrap();
    carrier.start(2000, 4, None).unwrap();
    let bits = [false; bts136::phy::traffic::SLOT_BITS];
    assert!(carrier.fence_complete(&carrier.queue_fence()).unwrap());
    let first = carrier.submit_slot(0, &bits, "old source").unwrap();
    let fence = carrier.queue_fence();
    carrier.submit_slot(1, &bits, "other source").unwrap();
    let mut elapsed = Vec::with_capacity(4);
    let mut completed = Vec::with_capacity(2);
    device.lock().unwrap().tick = first.end_tick();
    carrier
        .poll_completed(&mut elapsed, &mut completed)
        .unwrap();
    assert!(!carrier.fence_complete(&fence).unwrap());
    let bound = fence.scheduled_end_tick().unwrap();
    device.lock().unwrap().tick = bound - 1;
    carrier
        .poll_completed(&mut elapsed, &mut completed)
        .unwrap();
    assert!(!carrier.fence_complete(&fence).unwrap());
    device.lock().unwrap().tick = bound;
    carrier
        .poll_completed(&mut elapsed, &mut completed)
        .unwrap();
    assert!(carrier.fence_complete(&fence).unwrap());
    assert_eq!(carrier.pending_slots(), 1);
    carrier.submit_slot(2, &bits, "later work").unwrap();
    assert!(carrier.fence_complete(&fence).unwrap());
    let other_device = Arc::new(Mutex::new(Device::default()));
    let other: Carrier<&str> = Carrier::try_new(duplex(other_device), 2).unwrap();
    assert!(matches!(
        other.fence_complete(&fence),
        Err(Error::ForeignFence)
    ));
}
#[test]
fn stopped_queue_fence_requires_verified_stop_and_rejects_unacknowledged_failure() {
    use bts136::radio::carrier::{Carrier, Error};
    let device = Arc::new(Mutex::new(Device::default()));
    let mut carrier = Carrier::try_new(duplex(device.clone()), 2).unwrap();
    carrier.start(2000, 3, None).unwrap();
    let bits = [false; bts136::phy::traffic::SLOT_BITS];
    carrier.submit_slot(0, &bits, "pending").unwrap();
    let fence = carrier.queue_fence();
    assert!(!carrier.fence_complete(&fence).unwrap());
    device.lock().unwrap().fail_tx_stop = true;
    assert!(carrier.stop().is_err());
    assert!(!carrier.stop_acknowledged());
    assert!(matches!(carrier.fence_complete(&fence), Err(Error::State)));
    device.lock().unwrap().fail_tx_stop = false;
    carrier.stop().unwrap();
    assert!(carrier.stop_acknowledged());
    assert!(carrier.fence_complete(&fence).unwrap());
}
#[test]
fn carrier_fence_cannot_infer_pulse_tail_submission_from_elapsed_clock_alone() {
    use bts136::radio::carrier::Carrier;
    let device = Arc::new(Mutex::new(Device::default()));
    let mut carrier = Carrier::try_new(duplex(device.clone()), 2).unwrap();
    carrier.start(2000, 3, None).unwrap();
    let bits = [false; bts136::phy::traffic::SLOT_BITS];
    carrier.submit_slot(0, &bits, "incomplete tail").unwrap();
    let fence = carrier.queue_fence();
    device.lock().unwrap().tick = fence.scheduled_end_tick().unwrap() + 1000;
    let mut elapsed = Vec::with_capacity(4);
    let mut completed = Vec::with_capacity(2);
    carrier
        .poll_completed(&mut elapsed, &mut completed)
        .unwrap();
    assert!(completed.is_empty());
    assert!(!carrier.fence_complete(&fence).unwrap());
    carrier.stop().unwrap();
    assert!(carrier.fence_complete(&fence).unwrap());
}
#[test]
fn auxiliary_packet_owner_survives_the_radio_queue_until_its_cancellation_fence() {
    use bts136::{
        core::{
            delivery::{AuxiliaryState, AuxiliaryTransport, DeliveryPolicy, SmsDelivery},
            messaging::LocalMessaging,
            registry::ProvisionedSubscriber,
            store::StateStore,
        },
        l3::relay::{HigherLayerProtocol, RelayMessage, addressing::AddressedRelay},
        link::rach::MobileIdentity,
        radio::carrier::Carrier,
    };
    let peer = MobileIdentity::Min34(1);
    let mut store = StateStore::open(":memory:").unwrap();
    store
        .reconcile(&[ProvisionedSubscriber {
            identity: peer,
            number: "1001".into(),
        }])
        .unwrap();
    let mut delivery = SmsDelivery::after_restart(
        LocalMessaging::from_store(store).unwrap(),
        DeliveryPolicy {
            response_timeout_ms: 50,
            reuse_guard_ms: 100,
        },
        0,
    )
    .unwrap();
    let packet = delivery
        .prepare_auxiliary(
            peer,
            AddressedRelay {
                message: RelayMessage::Data {
                    transaction_id: 0,
                    protocol: HigherLayerProtocol::Segmented(bts136::l3::relay::Teleservice::Sms),
                    payload: vec![0x18, 0, 1, 0],
                },
                addresses: Default::default(),
            },
            AuxiliaryTransport::Control,
            100,
        )
        .unwrap();
    let device = Arc::new(Mutex::new(Device::default()));
    let mut carrier = Carrier::try_new(duplex(device.clone()), 2).unwrap();
    carrier.start(200_000, 3, None).unwrap();
    let bits = [false; bts136::phy::traffic::SLOT_BITS];
    carrier.submit_slot(0, &bits, Some(packet.clone())).unwrap();
    delivery.auxiliary_submitted(&packet, 100, 100).unwrap();
    let fence = carrier.queue_fence();
    carrier.submit_slot(1, &bits, None).unwrap();
    let mut elapsed = Vec::with_capacity(4);
    let mut completed = Vec::with_capacity(2);
    device.lock().unwrap().tick = fence.scheduled_end_tick().unwrap() - 1;
    carrier
        .poll_completed(&mut elapsed, &mut completed)
        .unwrap();
    assert!(!carrier.fence_complete(&fence).unwrap());
    assert_eq!(
        delivery.auxiliary_state(&packet).unwrap(),
        AuxiliaryState::Submitted
    );
    device.lock().unwrap().tick += 1;
    carrier
        .poll_completed(&mut elapsed, &mut completed)
        .unwrap();
    assert!(carrier.fence_complete(&fence).unwrap());
    assert_eq!(completed.len(), 1);
    let retained = completed.pop().unwrap().completion.unwrap();
    assert_eq!(retained.transaction_id(), packet.transaction_id());
    delivery.cancel_auxiliary_fenced(&retained, 207).unwrap();
    assert_eq!(
        delivery.auxiliary_state(&packet).unwrap(),
        AuxiliaryState::Canceled
    );
    assert_eq!(carrier.pending_slots(), 1);
}
#[test]
fn interrupted_completion_construction_stops_untracked_samples_or_leaves_fence_unproven() {
    use bts136::radio::carrier::{Carrier, Error};
    for failed_stop in [false, true] {
        let device = Arc::new(Mutex::new(Device::default()));
        let mut carrier: Carrier<&str> = Carrier::try_new(duplex(device.clone()), 2).unwrap();
        carrier.start(2000, 3, None).unwrap();
        device.lock().unwrap().fail_tx_stop = failed_stop;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = carrier.submit_slot_with(0, &[false; bts136::phy::traffic::SLOT_BITS], || {
                panic!("completion construction failed")
            });
        }));
        assert!(result.is_err());
        assert_eq!(carrier.pending_slots(), 0);
        let fence = carrier.queue_fence();
        if failed_stop {
            assert!(!carrier.stop_acknowledged());
            assert!(matches!(carrier.fence_complete(&fence), Err(Error::State)));
            device.lock().unwrap().fail_tx_stop = false;
            carrier.stop().unwrap();
        }
        assert!(carrier.stop_acknowledged());
        assert!(carrier.fence_complete(&fence).unwrap());
    }
}
