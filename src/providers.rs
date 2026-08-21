use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub process_group_id: u32,
    pub uid: u32,
}

pub trait MonotonicClock: Send + Sync {
    fn now(&self) -> Duration;
}

pub trait BootIdentityProvider: Send + Sync {
    fn boot_session_id(&self) -> Result<String, ProviderError>;
}

pub trait IdentityProvider: Send + Sync {
    fn current_process(&self) -> Result<ProcessIdentity, ProviderError>;
}

pub trait ProcessEnumerator: Send + Sync {
    fn process_group_members(
        &self,
        process_group_id: u32,
    ) -> Result<Vec<ProcessIdentity>, ProviderError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderError(pub String);

impl std::fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ProviderError {}

#[derive(Clone, Copy, Debug)]
pub struct FixedClock(pub Duration);

impl MonotonicClock for FixedClock {
    fn now(&self) -> Duration {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_clock_is_injectable_without_sleeping() {
        let clock: &dyn MonotonicClock = &FixedClock(Duration::from_millis(42));
        assert_eq!(clock.now(), Duration::from_millis(42));
        assert_eq!(clock.now(), Duration::from_millis(42));
    }
}
