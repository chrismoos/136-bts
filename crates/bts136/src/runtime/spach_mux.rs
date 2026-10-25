// TIA/EIA-136-122-B §6 limits a full-rate interruption to 32 logically consecutive SPACH frames.
const MAX_INTERRUPTION: u64 = 32;
#[derive(Default)]
pub(crate) struct Arbiter {
    next_frame: u64,
    current_frame: u64,
    peers: Vec<Peer>,
    prefer_arq: bool,
}
struct Peer {
    echo: u8,
    last_frame: u64,
}
impl Arbiter {
    pub(crate) fn opportunity(&mut self) -> Result<(), &'static str> {
        self.current_frame = self.next_frame;
        self.next_frame = self
            .next_frame
            .checked_add(1)
            .ok_or("SPACH frame clock range")?;
        Ok(())
    }
    pub(crate) fn retain(&mut self, live: impl Fn(u8) -> bool) {
        self.peers.retain(|peer| live(peer.echo));
    }
    pub(crate) fn contains(&self, echo: u8) -> bool {
        self.peers.iter().any(|peer| peer.echo == echo)
    }
    pub(crate) fn prefers_arq(&self) -> bool {
        self.prefer_arq
    }
    pub(crate) fn other_transmitted(&mut self) {
        self.prefer_arq = true;
    }
    pub(crate) fn transmitted(&mut self, echo: u8) {
        self.prefer_arq = false;
        self.peers.retain(|peer| peer.echo != echo);
        self.peers.push(Peer {
            echo,
            last_frame: self.current_frame,
        });
    }
    pub(crate) fn oldest(&self) -> Option<u8> {
        self.peers.first().map(|peer| peer.echo)
    }
    // Peers stay ordered by last transmission so rank reserves a refresh slot for every older peer.
    pub(crate) fn can_interrupt(&self, frames: usize, repeats: usize) -> bool {
        self.peers.iter().enumerate().all(|(rank, peer)| {
            self.current_frame
                .saturating_sub(peer.last_frame)
                .saturating_add(frames as u64)
                .saturating_add(repeats as u64)
                .saturating_add(rank as u64)
                .saturating_sub(1)
                <= MAX_INTERRUPTION
        })
    }
    pub(crate) fn can_begin(&self, repeats: usize) -> bool {
        self.peers.len().saturating_add(repeats) as u64 <= MAX_INTERRUPTION
            && self.can_interrupt(1, repeats)
    }
    pub(crate) fn overdue(&self) -> Vec<u8> {
        self.peers
            .iter()
            .filter(|peer| {
                self.current_frame
                    .saturating_sub(peer.last_frame)
                    .saturating_sub(1)
                    > MAX_INTERRUPTION
            })
            .map(|peer| peer.echo)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn atomic_admission_reserves_a_refresh_for_every_peer_and_all_required_copies() {
        let mut mux = Arbiter::default();
        for echo in 0..16 {
            mux.opportunity().unwrap();
            mux.transmitted(echo);
        }
        mux.opportunity().unwrap();
        assert!(mux.can_interrupt(17, 0));
        assert!(!mux.can_interrupt(18, 0));
        assert!(mux.can_interrupt(16, 1));
        assert!(!mux.can_interrupt(17, 1));
        assert!(!mux.can_begin(18));
    }
    #[test]
    fn exact_interruption_boundary_and_missed_opportunities_require_a_new_begin() {
        let mut mux = Arbiter::default();
        mux.opportunity().unwrap();
        mux.transmitted(7);
        for _ in 0..32 {
            mux.opportunity().unwrap();
        }
        assert!(mux.can_interrupt(1, 0));
        mux.opportunity().unwrap();
        assert!(!mux.can_interrupt(1, 0));
        assert!(mux.overdue().is_empty());
        mux.opportunity().unwrap();
        assert_eq!(mux.overdue(), vec![7]);
        mux.retain(|_| false);
        assert!(mux.overdue().is_empty());
        assert!(mux.can_interrupt(27, 0));
    }
    #[test]
    fn sustained_atomic_control_and_paging_copies_preserve_all_sixteen_arq_peers() {
        use std::collections::BTreeSet;
        for peers in 1..=16 {
            let mut mux = Arbiter::default();
            for echo in 0..peers {
                mux.opportunity().unwrap();
                mux.transmitted(echo);
            }
            let mut copies = BTreeSet::new();
            let mut remaining = 0;
            let mut controls = 0;
            let mut repeats = 0;
            let mut refreshes = 0;
            for step in 0..4096 {
                mux.opportunity().unwrap();
                assert!(mux.overdue().is_empty(), "peers={peers} step={step}");
                if copies.remove(&step) {
                    repeats += 1;
                    mux.other_transmitted();
                    continue;
                }
                if remaining > 0 {
                    remaining -= 1;
                    controls += 1;
                    mux.other_transmitted();
                    continue;
                }
                let length = step % 4 + 1;
                let page = step % 7 == 0;
                let frames = if page { 1 } else { length };
                if !mux.prefers_arq() && mux.can_interrupt(frames, copies.len() + usize::from(page))
                {
                    if page {
                        assert!(copies.insert(step + 24));
                    }
                    remaining = frames - 1;
                    controls += 1;
                    mux.other_transmitted();
                } else {
                    let echo = mux.oldest().unwrap();
                    mux.transmitted(echo);
                    refreshes += 1;
                }
            }
            assert!(controls > 100 && repeats > 10 && refreshes > 100);
        }
    }
}
