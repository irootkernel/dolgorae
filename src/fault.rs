#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FaultBarrier {
    BeforeManifestCreate,
    AfterManifestFileSync,
    AfterManifestDirectorySync,
    BeforeLedgerAppend,
    AfterLedgerAppend,
    BeforeLedgerFileSync,
    AfterLedgerFileSync,
    BeforeTailEvidenceFileSync,
    AfterTailEvidenceFileSync,
    BeforeTailEvidenceDirectorySync,
    AfterTailEvidenceDirectorySync,
    BeforeLedgerTruncate,
    AfterLedgerTruncate,
    BeforeProjectionReplace,
    BeforeProjectionFileSync,
    AfterProjectionFileSync,
    BeforeProjectionDirectorySync,
    AfterProjectionDirectorySync,
    BeforeExternalEffect,
    AfterExternalEffect,
}

pub const ALL_FAULT_BARRIERS: [FaultBarrier; 20] = [
    FaultBarrier::BeforeManifestCreate,
    FaultBarrier::AfterManifestFileSync,
    FaultBarrier::AfterManifestDirectorySync,
    FaultBarrier::BeforeLedgerAppend,
    FaultBarrier::AfterLedgerAppend,
    FaultBarrier::BeforeLedgerFileSync,
    FaultBarrier::AfterLedgerFileSync,
    FaultBarrier::BeforeTailEvidenceFileSync,
    FaultBarrier::AfterTailEvidenceFileSync,
    FaultBarrier::BeforeTailEvidenceDirectorySync,
    FaultBarrier::AfterTailEvidenceDirectorySync,
    FaultBarrier::BeforeLedgerTruncate,
    FaultBarrier::AfterLedgerTruncate,
    FaultBarrier::BeforeProjectionReplace,
    FaultBarrier::BeforeProjectionFileSync,
    FaultBarrier::AfterProjectionFileSync,
    FaultBarrier::BeforeProjectionDirectorySync,
    FaultBarrier::AfterProjectionDirectorySync,
    FaultBarrier::BeforeExternalEffect,
    FaultBarrier::AfterExternalEffect,
];

pub fn exercise_fault_barriers(injector: &dyn FaultInjector) -> Result<(), FaultInjected> {
    for barrier in ALL_FAULT_BARRIERS {
        injector.check(barrier)?;
    }
    Ok(())
}

pub trait FaultInjector: Send + Sync {
    fn check(&self, barrier: FaultBarrier) -> Result<(), FaultInjected>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FaultInjected(pub FaultBarrier);

impl std::fmt::Display for FaultInjected {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "fault injected at {:?}", self.0)
    }
}

impl std::error::Error for FaultInjected {}

#[derive(Default)]
pub struct NoFaults;

impl FaultInjector for NoFaults {
    fn check(&self, _barrier: FaultBarrier) -> Result<(), FaultInjected> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct RecordingInjector(Mutex<Vec<FaultBarrier>>);

    impl FaultInjector for RecordingInjector {
        fn check(&self, barrier: FaultBarrier) -> Result<(), FaultInjected> {
            self.0.lock().unwrap().push(barrier);
            Ok(())
        }
    }

    #[test]
    fn every_named_barrier_is_addressable_without_sleeping() {
        let injector = RecordingInjector::default();
        exercise_fault_barriers(&injector).unwrap();
        assert_eq!(*injector.0.lock().unwrap(), ALL_FAULT_BARRIERS);
    }
}
