use super::*;
use crate::runtime::sms::tests::auxiliary::radio;
use crate::{radio::carrier::Carrier, runtime::voice::VoiceRuntime};

struct CompletedTransfer {
    handle: JournalHandle,
    response: MessageSet,
    last_packet: AuxiliaryPacket,
}
fn cancel_unsubmitted(runtime: &mut SmsRuntime) -> JournalHandle {
    queue(runtime);
    runtime.dispatch_outgoing_sms(0).unwrap();
    assert!(runtime.outgoing_sms.jobs.is_empty());
    let handle = runtime.outgoing_sms.streams[0].snapshot.handle.clone();
    runtime
        .apply_outgoing(0, JournalEvent::Cancel, UNIX_MS)
        .unwrap();
    handle
}
fn transmit_reply(
    runtime: &mut SmsRuntime,
    handle: &JournalHandle,
    carrier: &Carrier<AuxiliaryPacket>,
    voice: &VoiceRuntime,
    now: &mut u64,
) -> ReplyPacket {
    let reply = runtime.latest_outgoing_reply(handle).unwrap().clone();
    let phase = runtime.allocation.phase(PEER).unwrap();
    let superframe = *now / crate::runtime::timers::SUPERFRAME_MS;
    let mut handed_off = false;
    for _ in 0..8 {
        if runtime.frame(superframe, phase).is_none() {
            break;
        }
        if let super::super::super::Completion::Reply(packet) = runtime.submitted_at(*now).unwrap()
        {
            assert_eq!(packet, reply);
            handed_off = true;
        }
    }
    assert!(handed_off);
    *now += 1;
    runtime.reply_transmitted(&reply, *now, *now).unwrap();
    runtime.settle_reply_fences(carrier, voice, *now).unwrap();
    assert!(reply.status().unwrap().fenced);
    reply
}
fn complete(
    runtime: &mut SmsRuntime,
    carrier: &Carrier<AuxiliaryPacket>,
    voice: &VoiceRuntime,
    now: &mut u64,
) -> CompletedTransfer {
    runtime.dispatch_outgoing_sms(*now).unwrap();
    assert_eq!(runtime.outgoing_sms.streams.len(), 1);
    let handle = runtime.outgoing_sms.streams[0].snapshot.handle.clone();
    let count = runtime.outgoing_sms.streams[0]
        .snapshot
        .plan
        .segments()
        .len();
    let mut last_packet = None;
    for _ in 0..count {
        let packet = current(runtime);
        receipt(runtime, &packet, *now);
        runtime
            .settle_auxiliary_fences(carrier, voice, *now + 3)
            .unwrap();
        runtime.dispatch_outgoing_sms(*now + 3).unwrap();
        *now += 4;
        last_packet = Some(packet);
    }
    assert_eq!(
        runtime.outgoing_sms.streams[0].sender.state(),
        SenderState::AwaitingResponse
    );
    let response = response(runtime, 222);
    runtime.receive(&response, *now, -1).unwrap();
    *now += 1;
    assert!(matches!(
        runtime.outgoing_sms.streams[0].sender.state(),
        SenderState::Complete(crate::link::tsar::Completion::PeerReceived)
    ));
    CompletedTransfer {
        handle,
        response,
        last_packet: last_packet.unwrap(),
    }
}
#[test]
fn unsubmitted_retirement_waits_for_the_guard_and_receive_drainage() {
    let mut runtime = fresh(None, UNIX_MS);
    let handle = cancel_unsubmitted(&mut runtime);
    let (mut carrier, _) = radio(&runtime);
    let guard = runtime.submission_retry_guard_ms;
    runtime
        .settle_outgoing_retirements(&carrier, 0, |_| false)
        .unwrap();
    runtime
        .settle_outgoing_retirements(&carrier, guard - 1, |_| false)
        .unwrap();
    assert!(
        runtime
            .core_mut()
            .store_mut()
            .outgoing_journal(&handle)
            .unwrap()
            .retirement
            .is_none()
    );
    runtime
        .settle_outgoing_retirements(&carrier, guard, |_| true)
        .unwrap();
    assert_eq!(runtime.outgoing_sms.streams.len(), 1);
    runtime
        .settle_outgoing_retirements(&carrier, guard + 1, |_| false)
        .unwrap();
    assert!(runtime.outgoing_sms.streams.is_empty());
    let journal = runtime
        .core_mut()
        .store_mut()
        .outgoing_journal(&handle)
        .unwrap();
    let retirement = journal.retirement.unwrap();
    assert_eq!(retirement.retired_at_unix_ms, UNIX_MS);
    assert_eq!(retirement.reuse_after_unix_ms, UNIX_MS + guard as i64);
    assert!(!journal.events.is_empty());
    carrier.stop().unwrap();
}
#[test]
fn submitted_cancellation_does_not_invent_peer_transaction_closure() {
    let mut runtime = fresh(None, UNIX_MS);
    queue(&mut runtime);
    runtime.dispatch_outgoing_sms(NOW_MS).unwrap();
    let handle = runtime.outgoing_sms.streams[0].snapshot.handle.clone();
    let unix = runtime.outgoing_time(NOW_MS).unwrap();
    runtime
        .apply_outgoing(0, JournalEvent::Cancel, unix)
        .unwrap();
    runtime.withdraw_outgoing(0, NOW_MS).unwrap();
    let (mut carrier, voice) = radio(&runtime);
    runtime
        .settle_auxiliary_fences(&carrier, &voice, NOW_MS)
        .unwrap();
    let later = NOW_MS + runtime.submission_retry_guard_ms * 2;
    runtime
        .settle_outgoing_retirements(&carrier, later, |_| false)
        .unwrap();
    assert!(
        runtime
            .core_mut()
            .store_mut()
            .outgoing_journal(&handle)
            .unwrap()
            .retirement
            .is_none()
    );
    assert_eq!(runtime.outgoing_sms.streams.len(), 1);
    assert!(
        runtime
            .outgoing_measurements(runtime.outgoing_sms.streams[0].snapshot.sms_id.value())
            .iter()
            .any(|metric| metric.name == "Transaction ID")
    );
    carrier.stop().unwrap();
}
#[test]
fn completed_response_retries_restart_the_quiet_guard_after_the_new_reply_fence() {
    let mut runtime = fresh(None, UNIX_MS);
    queue(&mut runtime);
    let (mut carrier, voice) = radio(&runtime);
    let mut now = NOW_MS;
    let completed = complete(&mut runtime, &carrier, &voice, &mut now);
    runtime
        .settle_outgoing_retirements(&carrier, now, |_| false)
        .unwrap();
    assert!(runtime.outgoing_sms.streams[0].retirement.is_none());
    transmit_reply(&mut runtime, &completed.handle, &carrier, &voice, &mut now);
    let guard = runtime.submission_retry_guard_ms;
    runtime
        .settle_outgoing_retirements(&carrier, now, |_| false)
        .unwrap();
    now += guard - 1;
    runtime.receive(&completed.response, now, -1).unwrap();
    runtime
        .settle_outgoing_retirements(&carrier, now, |_| false)
        .unwrap();
    assert!(runtime.outgoing_sms.streams[0].retirement.is_none());
    transmit_reply(&mut runtime, &completed.handle, &carrier, &voice, &mut now);
    let anchor = now;
    runtime
        .settle_outgoing_retirements(&carrier, now, |_| false)
        .unwrap();
    now += guard - 1;
    runtime
        .settle_outgoing_retirements(&carrier, now, |_| false)
        .unwrap();
    assert_eq!(runtime.outgoing_sms.streams.len(), 1);
    now += 1;
    runtime
        .settle_outgoing_retirements(&carrier, now, |_| false)
        .unwrap();
    assert!(runtime.outgoing_sms.streams.is_empty());
    let journal = runtime
        .core_mut()
        .store_mut()
        .outgoing_journal(&completed.handle)
        .unwrap();
    assert_eq!(
        journal.retirement.unwrap().retired_at_unix_ms,
        UNIX_MS + anchor as i64
    );
    assert!(runtime.latest_outgoing_reply(&completed.handle).is_none());
    carrier.stop().unwrap();
}
#[test]
fn restart_discards_volatile_retirement_proof_and_requires_a_fresh_radio_fence() {
    let database = Database::new();
    let mut runtime = fresh(Some(&database), UNIX_MS);
    let handle = cancel_unsubmitted(&mut runtime);
    let (mut carrier, _) = radio(&runtime);
    let guard = runtime.submission_retry_guard_ms;
    runtime
        .settle_outgoing_retirements(&carrier, 0, |_| false)
        .unwrap();
    carrier.stop().unwrap();
    drop(runtime);
    let restart_unix = UNIX_MS + guard as i64 - 1;
    let mut runtime = fresh(Some(&database), restart_unix);
    let (mut fresh_carrier, _) = radio(&runtime);
    runtime
        .settle_outgoing_retirements(&fresh_carrier, guard, |_| false)
        .unwrap();
    assert!(runtime.outgoing_sms.streams[0].retirement.is_none());
    fresh_carrier.stop().unwrap();
    runtime
        .settle_outgoing_retirements(&fresh_carrier, guard, |_| false)
        .unwrap();
    runtime
        .settle_outgoing_retirements(&fresh_carrier, guard * 2 - 1, |_| false)
        .unwrap();
    assert_eq!(runtime.outgoing_sms.streams.len(), 1);
    runtime
        .settle_outgoing_retirements(
            &fresh_carrier,
            guard * 2 + crate::runtime::timers::SUPERFRAME_MS,
            |_| false,
        )
        .unwrap();
    let journal = runtime
        .core_mut()
        .store_mut()
        .outgoing_journal(&handle)
        .unwrap();
    assert_eq!(
        journal.retirement.unwrap().retired_at_unix_ms,
        restart_unix + guard as i64
    );
}
#[test]
fn failed_retirement_write_retries_without_extending_the_original_guard() {
    let database = Database::new();
    let mut runtime = fresh(Some(&database), UNIX_MS);
    let handle = cancel_unsubmitted(&mut runtime);
    let (mut carrier, _) = radio(&runtime);
    let guard = runtime.submission_retry_guard_ms;
    runtime
        .settle_outgoing_retirements(&carrier, 0, |_| false)
        .unwrap();
    let connection = rusqlite::Connection::open(&database.0).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_retirement BEFORE UPDATE OF retired_at_unix_ms ON tsar_outgoing BEGIN SELECT RAISE(ABORT,'retirement failure'); END;").unwrap();
    assert!(
        runtime
            .settle_outgoing_retirements(&carrier, guard, |_| false)
            .is_err()
    );
    assert!(
        runtime
            .core_mut()
            .store_mut()
            .outgoing_journal(&handle)
            .unwrap()
            .retirement
            .is_none()
    );
    connection
        .execute_batch("DROP TRIGGER fail_retirement;")
        .unwrap();
    runtime
        .settle_outgoing_retirements(&carrier, guard + 1, |_| false)
        .unwrap();
    let journal = runtime
        .core_mut()
        .store_mut()
        .outgoing_journal(&handle)
        .unwrap();
    assert_eq!(journal.retirement.unwrap().retired_at_unix_ms, UNIX_MS);
    carrier.stop().unwrap();
}
#[test]
fn twelve_successful_runtime_transfers_reuse_ids_and_preserve_history_across_restart() {
    const TRANSFERS: usize = 12;
    const RESTART_AFTER: usize = 6;
    let database = Database::new();
    let mut runtime = fresh(Some(&database), UNIX_MS);
    let mut origin = UNIX_MS;
    let mut now = NOW_MS;
    let mut handles = Vec::new();
    let mut wire_ids = Vec::new();
    for index in 0..TRANSFERS {
        let id = queue_with_key(&mut runtime, &format!("retirement-{index}"));
        let (mut carrier, voice) = radio(&runtime);
        let completed = complete(&mut runtime, &carrier, &voice, &mut now);
        assert!(!handles.contains(&completed.handle));
        handles.push(completed.handle.clone());
        let assigned = runtime.outgoing_sms.streams[0].snapshot.plan.segments()[0].transaction_id;
        wire_ids.push(assigned);
        transmit_reply(&mut runtime, &completed.handle, &carrier, &voice, &mut now);
        runtime
            .settle_outgoing_retirements(&carrier, now, |_| false)
            .unwrap();
        now += runtime.submission_retry_guard_ms;
        runtime
            .settle_outgoing_retirements(&carrier, now, |_| false)
            .unwrap();
        assert!(runtime.outgoing_sms.streams.is_empty());
        assert_eq!(
            runtime.core().store().state(id).unwrap(),
            QueueState::Delivered
        );
        assert!(
            runtime
                .operation_messages(TRANSFERS)
                .unwrap()
                .iter()
                .find(|sms| sms.id == id.value())
                .unwrap()
                .measurements
                .iter()
                .any(|metric| metric.name == "Segmentation state"
                    && metric.value.as_deref() == Some("Retired"))
        );
        carrier.stop().unwrap();
        now += 1;
        if index + 1 == RESTART_AFTER {
            origin += now as i64;
            drop(runtime);
            runtime = fresh(Some(&database), origin);
            now = NOW_MS;
        }
    }
    const WIRE_ID_COUNT: usize = 8;
    let unique: std::collections::HashSet<_> = wire_ids.iter().copied().collect();
    assert!(wire_ids.iter().all(|id| usize::from(*id) < WIRE_ID_COUNT));
    assert!(unique.len() < wire_ids.len());
    let journals = runtime
        .core_mut()
        .store_mut()
        .outgoing_journals(0, TRANSFERS as u32 + 1)
        .unwrap();
    assert_eq!(journals.len(), TRANSFERS);
    assert!(journals.iter().all(|journal| journal.retirement.is_some()
        && journal.replay().unwrap().state()
            == SenderState::Complete(crate::link::tsar::Completion::PeerReceived)));
    assert!(
        runtime
            .core_mut()
            .store_mut()
            .active_outgoing_journals(0, TRANSFERS as u32 + 1)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn queued_radio_samples_must_drain_before_the_late_message_guard_starts() {
    let mut runtime = fresh(None, UNIX_MS);
    queue(&mut runtime);
    let (mut first_carrier, voice) = radio(&runtime);
    let mut now = NOW_MS;
    let completed = complete(&mut runtime, &first_carrier, &voice, &mut now);
    transmit_reply(
        &mut runtime,
        &completed.handle,
        &first_carrier,
        &voice,
        &mut now,
    );
    first_carrier.stop().unwrap();
    let (mut carrier, _) = radio(&runtime);
    carrier
        .submit_slot(
            0,
            &[false; crate::phy::traffic::SLOT_BITS],
            completed.last_packet,
        )
        .unwrap();
    let guard = runtime.submission_retry_guard_ms;
    runtime
        .settle_outgoing_retirements(&carrier, now, |_| false)
        .unwrap();
    now += guard * 2;
    runtime
        .settle_outgoing_retirements(&carrier, now, |_| false)
        .unwrap();
    assert!(
        runtime
            .core_mut()
            .store_mut()
            .outgoing_journal(&completed.handle)
            .unwrap()
            .retirement
            .is_none()
    );
    carrier.stop().unwrap();
    runtime
        .settle_outgoing_retirements(&carrier, now, |_| false)
        .unwrap();
    now += guard - 1;
    runtime
        .settle_outgoing_retirements(&carrier, now, |_| false)
        .unwrap();
    assert_eq!(runtime.outgoing_sms.streams.len(), 1);
    now += 1;
    runtime
        .settle_outgoing_retirements(&carrier, now, |_| false)
        .unwrap();
    assert!(runtime.outgoing_sms.streams.is_empty());
}

#[test]
fn stale_private_callbacks_cannot_modify_the_next_owner_of_a_reused_wire_id() {
    let mut runtime = fresh(None, UNIX_MS);
    queue(&mut runtime);
    let (mut carrier, voice) = radio(&runtime);
    let mut now = NOW_MS;
    let completed = complete(&mut runtime, &carrier, &voice, &mut now);
    let old_reply = transmit_reply(&mut runtime, &completed.handle, &carrier, &voice, &mut now);
    let old_wire_id = runtime.outgoing_sms.streams[0].snapshot.plan.segments()[0].transaction_id;
    runtime
        .settle_outgoing_retirements(&carrier, now, |_| false)
        .unwrap();
    now += runtime.submission_retry_guard_ms;
    runtime
        .settle_outgoing_retirements(&carrier, now, |_| false)
        .unwrap();
    const WIRE_ID_COUNT: usize = 8;
    for index in 0..WIRE_ID_COUNT {
        queue_with_key(&mut runtime, &format!("new-generation-{index}"));
    }
    now += 1;
    runtime.dispatch_outgoing_sms(now).unwrap();
    let stream = runtime
        .outgoing_sms
        .streams
        .iter()
        .find(|stream| stream.snapshot.plan.segments()[0].transaction_id == old_wire_id)
        .unwrap();
    assert_ne!(stream.snapshot.handle, completed.handle);
    let before: Vec<_> = runtime
        .outgoing_sms
        .streams
        .iter()
        .map(|stream| {
            (
                stream.snapshot.handle.clone(),
                stream.snapshot.revision(),
                stream.sender.state(),
            )
        })
        .collect();
    runtime
        .outgoing_lower_reply(
            &crate::core::delivery::AuxiliaryReply {
                packet: completed.last_packet,
                state: AuxiliaryState::Accepted,
            },
            now,
        )
        .unwrap();
    runtime.reply_transmitted(&old_reply, now, now).unwrap();
    let after: Vec<_> = runtime
        .outgoing_sms
        .streams
        .iter()
        .map(|stream| {
            (
                stream.snapshot.handle.clone(),
                stream.snapshot.revision(),
                stream.sender.state(),
            )
        })
        .collect();
    assert_eq!(after, before);
    assert!(
        before
            .iter()
            .any(|(_, _, state)| *state == SenderState::AwaitingAccept)
    );
    carrier.stop().unwrap();
}

#[test]
fn unrepresentable_retirement_deadline_keeps_the_transaction_reserved() {
    const NEAR_CLOCK_END: u64 = u64::MAX - crate::runtime::timers::SUPERFRAME_MS;
    let mut runtime = crate::runtime::sms::tests::runtime();
    register(&mut runtime, ProtocolVersion::Pv1);
    runtime.start_incoming_sms(NEAR_CLOCK_END, UNIX_MS).unwrap();
    runtime.start_outgoing_sms(NEAR_CLOCK_END).unwrap();
    queue(&mut runtime);
    let queued = runtime.core().store().pending_for(PEER).unwrap().remove(0);
    runtime
        .admit_outgoing(PEER, queued, Route::Control, ProtocolVersion::Pv1, UNIX_MS)
        .unwrap();
    runtime
        .apply_outgoing(0, JournalEvent::Cancel, UNIX_MS)
        .unwrap();
    let handle = runtime.outgoing_sms.streams[0].snapshot.handle.clone();
    let (mut carrier, _) = radio(&runtime);
    assert!(matches!(
        runtime.settle_outgoing_retirements(&carrier, NEAR_CLOCK_END, |_| false),
        Err(Failure::Clock)
    ));
    assert!(
        runtime
            .core_mut()
            .store_mut()
            .outgoing_journal(&handle)
            .unwrap()
            .retirement
            .is_none()
    );
    carrier.stop().unwrap();
}
