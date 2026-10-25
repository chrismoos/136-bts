use super::*;
use crate::link::rach::ArqStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryState {
    AwaitingStatus,
    Complete,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusDisposition {
    Ignored,
    Accepted,
}
#[derive(Debug)]
pub struct ArqDelivery {
    partial_echo: u8,
    frames: Vec<ArqFrame>,
    acknowledged: Vec<bool>,
    state: DeliveryState,
}
impl ArqDelivery {
    pub fn try_new(
        header: ArqHeader,
        identity: MobileIdentity,
        message: &[u8],
    ) -> Result<Self, SpachError> {
        let frames = fragment_message(header, identity, message)?;
        Ok(Self {
            partial_echo: header.partial_echo,
            acknowledged: vec![false; frames.len() - 1],
            frames,
            state: DeliveryState::AwaitingStatus,
        })
    }
    pub fn frames(&self) -> &[ArqFrame] {
        &self.frames
    }
    pub fn state(&self) -> DeliveryState {
        self.state
    }

    // IS-136.1-A Table 5-2 and §5.2.5.1. FRNO and payload stay fixed on retransmission.
    pub fn apply_status(&mut self, status: &ArqStatus) -> StatusDisposition {
        if status.partial_echo != self.partial_echo {
            return StatusDisposition::Ignored;
        }
        for (acknowledged, received) in self.acknowledged.iter_mut().zip(status.received_frames) {
            *acknowledged |= received;
        }
        if self.acknowledged.iter().all(|received| *received) {
            self.state = DeliveryState::Complete;
        }
        StatusDisposition::Accepted
    }

    /// Call after a matching status. BEGIN recovery and timeout policy belong to the procedure driver.
    pub fn pending_frames(&self) -> Vec<&ArqFrame> {
        if self.state == DeliveryState::Complete {
            return Vec::new();
        }
        self.frames[1..]
            .iter()
            .zip(&self.acknowledged)
            .filter_map(|(frame, acknowledged)| if *acknowledged { None } else { Some(frame) })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn header() -> ArqHeader {
        ArqHeader {
            channel: Channel::Smsch,
            flags: HeaderFlags::default(),
            partial_echo: 42,
            polling: true,
            response_mode: ResponseMode::Reservation,
        }
    }
    #[test]
    fn statuses_select_only_missing_frames_and_delayed_snapshots_do_not_revoke_acknowledgments() {
        let mut delivery =
            ArqDelivery::try_new(header(), MobileIdentity::Imsi50(1), &[0xff; 40]).unwrap();
        let original = delivery.frames().to_vec();
        let mut status = ArqStatus {
            partial_echo: 43,
            received_frames: [false; ARQ_MAP_BITS],
        };
        status.received_frames[0] = true;
        assert_eq!(delivery.apply_status(&status), StatusDisposition::Ignored);
        assert_eq!(delivery.pending_frames().len(), 4);
        status.partial_echo = 42;
        status.received_frames[2] = true;
        assert_eq!(delivery.apply_status(&status), StatusDisposition::Accepted);
        assert_eq!(delivery.pending_frames(), [&original[2], &original[4]]);
        status.received_frames = [false; ARQ_MAP_BITS];
        delivery.apply_status(&status);
        assert_eq!(delivery.pending_frames(), [&original[2], &original[4]]);
        status.received_frames[1] = true;
        status.received_frames[3] = true;
        delivery.apply_status(&status);
        assert_eq!(delivery.state(), DeliveryState::Complete);
        assert!(delivery.pending_frames().is_empty());
        status.received_frames = [false; ARQ_MAP_BITS];
        delivery.apply_status(&status);
        assert_eq!(delivery.state(), DeliveryState::Complete);
        assert_eq!(delivery.frames(), original);
    }
    #[test]
    fn begin_only_delivery_completes_on_matching_status() {
        let mut delivery =
            ArqDelivery::try_new(header(), MobileIdentity::Tmsi20(1), &[0x1d]).unwrap();
        assert_eq!(delivery.frames().len(), 1);
        assert_eq!(delivery.state(), DeliveryState::AwaitingStatus);
        delivery.apply_status(&ArqStatus {
            partial_echo: 42,
            received_frames: [false; ARQ_MAP_BITS],
        });
        assert_eq!(delivery.state(), DeliveryState::Complete);
    }
}
