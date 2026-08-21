use crate::machine::MachineError;
use crate::runtime::{RuntimeCapabilities, capabilities};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemanticCommand {
    RuntimeCapabilities,
    Future { dotted_name: String },
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(untagged)]
pub enum SemanticResult {
    RuntimeCapabilities(RuntimeCapabilities),
}

pub trait SemanticService: Send + Sync {
    fn execute(&self, command: &SemanticCommand) -> Result<SemanticResult, MachineError>;
}

#[derive(Default)]
pub struct CoreSemanticService;

impl SemanticService for CoreSemanticService {
    fn execute(&self, command: &SemanticCommand) -> Result<SemanticResult, MachineError> {
        match command {
            SemanticCommand::RuntimeCapabilities => {
                Ok(SemanticResult::RuntimeCapabilities(capabilities()))
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
