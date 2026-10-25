use super::*;
use std::sync::Arc;
#[derive(Debug, Clone)]
pub struct PageReceipt {
    handle: CallHandle,
    token: Arc<()>,
    frame: usize,
}
impl PageReceipt {
    pub(super) fn matches(&self, handle: &CallHandle) -> bool {
        handle.matches(&self.handle)
    }
}
pub(super) struct Transmission {
    pub bytes: Vec<u8>,
    frames: Vec<[bool; INFORMATION_BITS]>,
    token: Arc<()>,
    receipts: Vec<bool>,
    next_frame: usize,
    due: Option<(u64, u8)>,
    last_superframe: Option<u64>,
    reported: bool,
}
impl Transmission {
    pub fn new(
        identity: crate::link::rach::MobileIdentity,
        bytes: Vec<u8>,
    ) -> Result<Self, crate::link::spach::SpachError> {
        let frames = SingleMessage {
            channel: Channel::Pch,
            flags: HeaderFlags::default(),
            identity,
            message: bytes.clone(),
        }
        .encode_fragmented()?;
        Ok(Self {
            receipts: vec![false; frames.len()],
            frames,
            bytes,
            token: Arc::new(()),
            next_frame: 0,
            due: None,
            last_superframe: None,
            reported: false,
        })
    }
    fn restart(&mut self) {
        self.token = Arc::new(());
        self.receipts.fill(false);
        self.next_frame = 0;
        self.due = None;
        self.reported = false;
    }
    pub fn in_progress(&self) -> bool {
        self.due.is_some()
    }
    pub fn select(
        &mut self,
        superframe: u64,
        phase: u8,
        assigned_phase: u8,
    ) -> Option<&[bool; INFORMATION_BITS]> {
        if let Some(due) = self.due {
            if (superframe, phase) > due {
                self.restart();
                tracing::warn!(
                    "Missed Page continuation. Restarting the full message at a paging opportunity"
                );
            } else {
                return ((superframe, phase) == due).then(|| &self.frames[self.next_frame]);
            }
        }
        if !superframe.is_multiple_of(2)
            || phase != assigned_phase
            || self.last_superframe == Some(superframe)
        {
            return None;
        }
        if self.next_frame == self.frames.len() {
            if !self.reported {
                return None;
            }
            self.restart();
        }
        Some(&self.frames[0])
    }
    pub fn frame_count(&self, allocation: PagingAllocation, superframe: u64, phase: u8) -> usize {
        allocation
            .continuation_span(superframe, phase, self.remaining_frames())
            .unwrap_or(usize::MAX)
    }
    pub fn remaining_frames(&self) -> usize {
        self.frames.len() - self.next_frame
    }
    pub fn receipt(&self, handle: &CallHandle) -> PageReceipt {
        PageReceipt {
            handle: handle.clone(),
            token: self.token.clone(),
            frame: self.next_frame,
        }
    }
    pub fn submitted(
        &mut self,
        allocation: PagingAllocation,
        superframe: u64,
        phase: u8,
    ) -> Result<Repeat, &'static str> {
        let repeat_superframe = superframe
            .checked_add(1)
            .ok_or("paging repeat superframe range")?;
        let due = if self.next_frame + 1 < self.frames.len() {
            Some(
                allocation
                    .continuation(superframe, phase)
                    .ok_or("paging continuation range")?,
            )
        } else {
            None
        };
        let information = self.frames[self.next_frame];
        self.next_frame += 1;
        self.due = due;
        self.last_superframe = Some(superframe);
        Ok(Repeat {
            superframe: repeat_superframe,
            phase,
            information,
        })
    }
    pub fn transmitted(&mut self, receipt: &PageReceipt, handle: &CallHandle) -> bool {
        if !handle.matches(&receipt.handle)
            || !Arc::ptr_eq(&self.token, &receipt.token)
            || receipt.frame >= self.next_frame
            || self.reported
        {
            return false;
        }
        self.receipts[receipt.frame] = true;
        if self.receipts.iter().all(|receipt| *receipt) {
            self.reported = true;
            return true;
        }
        false
    }
}
pub(super) fn message(number: &str) -> Result<Vec<u8>, crate::l3::page::Error> {
    use crate::l3::{
        access_call::Presentation,
        address::{AddressInfo, AddressValue},
        page::Page,
    };
    const UNKNOWN_NUMBER: u8 = 0;
    const TELEPHONY_PLAN: u8 = 1;
    const PRESENTATION_ALLOWED: u8 = 0;
    const NUMBER_UNAVAILABLE: u8 = 2;
    const NETWORK_SCREENING: u8 = 3;
    let page = Page {
        presentation: Some(Presentation {
            presentation: PRESENTATION_ALLOWED,
            screening: NETWORK_SCREENING,
        }),
        calling_number: Some(AddressInfo {
            number_type: UNKNOWN_NUMBER,
            numbering_plan: TELEPHONY_PLAN,
            address: AddressValue::Tbcd(number.into()),
        }),
        ..Default::default()
    };
    match page.encode() {
        Ok(bytes) => Ok(bytes),
        Err(crate::l3::page::Error::Length | crate::l3::page::Error::Field("address")) => Page {
            presentation: Some(Presentation {
                presentation: NUMBER_UNAVAILABLE,
                screening: NETWORK_SCREENING,
            }),
            ..Default::default()
        }
        .encode(),
        Err(error) => Err(error),
    }
}
