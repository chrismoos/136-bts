use crate::link::rach::MobileIdentity;
use std::{error::Error, fmt, sync::Arc};

const IDENTIFIER_COUNT: usize = u8::MAX as usize + 1;
#[derive(Debug)]
enum IdentifierState {
    Available,
    Reserved(Arc<()>),
    Guarded { until_monotonic_ms: u64 },
}
#[derive(Debug, Clone)]
pub struct RelayIdentifier {
    recipient: MobileIdentity,
    value: u8,
    reservation: Arc<()>,
}
impl RelayIdentifier {
    pub fn value(&self) -> u8 {
        self.value
    }
    pub fn recipient(&self) -> MobileIdentity {
        self.recipient
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentifierError {
    GuardDuration,
    Exhausted,
    StaleReservation,
    ClockRegression,
    TimeOverflow,
}
impl fmt::Display for IdentifierError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "relay identifier allocation failed: {self:?}")
    }
}
impl Error for IdentifierError {}
#[derive(Debug)]
pub struct RelayIdentifiers {
    recipient: MobileIdentity,
    reuse_guard_ms: u64,
    last_monotonic_ms: u64,
    next: usize,
    identifiers: [IdentifierState; IDENTIFIER_COUNT],
}
impl RelayIdentifiers {
    /// Immediate allocation requires a fresh radio session with no possible earlier responses.
    /// Keep one bank per recipient for the radio session, including guarded identifiers.
    /// The reuse guard must cover the caller's maximum late-response lifetime.
    pub fn try_new(
        recipient: MobileIdentity,
        reuse_guard_ms: u64,
    ) -> Result<Self, IdentifierError> {
        if reuse_guard_ms == 0 {
            return Err(IdentifierError::GuardDuration);
        }
        Ok(Self {
            recipient,
            reuse_guard_ms,
            last_monotonic_ms: 0,
            next: 0,
            identifiers: std::array::from_fn(|_| IdentifierState::Available),
        })
    }
    pub fn after_restart(
        recipient: MobileIdentity,
        reuse_guard_ms: u64,
        monotonic_ms: u64,
    ) -> Result<Self, IdentifierError> {
        let mut bank = Self::try_new(recipient, reuse_guard_ms)?;
        let until_monotonic_ms = monotonic_ms
            .checked_add(reuse_guard_ms)
            .ok_or(IdentifierError::TimeOverflow)?;
        bank.last_monotonic_ms = monotonic_ms;
        bank.identifiers = std::array::from_fn(|_| IdentifierState::Guarded { until_monotonic_ms });
        Ok(bank)
    }
    pub fn reserve(&mut self, monotonic_ms: u64) -> Result<RelayIdentifier, IdentifierError> {
        self.check_clock(monotonic_ms)?;
        self.last_monotonic_ms = monotonic_ms;
        for offset in 0..IDENTIFIER_COUNT {
            let index = (self.next + offset) % IDENTIFIER_COUNT;
            let available = match self.identifiers[index] {
                IdentifierState::Available => true,
                IdentifierState::Guarded { until_monotonic_ms } => {
                    monotonic_ms >= until_monotonic_ms
                }
                IdentifierState::Reserved(_) => false,
            };
            if !available {
                continue;
            }
            let reservation = Arc::new(());
            self.identifiers[index] = IdentifierState::Reserved(reservation.clone());
            self.next = (index + 1) % IDENTIFIER_COUNT;
            return Ok(RelayIdentifier {
                recipient: self.recipient,
                value: index as u8,
                reservation,
            });
        }
        Err(IdentifierError::Exhausted)
    }
    pub fn retire(
        &mut self,
        identifier: &RelayIdentifier,
        monotonic_ms: u64,
    ) -> Result<(), IdentifierError> {
        let until_monotonic_ms = self.retirement_deadline(identifier, monotonic_ms)?;
        self.identifiers[usize::from(identifier.value)] =
            IdentifierState::Guarded { until_monotonic_ms };
        self.last_monotonic_ms = monotonic_ms;
        Ok(())
    }
    pub fn recipient(&self) -> MobileIdentity {
        self.recipient
    }
    pub(crate) fn retirement_deadline(
        &self,
        identifier: &RelayIdentifier,
        monotonic_ms: u64,
    ) -> Result<u64, IdentifierError> {
        self.check_clock(monotonic_ms)?;
        if identifier.recipient != self.recipient {
            return Err(IdentifierError::StaleReservation);
        }
        let IdentifierState::Reserved(reservation) =
            &self.identifiers[usize::from(identifier.value)]
        else {
            return Err(IdentifierError::StaleReservation);
        };
        if !Arc::ptr_eq(reservation, &identifier.reservation) {
            return Err(IdentifierError::StaleReservation);
        }
        monotonic_ms
            .checked_add(self.reuse_guard_ms)
            .ok_or(IdentifierError::TimeOverflow)
    }
    fn check_clock(&self, monotonic_ms: u64) -> Result<(), IdentifierError> {
        if monotonic_ms < self.last_monotonic_ms {
            return Err(IdentifierError::ClockRegression);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_wire_ids_are_unique_and_reuse_waits_for_guard_boundary() {
        let mut bank = RelayIdentifiers::try_new(MobileIdentity::Min34(1), 100).unwrap();
        let leases: Vec<_> = (0..IDENTIFIER_COUNT)
            .map(|_| bank.reserve(10).unwrap())
            .collect();
        assert_eq!(
            leases
                .iter()
                .map(RelayIdentifier::value)
                .collect::<Vec<_>>(),
            (0..=u8::MAX).collect::<Vec<_>>()
        );
        assert!(matches!(bank.reserve(10), Err(IdentifierError::Exhausted)));
        bank.retire(&leases[0], 20).unwrap();
        assert!(matches!(bank.reserve(119), Err(IdentifierError::Exhausted)));
        let replacement = bank.reserve(120).unwrap();
        assert_eq!(replacement.value(), leases[0].value());
        assert_eq!(
            bank.retire(&leases[0], 120),
            Err(IdentifierError::StaleReservation)
        );
        bank.retire(&replacement, 120).unwrap();
    }
    #[test]
    fn foreign_bank_foreign_peer_and_repeated_retirement_cannot_release_reservations() {
        let mut bank = RelayIdentifiers::try_new(MobileIdentity::Min34(1), 100).unwrap();
        let mut other = RelayIdentifiers::try_new(MobileIdentity::Min34(1), 100).unwrap();
        let mut peer = RelayIdentifiers::try_new(MobileIdentity::Min34(2), 100).unwrap();
        let own = bank.reserve(0).unwrap();
        let foreign = other.reserve(0).unwrap();
        let wrong_peer = peer.reserve(0).unwrap();
        assert_eq!(
            bank.retire(&foreign, 0),
            Err(IdentifierError::StaleReservation)
        );
        assert_eq!(
            bank.retire(&wrong_peer, 0),
            Err(IdentifierError::StaleReservation)
        );
        bank.retire(&own, 1).unwrap();
        assert_eq!(bank.retire(&own, 1), Err(IdentifierError::StaleReservation));
        other.retire(&foreign, 1).unwrap();
        peer.retire(&wrong_peer, 1).unwrap();
    }
    #[test]
    fn backwards_time_and_overflow_leave_reservation_owned() {
        assert!(matches!(
            RelayIdentifiers::try_new(MobileIdentity::Min34(1), 0),
            Err(IdentifierError::GuardDuration)
        ));
        let mut bank = RelayIdentifiers::try_new(MobileIdentity::Min34(1), 100).unwrap();
        let lease = bank.reserve(20).unwrap();
        assert!(matches!(
            bank.reserve(19),
            Err(IdentifierError::ClockRegression)
        ));
        assert_eq!(
            bank.retire(&lease, 19),
            Err(IdentifierError::ClockRegression)
        );
        assert_eq!(
            bank.retire(&lease, u64::MAX),
            Err(IdentifierError::TimeOverflow)
        );
        bank.retire(&lease, 20).unwrap();
    }
    #[test]
    fn restart_guards_every_identifier_until_full_holdoff_and_rejects_old_leases() {
        let recipient = MobileIdentity::Min34(1);
        let mut previous = RelayIdentifiers::try_new(recipient, 100).unwrap();
        let old_lease = previous.reserve(0).unwrap();
        let mut restarted = RelayIdentifiers::after_restart(recipient, 100, 500).unwrap();
        assert!(matches!(
            restarted.reserve(499),
            Err(IdentifierError::ClockRegression)
        ));
        assert!(matches!(
            restarted.reserve(500),
            Err(IdentifierError::Exhausted)
        ));
        assert!(matches!(
            restarted.reserve(599),
            Err(IdentifierError::Exhausted)
        ));
        let first = restarted.reserve(600).unwrap();
        assert_eq!(first.value(), old_lease.value());
        assert_eq!(
            restarted.retire(&old_lease, 600),
            Err(IdentifierError::StaleReservation)
        );
        let mut values = vec![first.value()];
        for _ in 1..IDENTIFIER_COUNT {
            values.push(restarted.reserve(600).unwrap().value());
        }
        assert_eq!(values, (0..=u8::MAX).collect::<Vec<_>>());
        assert!(matches!(
            restarted.reserve(600),
            Err(IdentifierError::Exhausted)
        ));
        restarted.retire(&first, 600).unwrap();
        assert!(matches!(
            restarted.reserve(699),
            Err(IdentifierError::Exhausted)
        ));
        assert_eq!(restarted.reserve(700).unwrap().value(), first.value());
    }
    #[test]
    fn restart_rejects_zero_guard_and_overflow_without_allocating_identifiers() {
        let recipient = MobileIdentity::Min34(1);
        assert!(matches!(
            RelayIdentifiers::after_restart(recipient, 0, 10),
            Err(IdentifierError::GuardDuration)
        ));
        assert!(matches!(
            RelayIdentifiers::after_restart(recipient, 100, u64::MAX),
            Err(IdentifierError::TimeOverflow)
        ));
        let mut bank = RelayIdentifiers::after_restart(recipient, 100, u64::MAX - 100).unwrap();
        assert!(matches!(
            bank.reserve(u64::MAX - 1),
            Err(IdentifierError::Exhausted)
        ));
        let lease = bank.reserve(u64::MAX).unwrap();
        assert_eq!(
            bank.retire(&lease, u64::MAX),
            Err(IdentifierError::TimeOverflow)
        );
    }

    #[test]
    fn dropped_lease_does_not_allow_unsafe_implicit_reuse() {
        let mut bank = RelayIdentifiers::try_new(MobileIdentity::Min34(1), 100).unwrap();
        let lease = bank.reserve(0).unwrap();
        let copy = lease.clone();
        drop(lease);
        for _ in 1..IDENTIFIER_COUNT {
            bank.reserve(0).unwrap();
        }
        assert!(matches!(bank.reserve(0), Err(IdentifierError::Exhausted)));
        bank.retire(&copy, 0).unwrap();
        assert_eq!(bank.reserve(100).unwrap().value(), copy.value());
    }
}
