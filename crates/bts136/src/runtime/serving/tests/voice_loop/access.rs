// TIA-136-123-E §3.6 starts WAFO_TMR on Page Response access success and requires it for DTCD.
use crate::link::feedback::{ObservedFeedback, Reception};
const FEEDBACK_OFFSET_BLOCKS: u64 = 2;
// TIA-136-123-E §7 Table 13 gives WAFO_TMR as 10 superframes of 32 TDMA blocks.
const WAIT_FOR_ORDER_BLOCKS: u64 = 320;
#[derive(Debug, Clone, Copy, Default)]
enum State {
    #[default]
    Idle,
    Awaiting {
        feedback_block: u64,
    },
    Waiting {
        feedback_block: u64,
        deadline_block: u64,
    },
    Assigned {
        feedback_block: u64,
        assignment_block: u64,
    },
    Failed,
}
#[derive(Debug)]
pub(super) enum AccessError {
    NotReady {
        block: u64,
        feedback_block: Option<u64>,
    },
    OutsideWindow {
        block: u64,
        feedback_block: u64,
        deadline_block: u64,
    },
}
impl std::fmt::Display for AccessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotReady {
                block,
                feedback_block,
            } => write!(
                f,
                "callee DTCD before successful Page Response feedback: block={block} expected={feedback_block:?}"
            ),
            Self::OutsideWindow {
                block,
                feedback_block,
                deadline_block,
            } => write!(
                f,
                "callee DTCD outside WAFO interval: block={block} feedback={feedback_block} deadline={deadline_block}"
            ),
        }
    }
}
impl std::error::Error for AccessError {}
#[derive(Debug, Default)]
pub(super) struct PageAccess {
    state: State,
}
impl PageAccess {
    pub(super) fn scheduled(&mut self, final_reverse_block: u64) {
        self.state = State::Awaiting {
            feedback_block: final_reverse_block + FEEDBACK_OFFSET_BLOCKS,
        };
    }
    pub(super) fn awaiting_feedback(&self) -> bool {
        matches!(self.state, State::Awaiting { .. })
    }
    pub(super) fn feedback(
        &mut self,
        block: u64,
        feedback: &ObservedFeedback,
        partial_echo: u8,
    ) -> Result<(), crate::link::feedback::InvalidPartialEcho> {
        let State::Awaiting { feedback_block } = self.state else {
            return Ok(());
        };
        if block < feedback_block {
            return Ok(());
        }
        self.state = if block == feedback_block
            && feedback.reception() == Reception::Received
            && feedback.matches_partial_echo(partial_echo)?
        {
            State::Waiting {
                feedback_block,
                deadline_block: feedback_block + WAIT_FOR_ORDER_BLOCKS,
            }
        } else {
            State::Failed
        };
        Ok(())
    }
    pub(super) fn designate(&mut self, block: u64) -> Result<(), AccessError> {
        let State::Waiting {
            feedback_block,
            deadline_block,
        } = self.state
        else {
            return Err(AccessError::NotReady {
                block,
                feedback_block: match self.state {
                    State::Awaiting { feedback_block } => Some(feedback_block),
                    _ => None,
                },
            });
        };
        if block < feedback_block || block >= deadline_block {
            return Err(AccessError::OutsideWindow {
                block,
                feedback_block,
                deadline_block,
            });
        }
        self.state = State::Assigned {
            feedback_block,
            assignment_block: block,
        };
        Ok(())
    }
    pub(super) fn assignment(&self) -> Option<(u64, u64)> {
        if let State::Assigned {
            feedback_block,
            assignment_block,
        } = self.state
        {
            Some((feedback_block, assignment_block))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::feedback::{AccessFeedback, Availability};
    const ECHO: u8 = 2;
    const FINAL_REVERSE_BLOCK: u64 = 10;
    const FINAL_FEEDBACK_BLOCK: u64 = 12;
    fn observed(echo: u8, reception: Reception) -> ObservedFeedback {
        ObservedFeedback::from(
            &AccessFeedback {
                availability: Availability::Idle,
                reception,
                partial_echo: echo,
            }
            .encode()
            .unwrap(),
        )
    }
    #[test]
    fn only_the_final_matching_scf_enables_same_slot_designation() {
        let mut access = PageAccess::default();
        access.scheduled(FINAL_REVERSE_BLOCK);
        access
            .feedback(
                FINAL_FEEDBACK_BLOCK - 1,
                &observed(ECHO, Reception::Received),
                ECHO,
            )
            .unwrap();
        assert!(access.awaiting_feedback());
        assert!(access.designate(FINAL_FEEDBACK_BLOCK - 1).is_err());
        access
            .feedback(
                FINAL_FEEDBACK_BLOCK,
                &observed(ECHO, Reception::Received),
                ECHO,
            )
            .unwrap();
        access.designate(FINAL_FEEDBACK_BLOCK).unwrap();
        assert_eq!(
            access.assignment(),
            Some((FINAL_FEEDBACK_BLOCK, FINAL_FEEDBACK_BLOCK))
        );
        assert!(access.designate(FINAL_FEEDBACK_BLOCK + 1).is_err());
    }
    #[test]
    fn negative_or_foreign_or_missing_feedback_cannot_be_revived_by_later_success() {
        for (block, feedback) in [
            (FINAL_FEEDBACK_BLOCK, observed(ECHO, Reception::NotReceived)),
            (
                FINAL_FEEDBACK_BLOCK,
                observed(ECHO ^ 0x7f, Reception::Received),
            ),
            (
                FINAL_FEEDBACK_BLOCK + 1,
                observed(ECHO, Reception::Received),
            ),
        ] {
            let mut access = PageAccess::default();
            access.scheduled(FINAL_REVERSE_BLOCK);
            access.feedback(block, &feedback, ECHO).unwrap();
            access
                .feedback(block + 1, &observed(ECHO, Reception::Received), ECHO)
                .unwrap();
            assert!(access.designate(block + 1).is_err());
            assert!(access.assignment().is_none());
        }
        let mut idle = PageAccess::default();
        idle.feedback(
            FINAL_FEEDBACK_BLOCK,
            &observed(ECHO, Reception::Received),
            ECHO,
        )
        .unwrap();
        assert!(idle.designate(FINAL_FEEDBACK_BLOCK).is_err());
    }
    #[test]
    fn designation_requires_the_unexpired_wafo_interval() {
        for (block, accepted) in [
            (FINAL_FEEDBACK_BLOCK + WAIT_FOR_ORDER_BLOCKS - 1, true),
            (FINAL_FEEDBACK_BLOCK + WAIT_FOR_ORDER_BLOCKS, false),
        ] {
            let mut access = PageAccess::default();
            access.scheduled(FINAL_REVERSE_BLOCK);
            access
                .feedback(
                    FINAL_FEEDBACK_BLOCK,
                    &observed(ECHO, Reception::Received),
                    ECHO,
                )
                .unwrap();
            assert_eq!(access.designate(block).is_ok(), accepted);
        }
    }
}
