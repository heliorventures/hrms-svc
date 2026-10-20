//! Durable payroll job state is separate from immutable command acceptance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}
impl JobState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
    pub fn can_cancel(self) -> bool {
        self == Self::Queued
    }
    pub fn can_claim(self, expired: bool) -> bool {
        self == Self::Queued || (self == Self::Running && expired)
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acceptance_is_not_business_completion() {
        assert!(!JobState::Queued.is_terminal());
        assert!(!JobState::Running.is_terminal());
        assert!(JobState::Succeeded.is_terminal());
        assert!(JobState::Failed.is_terminal());
    }

    #[test]
    fn cancellation_cannot_race_a_claimed_execution() {
        assert!(JobState::Queued.can_cancel());
        assert!(!JobState::Running.can_cancel());
        assert!(!JobState::Succeeded.can_cancel());
    }

    #[test]
    fn expired_claim_is_recoverable_without_replaying_terminal_work() {
        assert!(JobState::Queued.can_claim(false));
        assert!(!JobState::Running.can_claim(false));
        assert!(JobState::Running.can_claim(true));
        for state in [JobState::Succeeded, JobState::Failed, JobState::Cancelled] {
            assert!(!state.can_claim(true));
        }
    }
}
