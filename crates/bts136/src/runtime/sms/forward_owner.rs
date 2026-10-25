use super::{PreparedDelivery, ReplyPacket};
use crate::core::delivery::AuxiliaryPacket;

#[derive(Debug, Clone)]
pub(super) enum ForwardOwner {
    Reply,
    TrackedReply(ReplyPacket),
    Delivery(PreparedDelivery),
    Auxiliary(AuxiliaryPacket),
}
impl ForwardOwner {
    pub(super) fn tracked_reply(&self) -> Option<&ReplyPacket> {
        match self {
            Self::TrackedReply(packet) => Some(packet),
            _ => None,
        }
    }
    pub(super) fn delivery(&self) -> Option<&PreparedDelivery> {
        match self {
            Self::Reply | Self::TrackedReply(_) | Self::Auxiliary(_) => None,
            Self::Delivery(delivery) => Some(delivery),
        }
    }
    pub(super) fn auxiliary(&self) -> Option<&AuxiliaryPacket> {
        match self {
            Self::Auxiliary(packet) => Some(packet),
            Self::Reply | Self::TrackedReply(_) | Self::Delivery(_) => None,
        }
    }
    pub(super) fn is_reply(&self) -> bool {
        match self {
            Self::Reply | Self::TrackedReply(_) => true,
            Self::Delivery(_) | Self::Auxiliary(_) => false,
        }
    }
}
impl From<Option<PreparedDelivery>> for ForwardOwner {
    fn from(delivery: Option<PreparedDelivery>) -> Self {
        delivery.map_or(Self::Reply, Self::Delivery)
    }
}
