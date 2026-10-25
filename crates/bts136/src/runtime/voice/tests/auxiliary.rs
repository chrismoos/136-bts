use super::*;
use crate::{
    core::{delivery::AuxiliaryState, messaging::LocalMessaging, store::StateStore},
    l3::relay::{
        HigherLayerProtocol, RelayMessage, Teleservice,
        addressing::{AddressedRelay, RelayAddresses},
    },
    radio::{PreparedRadio, carrier::Carrier, duplex::Duplex, receiver, timed_tx},
    runtime::sms::{RelayChannel, SmsRuntime, TrafficPeer},
};
const NOW_MS: u64 = crate::runtime::timers::SMS_ORIGINATION_MS;
const MAX_QUEUED_SLOTS: usize = 256;

#[test]
fn auxiliary_cancellation_waits_for_sacch_encoder_then_the_actual_carrier_fence() {
    let (mut voice, registry) = setup();
    assigned(&mut voice, &registry);
    let config = Config::from_yaml(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/two-phones.yaml"
    )))
    .unwrap();
    let mut store = StateStore::open(":memory:").unwrap();
    store
        .reconcile(
            &registry
                .subscribers()
                .iter()
                .map(|subscriber| subscriber.provisioning.clone())
                .collect::<Vec<_>>(),
        )
        .unwrap();
    let recipient = MobileIdentity::Min34(2);
    let mut core = LocalMessaging::from_store(store).unwrap();
    core.register(
        &set(
            2,
            AccessMessage::Registration {
                registration_type: RegistrationType::PowerUp,
                scm: 15,
                protocol_version: ProtocolVersion::Pv1,
            }
            .encode()
            .unwrap(),
        ),
        0,
    )
    .unwrap();
    let mut sms = SmsRuntime::try_new(core, voice.allocation, "auxiliary-drain".into()).unwrap();
    let call_id = voice.call_id().unwrap();
    let peer = TrafficPeer {
        identity: recipient,
        call_id,
        ready: true,
    };
    sms.sync_traffic(vec![peer], 0).unwrap();
    let packet = sms
        .send_auxiliary(
            recipient,
            AddressedRelay {
                message: RelayMessage::Data {
                    transaction_id: 0,
                    protocol: HigherLayerProtocol::Segmented(Teleservice::Sms),
                    payload: crate::l3::tsar::Response {
                        transaction_id: 0,
                        status: crate::l3::tsar::Status::Received,
                        acknowledgments: crate::l3::tsar::AcknowledgmentMap::try_new(vec![true; 8])
                            .unwrap(),
                        optional_services_support: None,
                    }
                    .encode(ProtocolVersion::Pv1)
                    .unwrap(),
                },
                addresses: RelayAddresses::default(),
            },
            NOW_MS,
        )
        .unwrap();
    let offer = sms.traffic_offer(peer, RelayChannel::Sacch).unwrap();
    let token = offer.token;
    sms.traffic_queued(token).unwrap();
    let leg = &mut voice.active_mut().unwrap().legs[index(Party::Callee)];
    let handle = leg.tx.enqueue_sacch(&offer.message).unwrap();
    leg.sms_sacch = Some((handle, offer));
    let first_interval = leg.next_tx;
    let first_slot = physical(&voice, Party::Callee, first_interval);
    let drain_intervals = (crate::phy::sacch::WORD_SLOTS + SACCH_BITS) as u64;
    let last_slot = physical(&voice, Party::Callee, first_interval + drain_intervals);
    let prepared = PreparedRadio::open(&config).unwrap();
    let settings = prepared.settings();
    let mut streams = prepared
        .into_streams(
            timed_tx::Limits {
                max_queued_blocks: MAX_QUEUED_SLOTS,
                max_block_samples: crate::phy::traffic::SLOT_BITS / 2
                    * crate::config::SAMPLES_PER_SYMBOL,
            },
            receiver::Limits {
                max_block_samples: 64,
                timeout_us: 1000,
            },
        )
        .unwrap();
    let observed = streams.tx.prepared_observation().unwrap();
    let mut carrier = Carrier::try_new(
        Duplex::try_new(streams, crate::config::SAMPLES_PER_SYMBOL).unwrap(),
        MAX_QUEUED_SLOTS,
    )
    .unwrap();
    carrier
        .start_continuous(observed.hardware_tick + settings.tx_tick_rate_hz * 2, None)
        .unwrap();
    for slot in 0..=last_slot {
        if slot >= first_slot && (slot - first_slot).is_multiple_of(FULL_RATE_PERIOD) {
            let interval = first_interval + (slot - first_slot) / FULL_RATE_PERIOD;
            let leg = &mut voice.active_mut().unwrap().legs[index(Party::Callee)];
            let burst = leg.tx.prepare(interval, SyncWord::Three).unwrap().clone();
            carrier
                .submit_slot_with(slot, burst.bits(), || Completion::Traffic {
                    party: Party::Callee,
                    call_id,
                    command: voice
                        .traffic_submitted_at(
                            voice.call_id().unwrap(),
                            Party::Callee,
                            &burst,
                            NOW_MS,
                        )
                        .unwrap(),
                })
                .unwrap();
            if slot == first_slot {
                sms.cancel_auxiliary(&packet, NOW_MS + 1).unwrap();
                voice.exchange_sms(&mut sms, NOW_MS + 1, 0).unwrap();
                assert!(voice.retains_sms(token));
                sms.settle_auxiliary_fences(&carrier, &voice, NOW_MS + 1)
                    .unwrap();
                assert!(sms.take_auxiliary_outcome().is_none());
            }
        } else {
            carrier
                .submit_slot(
                    slot,
                    &[false; crate::phy::traffic::SLOT_BITS],
                    Completion::None,
                )
                .unwrap();
        }
    }
    voice.exchange_sms(&mut sms, NOW_MS + 2, 0).unwrap();
    assert!(!voice.retains_sms(token));
    sms.settle_auxiliary_fences(&carrier, &voice, NOW_MS + 2)
        .unwrap();
    assert_eq!(
        sms.auxiliary_state(&packet).unwrap(),
        AuxiliaryState::Prepared
    );
    assert!(sms.take_auxiliary_outcome().is_none());
    carrier.stop().unwrap();
    assert!(carrier.stop_acknowledged());
    sms.settle_auxiliary_fences(&carrier, &voice, NOW_MS + 3)
        .unwrap();
    assert_eq!(
        sms.take_auxiliary_outcome().unwrap().reply.state,
        AuxiliaryState::Canceled
    );
    voice.shutdown(NOW_MS + 3).unwrap();
}
