//! Production entrypoint from the authenticated native Primary tool to the
//! transport-independent orchestration service.

use crate::machine::MachineError;
use crate::orchestration::{
    AdapterFailure, AssignSpecialistTask, BrokerCredential, BrokeredMemberSnapshot,
    BrokeredRunPlan, CompletedTask, OrchestrationAdapter, OrchestrationStore, PrimaryCallContext,
    PrimaryOrchestrationService, RequestSpecialist, SpecialistTaskSnapshot,
};
use serde_json::Value;
use std::path::Path;
use uuid::Uuid;

/// Execute one already-authenticated Primary tool call.  Authentication and
/// generation fencing belong to the Worker transport; this function only
/// opens the durable semantic authority and applies its replay rules.
pub fn dispatch(
    state_root: &Path,
    context: &PrimaryCallContext,
    payload: &Value,
) -> Result<Value, MachineError> {
    let mut store = OrchestrationStore::open(state_root)?;
    let mut adapter = UnavailableEffects;
    PrimaryOrchestrationService {
        store: &mut store,
        adapter: &mut adapter,
    }
    .dispatch_live_bridge(context, payload)
}

struct UnavailableEffects;

fn unavailable() -> Result<(), AdapterFailure> {
    Err(AdapterFailure::Rejected(
        "ORCHESTRATION_OPERATION_UNAVAILABLE".to_owned(),
    ))
}

impl OrchestrationAdapter for UnavailableEffects {
    fn publish_approval_request(
        &mut self,
        _session_id: Uuid,
        _operation_id: Uuid,
        _approval_request_id: Uuid,
        _request: &RequestSpecialist,
    ) -> Result<(), AdapterFailure> {
        unavailable()
    }

    fn publish_specialist(
        &mut self,
        _plan: &BrokeredRunPlan,
        _credential: &BrokerCredential,
    ) -> Result<(), AdapterFailure> {
        unavailable()
    }

    fn create_thread(
        &mut self,
        _plan: &BrokeredRunPlan,
        _credential: &BrokerCredential,
    ) -> Result<(), AdapterFailure> {
        unavailable()
    }

    fn dispatch_task(
        &mut self,
        _member: &BrokeredMemberSnapshot,
        _request: &AssignSpecialistTask,
        _credential: &BrokerCredential,
    ) -> Result<CompletedTask, AdapterFailure> {
        Err(AdapterFailure::Rejected(
            "ORCHESTRATION_OPERATION_UNAVAILABLE".to_owned(),
        ))
    }

    fn cancel_task(
        &mut self,
        _task: &SpecialistTaskSnapshot,
        _credential: &BrokerCredential,
    ) -> Result<(), AdapterFailure> {
        unavailable()
    }

    fn release_specialist(
        &mut self,
        _member: &BrokeredMemberSnapshot,
        _credential: &BrokerCredential,
    ) -> Result<(), AdapterFailure> {
        unavailable()
    }

    fn release_writer(&mut self, _run_id: Uuid) -> Result<(), AdapterFailure> {
        unavailable()
    }

    fn verify_writer_none(&mut self) -> Result<(), AdapterFailure> {
        unavailable()
    }

    fn acquire_writer(&mut self, _run_id: Uuid) -> Result<(), AdapterFailure> {
        unavailable()
    }
}
