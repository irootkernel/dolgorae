use crate::machine::MachineError;
use crate::runtime::{RuntimeCapabilities, capabilities};
use crate::workspace::{WorkspaceMode, WorkspaceService, WorkspaceView};
use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemanticCommand {
    RuntimeCapabilities,
    Initialize {
        path: Option<PathBuf>,
        mode: WorkspaceMode,
    },
    WorkspaceInspect {
        workspace: Option<PathBuf>,
    },
    RunStartPreflight {
        workspace: Option<PathBuf>,
    },
    Future {
        dotted_name: String,
    },
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(untagged)]
pub enum SemanticResult {
    RuntimeCapabilities(Box<RuntimeCapabilities>),
    Workspace(WorkspaceView),
}

pub trait SemanticService: Send + Sync {
    fn execute(&self, command: &SemanticCommand) -> Result<SemanticResult, MachineError>;
}

#[derive(Default)]
pub struct CoreSemanticService;

impl SemanticService for CoreSemanticService {
    fn execute(&self, command: &SemanticCommand) -> Result<SemanticResult, MachineError> {
        match command {
            SemanticCommand::RuntimeCapabilities => Ok(SemanticResult::RuntimeCapabilities(
                Box::new(capabilities()),
            )),
            SemanticCommand::Initialize { path, mode } => WorkspaceService::system()?
                .initialize(path.as_deref(), *mode)
                .map(SemanticResult::Workspace),
            SemanticCommand::WorkspaceInspect { workspace } => WorkspaceService::system()?
                .discover(workspace.as_deref())
                .map(SemanticResult::Workspace),
            SemanticCommand::RunStartPreflight { workspace } => {
                WorkspaceService::system()?.discover_for_run_start(workspace.as_deref())?;
                Err(MachineError::invalid_argument(
                    "command",
                    "run.start is owned by a later roadmap task",
                ))
            }
            SemanticCommand::Future { dotted_name } => Err(MachineError::invalid_argument(
                "command",
                format!("{dotted_name} is owned by a later roadmap task"),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_flow_through_adapter_independent_service() {
        let result = CoreSemanticService
            .execute(&SemanticCommand::RuntimeCapabilities)
            .unwrap();
        let value = serde_json::to_value(result).unwrap();
        assert_eq!(value["features"]["persistent_runs"], false);
    }
}
