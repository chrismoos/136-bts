use std::collections::VecDeque;

/// One frame of slack absorbs reverse decode jitter.
const PRIMED_FRAMES: usize = 2;
const MAX_FRAMES: usize = 4;

/// A bad frame is queued as `None` so later frames stay in step.
pub(super) struct Playout<T> {
    frames: VecDeque<Option<T>>,
    primed: bool,
    underruns: u64,
    overflows: u64,
}

impl<T> Default for Playout<T> {
    fn default() -> Self {
        Self {
            frames: VecDeque::with_capacity(MAX_FRAMES),
            primed: false,
            underruns: 0,
            overflows: 0,
        }
    }
}

impl<T> Playout<T> {
    pub(super) fn push(&mut self, frame: Option<T>) {
        if self.frames.len() == MAX_FRAMES {
            self.frames.pop_front();
            self.overflows += 1;
        }
        self.frames.push_back(frame);
    }

    pub(super) fn pop(&mut self) -> Option<T> {
        if !self.primed {
            if self.frames.len() < PRIMED_FRAMES {
                return None;
            }
            self.primed = true;
        }
        match self.frames.pop_front() {
            Some(frame) => frame,
            None => {
                self.primed = false;
                self.underruns += 1;
                None
            }
        }
    }

    pub(super) fn clear(&mut self) {
        self.frames.clear();
        self.primed = false;
    }

    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub(super) fn underruns(&self) -> u64 {
        self.underruns
    }

    pub(super) fn overflows(&self) -> u64 {
        self.overflows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jittered_arrivals_play_every_frame_in_order() {
        let mut playout = Playout::default();
        let mut played = Vec::new();
        let mut next = 0;
        // Arrivals alternate between just before and just after each preparation.
        for arrivals in [1, 1, 0, 2, 1, 0, 2, 1, 1, 0, 2, 1] {
            for _ in 0..arrivals {
                playout.push(Some(next));
                next += 1;
            }
            played.extend(playout.pop());
        }
        assert_eq!(played, (0..played.len() as i32).collect::<Vec<_>>());
        assert_eq!(played.len(), next as usize - 1);
        assert_eq!(playout.underruns(), 0);
        assert_eq!(playout.overflows(), 0);
    }

    #[test]
    fn erased_intervals_keep_their_place() {
        let mut playout = Playout::default();
        playout.push(Some(1));
        playout.push(None);
        playout.push(Some(3));
        assert_eq!(playout.pop(), Some(1));
        assert_eq!(playout.pop(), None);
        assert_eq!(playout.pop(), Some(3));
        assert_eq!(playout.underruns(), 0);
    }

    #[test]
    fn underrun_waits_for_the_buffer_to_refill() {
        let mut playout = Playout::default();
        playout.push(Some(1));
        playout.push(Some(2));
        assert_eq!(playout.pop(), Some(1));
        assert_eq!(playout.pop(), Some(2));
        assert_eq!(playout.pop(), None);
        assert_eq!(playout.underruns(), 1);
        playout.push(Some(3));
        assert_eq!(playout.pop(), None);
        playout.push(Some(4));
        assert_eq!(playout.pop(), Some(3));
    }

    #[test]
    fn overflow_drops_the_oldest_frame() {
        let mut playout = Playout::default();
        for frame in 0..=MAX_FRAMES as i32 {
            playout.push(Some(frame));
        }
        assert_eq!(playout.overflows(), 1);
        assert_eq!(playout.pop(), Some(1));
    }
}
