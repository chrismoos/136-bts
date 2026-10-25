use super::*;
use crate::{
    l3::message_waiting::{MessageWaiting, Waiting},
    phy::control::INFORMATION_BITS,
};
#[derive(Clone, Copy)]
enum IndicationKind {
    MessageWaiting,
    UserAlert,
}
type Frame = (u64, u8, [bool; INFORMATION_BITS]);
pub(super) struct Observer {
    kind: IndicationKind,
    identities: [MobileIdentity; 2],
    receivers: [SpachReceiver; 2],
    pending: [Vec<Frame>; 2],
    primary: [Vec<Frame>; 2],
}
pub(super) struct Observation {
    pub index: usize,
    pub identity: MobileIdentity,
    pub confirmed_type: u8,
    pub superframe: u64,
    pub physical: u64,
    pub information: [bool; INFORMATION_BITS],
}
pub(super) fn message() -> MessageWaiting {
    MessageWaiting {
        entries: vec![
            Waiting { kind: 0, count: 63 },
            Waiting { kind: 1, count: 0 },
            Waiting { kind: 2, count: 62 },
            Waiting { kind: 4, count: 1 },
        ],
        display: Some("D".repeat(82)),
        ..Default::default()
    }
}
impl Default for Observer {
    fn default() -> Self {
        Self::with_identities([MobileIdentity::Min34(1), MobileIdentity::Min34(2)])
    }
}
impl Observer {
    pub fn for_user_alert() -> Self {
        Self {
            kind: IndicationKind::UserAlert,
            ..Self::default()
        }
    }

    pub fn with_identities(identities: [MobileIdentity; 2]) -> Self {
        Self {
            kind: IndicationKind::MessageWaiting,
            identities,
            receivers: std::array::from_fn(|_| SpachReceiver::default()),
            pending: Default::default(),
            primary: Default::default(),
        }
    }

    pub fn observe(
        &mut self,
        information: &[bool],
        first: f64,
        allocation: PagingAllocation,
    ) -> Result<Option<Observation>, HalError> {
        if !information[..3].iter().all(|bit| *bit) {
            return Ok(None);
        }
        let physical = ((first - (FILTER_SPAN * SAMPLES_PER_SYMBOL) as f64)
            / (SYMBOLS_PER_SLOT * SAMPLES_PER_SYMBOL) as f64)
            .round() as u64;
        let sf = physical / PHYSICAL_SLOTS;
        let phase = ((physical % PHYSICAL_SLOTS) / CONTROL_SLOT_PERIOD) as u8;
        let parity = (sf % 2) as usize;
        let frame: [bool; INFORMATION_BITS] = information
            .try_into()
            .map_err(|_| HalError::from("PCH information length"))?;
        self.pending[parity].push((sf, phase, frame));
        let Some(decoded) = self.receivers[parity]
            .push(information)
            .map_err(|error| HalError::from(error.to_string()))?
        else {
            return Ok(None);
        };
        let (expected, confirmed_type) = match self.kind {
            IndicationKind::MessageWaiting => (
                message().encode().unwrap(),
                crate::l3::message_waiting::MESSAGE_TYPE,
            ),
            IndicationKind::UserAlert => (
                alert().encode().unwrap(),
                crate::l3::user_alert::MESSAGE_TYPE,
            ),
        };
        if decoded.channel != crate::link::spach::Channel::Pch || decoded.message != expected {
            return Err("Unexpected idle indication payload or channel".into());
        }
        let index = self
            .identities
            .iter()
            .position(|identity| *identity == decoded.identity)
            .ok_or_else(|| HalError::from("Unexpected idle indication identity"))?;
        let fragments = std::mem::take(&mut self.pending[parity]);
        if parity == 0 {
            if !self.primary[index].is_empty() {
                return Err("Duplicate primary idle indication".into());
            }
            if fragments.len() < 2
                || fragments[0].1
                    != allocation
                        .phase(decoded.identity)
                        .map_err(|error| HalError::from(error.to_string()))?
            {
                return Err("idle indication did not start at assigned PCH phase".into());
            }
            for pair in fragments.windows(2) {
                if allocation.continuation(pair[0].0, pair[0].1) != Some((pair[1].0, pair[1].1)) {
                    return Err("idle indication continuation spacing is not every other primary SPACH slot".into());
                }
            }
            self.primary[index] = fragments;
        } else {
            let expected: Vec<_> = self.primary[index]
                .iter()
                .map(|(sf, phase, bits)| (sf + 1, *phase, *bits))
                .collect();
            if fragments != expected {
                return Err("idle indication secondary copy changed timing or contents".into());
            }
        }
        Ok(Some(Observation {
            index,
            identity: decoded.identity,
            confirmed_type,
            superframe: sf,
            physical,
            information: frame,
        }))
    }
}

pub(super) fn alert() -> crate::l3::user_alert::UserAlert {
    crate::l3::user_alert::UserAlert {
        signal: Some(crate::l3::user_alert::Signal {
            pitch: crate::l3::user_alert::Pitch::Low,
            cadence: 12,
            duration_code: 0,
        }),
        display: Some("X".repeat(82)),
        ..Default::default()
    }
}
