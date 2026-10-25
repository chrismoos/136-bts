use super::*;
use crate::{phy::control::INFORMATION_BITS, scheduler::paging::PagingAllocation};
use std::collections::BTreeMap;
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ReplyOn {
    FirstComplete,
    Secondary,
}
type Frame = (u64, u8, [bool; INFORMATION_BITS]);
pub(super) struct Observer {
    allocation: PagingAllocation,
    receivers: [SpachReceiver; 2],
    pending: [Vec<Frame>; 2],
    primary_frames: BTreeMap<(u64, u8), [bool; INFORMATION_BITS]>,
    pub primary_pages: usize,
    pub secondary_pages: usize,
}
impl Observer {
    pub fn new(allocation: PagingAllocation) -> Self {
        Self {
            allocation,
            receivers: std::array::from_fn(|_| SpachReceiver::default()),
            pending: Default::default(),
            primary_frames: BTreeMap::new(),
            primary_pages: 0,
            secondary_pages: 0,
        }
    }
    pub fn observe(
        &mut self,
        physical: u64,
        information: &[bool],
    ) -> Result<Option<crate::link::spach::SingleMessage>, HalError> {
        let information: &[bool; INFORMATION_BITS] = information
            .try_into()
            .map_err(|_| HalError::from("Page information length"))?;
        let superframe = physical / PHYSICAL_SLOTS;
        let phase = ((physical % PHYSICAL_SLOTS) / CONTROL_SLOT_PERIOD) as u8;
        let parity = (superframe % 2) as usize;
        if parity == 0 {
            if self
                .primary_frames
                .insert((superframe, phase), *information)
                .is_some()
            {
                return Err("duplicate Page primary physical slot".into());
            }
        } else if self.primary_frames.get(&(superframe - 1, phase)) != Some(information) {
            return Err("Page secondary copy differs from its primary slot".into());
        }
        self.pending[parity].push((superframe, phase, *information));
        let Some(message) = self.receivers[parity].push(information).map_err(hal)? else {
            return Ok(None);
        };
        if message.channel != crate::link::spach::Channel::Pch
            || message.identity != MobileIdentity::Min34(2)
            || message.message.first() != Some(&crate::l3::page::MESSAGE_TYPE)
        {
            return Err("unexpected Page channel, identity or message type".into());
        }
        let frames = std::mem::take(&mut self.pending[parity]);
        if frames.len() < 2
            || frames[0].1 != self.allocation.phase(message.identity).map_err(hal)?
        {
            return Err("Page did not begin at its assigned phase as a fragmented message".into());
        }
        for pair in frames.windows(2) {
            let next = self
                .allocation
                .continuation(pair[0].0 - parity as u64, pair[0].1)
                .map(|(sf, phase)| (sf + parity as u64, phase));
            if next != Some((pair[1].0, pair[1].1)) {
                return Err(
                    "Page continuation does not follow every-other-primary SPACH spacing".into(),
                );
            }
        }
        if parity == 0 {
            self.primary_pages += 1;
        } else {
            self.secondary_pages += 1;
        }
        Ok(Some(message))
    }
}
