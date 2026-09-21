//! Production entrypoint from the authenticated native Primary tool to the
//! transport-independent orchestration service.

use crate::machine::MachineError;
use crate::orchestration::{OrchestrationStore, PrimaryCallContext, PrimaryOrchestrationService};
use crate::semantic::ProductionOrchestrationEffects;
use serde_json::Value;
use std::path::Path;

/// Execute one already-authenticated Primary tool call. Authentication and
/// generation fencing belong to the Worker transport; this function only
/// opens the durable semantic authority and applies its replay rules.
pub fn dispatch(
    state_root: &Path,
    context: &PrimaryCallContext,
    payload: &Value,
) -> Result<Value, MachineError> {
    let mut store = OrchestrationStore::open(state_root)?;
    let mut adapter = ProductionOrchestrationEffects::new(state_root);
    PrimaryOrchestrationService {
        store: &mut store,
        adapter: &mut adapter,
    }
    .dispatch_live_bridge(context, payload)
}
