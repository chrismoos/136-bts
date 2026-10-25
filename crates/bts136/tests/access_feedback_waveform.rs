use bts136::{
    link::feedback::{AccessFeedback, AccessState, Availability, ObservedFeedback, Reception},
    phy::{
        acquisition::decode_forward_control,
        control::{self, LogicalChannel},
        forward::ForwardModulator,
        sync::SyncWord,
    },
};

#[test]
fn forward_feedback_preserves_access_state_reception_and_expected_echo() {
    let expected = [
        AccessFeedback {
            availability: Availability::Idle,
            reception: Reception::NotReceived,
            partial_echo: 0,
        },
        AccessFeedback {
            availability: Availability::Reserved,
            reception: Reception::Received,
            partial_echo: 85,
        },
        AccessFeedback {
            availability: Availability::Busy,
            reception: Reception::Received,
            partial_echo: 127,
        },
    ];
    let mut transmitter = ForwardModulator::try_new(8).unwrap();
    let mut samples = Vec::new();
    for (phase, feedback) in expected.iter().enumerate() {
        let bits = control::encode_forward(
            SyncWord::One,
            phase as u8,
            &feedback.encode().unwrap(),
            &[false; control::INFORMATION_BITS],
            LogicalChannel::Spach,
            42,
        )
        .unwrap();
        samples.extend(transmitter.encode_slot(&bits));
    }
    samples.extend(transmitter.finish());
    let bursts = decode_forward_control(
        &samples,
        transmitter.sample_rate_hz(),
        LogicalChannel::Spach,
        42,
    )
    .unwrap();
    assert_eq!(bursts.len(), expected.len());
    for (burst, expected) in bursts.iter().zip(expected) {
        let observed = ObservedFeedback::from(&burst.fields.feedback);
        assert_eq!(observed.reception(), expected.reception);
        assert!(
            observed
                .matches_partial_echo(expected.partial_echo)
                .unwrap()
        );
        let state = match expected.availability {
            Availability::Idle => AccessState::StartRandomAccess,
            Availability::Reserved => AccessState::StartReservedAccess,
            Availability::Busy => AccessState::MoreBursts,
        };
        assert_eq!(observed.availability(state), Some(expected.availability));
    }
}
