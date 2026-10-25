use std::collections::VecDeque;

const WINDOW_BLOCKS: usize = 64;
const PERCENT: f64 = 100.0;

/// Coded bits of one CRC-valid block and how many of them the decoder corrected. With the
/// decoded bits known to be right, the corrected count is the channel bit error count before
/// error correction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BitErrors {
    pub corrected: u32,
    pub checked: u32,
}
impl BitErrors {
    pub fn percent(self) -> Option<f64> {
        (self.checked != 0).then(|| PERCENT * f64::from(self.corrected) / f64::from(self.checked))
    }
}

#[derive(Debug, Clone, Default)]
pub struct BitErrorWindow {
    blocks: VecDeque<BitErrors>,
    total: BitErrors,
}
impl BitErrorWindow {
    pub fn push(&mut self, block: BitErrors) {
        if self.blocks.len() == WINDOW_BLOCKS
            && let Some(oldest) = self.blocks.pop_front()
        {
            self.total.corrected -= oldest.corrected;
            self.total.checked -= oldest.checked;
        }
        self.total.corrected += block.corrected;
        self.total.checked += block.checked;
        self.blocks.push_back(block);
    }
    pub fn total(&self) -> BitErrors {
        self.total
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn window_keeps_only_the_most_recent_blocks() {
        let mut window = BitErrorWindow::default();
        assert_eq!(window.total().percent(), None);
        window.push(BitErrors {
            corrected: 10,
            checked: 100,
        });
        assert_eq!(window.total().percent(), Some(10.0));
        for _ in 0..WINDOW_BLOCKS {
            window.push(BitErrors {
                corrected: 1,
                checked: 100,
            });
        }
        assert_eq!(
            window.total(),
            BitErrors {
                corrected: WINDOW_BLOCKS as u32,
                checked: 100 * WINDOW_BLOCKS as u32,
            }
        );
        assert_eq!(window.total().percent(), Some(1.0));
    }
}
