//! Private consumer authority and durable dispatch. Native registration is separate.
use crate::machine::MachineError;
use crate::trusted_host::{VerifiedOwner, refusal};
use rusqlite::{OptionalExtension as _, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::PathBuf;
use uuid::Uuid;

pub(crate) const MAX_INPUT: usize = 1_048_576;
const DATABASE: &str = "consumer.sqlite";
const STAGING: &str = "consumer.pending";

pub(crate) const EXTERNAL_NATIVE: &str = "specialist_native";
pub(crate) const EXTERNAL_CONTROL: &str = "specialist_consumer";
pub(crate) const MANAGED_NATIVE: &str = "dolgorae_specialists_native";
pub(crate) const MANAGED_CONTROL: &str = "dolgorae_specialist_consumer";

#[derive(Clone, Debug)]
pub(crate) struct ControlCorrelation {
    pub action: &'static str,
    pub request_id: Option<String>,
}

pub(crate) fn is_control(tool: &str) -> bool {
    matches!(tool, EXTERNAL_CONTROL | MANAGED_CONTROL)
}
pub(crate) fn is_native(tool: &str) -> bool {
    is_control(tool) || matches!(tool, EXTERNAL_NATIVE | MANAGED_NATIVE)
}
pub(crate) fn managed_tool(tool: &str) -> bool {
    matches!(tool, MANAGED_NATIVE | MANAGED_CONTROL)
        || tool == crate::specialist_facade::MANAGED_TOOL
}

pub(crate) fn control_spec(name: &str) -> Value {
    let identifier =
        json!({"type":"string","minLength":1,"maxLength":128,"pattern":"^[A-Za-z0-9._:-]+$"});
    let object = |properties: Value| {
        let required = properties
            .as_object()
            .expect("literal properties")
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        json!({"type":"object","additionalProperties":false,"required":required,"properties":properties})
    };
    let mut views = [
        "complete_result",
        "final_response",
        "structured_output",
        "package",
        "prepared_input",
        "outcome",
        "receipt",
    ]
    .into_iter()
    .map(|kind| object(json!({"kind":{"const":kind}})))
    .collect::<Vec<_>>();
    views.push(object(
        json!({"kind":{"const":"package_evidence"},"index":{"type":"integer","minimum":0}}),
    ));
    views.push(object(json!({"kind":{"const":"change"},"index":{"type":"integer","minimum":0},"side":{"enum":["before","after"]}})));
    let value = |kind: &str, properties: Value| {
        let mut p = properties.as_object().unwrap().clone();
        p.insert("type".into(), json!({"const":kind}));
        object(Value::Object(p))
    };
    let string = json!({"type":"string"});
    let records = vec![
        value("confirm", json!({"value":{"type":"boolean"}})),
        value("text", json!({"value":string})),
        value("choice", json!({"value":string})),
        value(
            "list",
            json!({"value":{"type":"array","items":{"type":"string"}}}),
        ),
        value(
            "integer",
            json!({"value":{"type":"string","pattern":"^-?(0|[1-9][0-9]*)$"}}),
        ),
        value("artifact", json!({"artifact_ref":identifier})),
        value(
            "check_result",
            json!({"operation_id":identifier,"operation_digest":string,
            "input_basis":object(json!({"descriptor":string,"digest":string})),
            "executor":object(json!({"name":string,"version":string})),"outcome":{"enum":["pass","fail","inconclusive"]},"summary":string,"output_digest":string}),
        ),
    ];
    let operations = json!({"type":"array","minItems":1,"maxItems":128,"items":{"oneOf":[object(json!({"item_id":identifier,"clear":{"const":true}})),object(json!({"item_id":identifier,"record":{"oneOf":records}}))]}});
    let actions = [
        ("status",json!({"target":{"oneOf":[object(json!({"kind":{"const":"context"}})),object(json!({"kind":{"const":"request"},"request_id":identifier}))]}})),
        ("inspect",json!({"selection_ref":identifier,"view":{"oneOf":views},"range":{"oneOf":[object(json!({"kind":{"const":"start"},"offset":{"type":"integer","minimum":0}})),object(json!({"kind":{"const":"continue"},"cursor":identifier}))]}})),
        ("prepare_record",json!({"request_id":identifier,"context_ref":identifier,"evidence_ref":identifier,"operations":operations})),
        ("record",json!({"request_id":identifier,"decision":{"const":"record"}})),
        ("prepare_accept",json!({"request_id":identifier,"evidence_ref":identifier,"summary":{"type":"string","minLength":1,"maxLength":4096}})),
        ("accept",json!({"request_id":identifier,"decision":{"const":"accept"}})),
    ].into_iter().map(|(action, fields)|{
        let mut fields=fields.as_object().unwrap().clone();
        fields.insert("schema".into(),json!({"const":"dolgorae.consumer-control/v1"}));
        fields.insert("action".into(),json!({"const":action})); object(Value::Object(fields))
    }).collect::<Vec<_>>();
    json!({"name":name,"description":"Inspect original Specialist evidence. Prepare explicit placement-local records or acceptance, then submit the retained request only after your decision. Retain request_id before calling; recover unknown effects with status. No automatic acceptance or sequential acknowledgement is required.","inputSchema":{"type":"object","properties":{"schema":{"const":"dolgorae.consumer-control/v1"},"action":{"enum":["status","inspect","prepare_record","record","prepare_accept","accept"]}},"required":["schema","action"],"dependentSchemas":{"action":{"oneOf":actions}}}})
}

/// Check duplicate keys without JCS number normalization. Public u64/i64 fences
/// are decoded from the original bytes, never from canonicalized float output.
pub(crate) fn decode<T: DeserializeOwned>(bytes: &[u8], maximum: usize) -> Result<T, MachineError> {
    if bytes.len() > maximum {
        return Err(invalid());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
    crate::jcs::parse(text).map_err(|_| invalid())?;
    serde_json::from_slice(bytes).map_err(|_| invalid())
}
pub(crate) fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, MachineError> {
    // Value maps are ordered by UTF-8 key bytes. Only typed integer fields enter
    // persisted intent/public preimages; floats are not part of this protocol.
    serde_json::to_vec(&serde_json::to_value(value).map_err(|_| invalid())?).map_err(|_| invalid())
}
pub(crate) fn hash(bytes: &[u8]) -> String {
    format!("sha256:{}", crate::jcs::sha256_hex(bytes))
}
pub(crate) fn invalid() -> MachineError {
    error("invalid_input", "decode", Effect::NotCalled)
}
pub(crate) fn error(code: &str, phase: &str, effect: Effect) -> MachineError {
    MachineError::new(
        "CONSUMER_CONTROL_REFUSED",
        "private consumer request refused",
        false,
        json!({"code":code,"phase":phase,"effect":effect,"recovery":match effect {
            Effect::Unknown => "lookup_original_request", Effect::Committed => "inspect_historical_outcome",
            _ => "fix_request"}}),
    )
}
fn identifier(text: &str) -> bool {
    (1..=128).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum StatusTarget {
    Context,
    Request { request_id: String },
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum View {
    CompleteResult,
    FinalResponse,
    StructuredOutput,
    Package,
    PreparedInput,
    Outcome,
    Receipt,
    PackageEvidence { index: u64 },
    Change { index: u64, side: Side },
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Side {
    Before,
    After,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Range {
    Start { offset: u64 },
    Continue { cursor: String },
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InputBasis {
    pub descriptor: String,
    pub digest: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Executor {
    pub name: String,
    pub version: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CheckOutcome {
    Pass,
    Fail,
    Inconclusive,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum RecordValue {
    Confirm {
        value: bool,
    },
    Text {
        value: String,
    },
    Choice {
        value: String,
    },
    Integer {
        value: String,
    },
    List {
        value: Vec<String>,
    },
    Artifact {
        artifact_ref: String,
    },
    CheckResult {
        operation_id: String,
        operation_digest: String,
        input_basis: InputBasis,
        executor: Executor,
        outcome: CheckOutcome,
        summary: String,
        output_digest: String,
    },
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum RecordOperation {
    Clear(ClearOperation),
    Record(ValueOperation),
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClearOperation {
    pub item_id: String,
    pub clear: bool,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ValueOperation {
    pub item_id: String,
    pub record: RecordValue,
}
impl RecordOperation {
    pub(crate) fn item(&self) -> &str {
        match self {
            Self::Clear(v) => &v.item_id,
            Self::Record(v) => &v.item_id,
        }
    }
    fn validate(&self) -> Result<(), MachineError> {
        if !crate::podway_client::item_id(self.item()) {
            return Err(invalid());
        }
        match self {
            Self::Clear(v) if !v.clear => Err(invalid()),
            Self::Record(v) => v.record.validate(),
            _ => Ok(()),
        }
    }
}
impl RecordValue {
    pub(crate) fn validate(&self) -> Result<(), MachineError> {
        use crate::trusted_host::is_digest;
        let valid = match self {
            Self::Confirm { .. } => true,
            Self::Text { value } => value.chars().count() <= 65_536,
            Self::Choice { value } => (1..=120).contains(&value.chars().count()),
            Self::Integer { value } => value.parse::<i64>().is_ok_and(|i| i.to_string() == *value),
            Self::List { value } => {
                value.len() <= 1000
                    && value
                        .iter()
                        .all(|v| (1..=8192).contains(&v.chars().count()))
            }
            Self::Artifact { artifact_ref } => identifier(artifact_ref),
            Self::CheckResult {
                operation_id,
                operation_digest,
                input_basis,
                executor,
                summary,
                output_digest,
                ..
            } => {
                crate::podway_client::item_id(operation_id)
                    && is_digest(operation_digest)
                    && (1..=512).contains(&input_basis.descriptor.chars().count())
                    && is_digest(&input_basis.digest)
                    && (1..=128).contains(&executor.name.chars().count())
                    && (1..=64).contains(&executor.version.chars().count())
                    && (1..=2000).contains(&summary.chars().count())
                    && is_digest(output_digest)
            }
        };
        if valid { Ok(()) } else { Err(invalid()) }
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Action {
    Status {
        target: StatusTarget,
    },
    Inspect {
        selection_ref: String,
        view: View,
        range: Range,
    },
    PrepareRecord {
        request_id: String,
        context_ref: String,
        evidence_ref: String,
        operations: Vec<RecordOperation>,
    },
    Record {
        request_id: String,
        decision: RecordDecision,
    },
    PrepareAccept {
        request_id: String,
        evidence_ref: String,
        summary: String,
    },
    Accept {
        request_id: String,
        decision: AcceptDecision,
    },
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RecordDecision {
    Record,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AcceptDecision {
    Accept,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Control {
    pub action: Action,
}
impl Control {
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, MachineError> {
        // Remove only the mandatory schema, then decode the closed action.
        let mut value: Value = decode(bytes, MAX_INPUT)?;
        let object = value.as_object_mut().ok_or_else(invalid)?;
        if object.remove("schema") != Some(json!("dolgorae.consumer-control/v1")) {
            return Err(invalid());
        }
        // Serde's internally tagged unit variants ignore extra members. Close
        // these singleton objects explicitly before the typed decoder.
        if value["action"] == "status"
            && value["target"]["kind"] == "context"
            && value["target"].as_object().is_none_or(|v| v.len() != 1)
        {
            return Err(invalid());
        }
        if value["action"] == "inspect"
            && matches!(
                value["view"]["kind"].as_str(),
                Some(
                    "complete_result"
                        | "final_response"
                        | "structured_output"
                        | "package"
                        | "prepared_input"
                        | "outcome"
                        | "receipt"
                )
            )
            && value["view"].as_object().is_none_or(|v| v.len() != 1)
        {
            return Err(invalid());
        }
        let action: Action = decode(&encode(&value)?, MAX_INPUT)?;
        let result = Self { action };
        result.validate()?;
        Ok(result)
    }
    fn validate(&self) -> Result<(), MachineError> {
        if self.request_id().is_some_and(|v| !identifier(v)) {
            return Err(invalid());
        }
        match &self.action {
            Action::PrepareRecord {
                context_ref,
                evidence_ref,
                operations,
                ..
            } => {
                if !identifier(context_ref)
                    || !identifier(evidence_ref)
                    || !(1..=128).contains(&operations.len())
                    || operations
                        .iter()
                        .map(RecordOperation::item)
                        .collect::<BTreeSet<_>>()
                        .len()
                        != operations.len()
                {
                    return Err(invalid());
                }
                for operation in operations {
                    operation.validate()?;
                }
            }
            Action::PrepareAccept {
                evidence_ref,
                summary,
                ..
            } => {
                if !identifier(evidence_ref)
                    || !(1..=4096).contains(&summary.len())
                    || summary.contains('\0')
                {
                    return Err(invalid());
                }
            }
            Action::Inspect {
                selection_ref,
                range,
                ..
            } if !identifier(selection_ref)
                || matches!(range,Range::Continue{cursor} if !identifier(cursor)) =>
            {
                return Err(invalid());
            }
            _ => {}
        }
        Ok(())
    }
    pub(crate) fn request_id(&self) -> Option<&str> {
        match &self.action {
            Action::Status {
                target: StatusTarget::Request { request_id },
            }
            | Action::PrepareRecord { request_id, .. }
            | Action::PrepareAccept { request_id, .. }
            | Action::Record { request_id, .. }
            | Action::Accept { request_id, .. } => Some(request_id),
            _ => None,
        }
    }
    pub(crate) fn correlation(&self) -> ControlCorrelation {
        ControlCorrelation {
            action: self.name(),
            request_id: self.request_id().map(str::to_owned),
        }
    }
    pub(crate) fn name(&self) -> &'static str {
        match self.action {
            Action::Status { .. } => "status",
            Action::Inspect { .. } => "inspect",
            Action::PrepareRecord { .. } => "prepare_record",
            Action::Record { .. } => "record",
            Action::PrepareAccept { .. } => "prepare_accept",
            Action::Accept { .. } => "accept",
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Effect {
    NotCalled,
    Rejected,
    Unknown,
    Committed,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum State {
    Preparing,
    Prepared,
    Claimed,
    Running,
    Uncertain,
    Committed,
    Failure,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Mutation {
    Record,
    Accept,
}
impl Mutation {
    pub(crate) fn command(self) -> &'static str {
        match self {
            Self::Record => "item.record_many",
            Self::Accept => "delegation.accept",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PodwayConnection {
    pub executable: PathBuf,
    pub executable_sha256: String,
    pub manifest_digest: String,
    pub worktree: PathBuf,
    pub workspace: Uuid,
    pub session: Uuid,
    pub attempt: Uuid,
    pub placement: String,
    pub adapter_id: String,
    pub adapter_version: String,
    pub permitted_items: BTreeSet<String>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Mode {
    NativeOnly {},
    Delegated { connection: Box<PodwayConnection> },
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Setup {
    owner: String,
    birth: Uuid,
    generation: u64,
    caller: Option<crate::delegation_identity::CallerScope>,
    mode: Mode,
}
impl Setup {
    fn current(owner: &VerifiedOwner, mode: Mode) -> Result<Self, MachineError> {
        owner.revalidate()?;
        Ok(Self {
            owner: owner.configuration_digest.clone(),
            birth: crate::engagement::require_delegation_namespace(&owner.state_root)?,
            generation: crate::host_invocation::preparation_generation(owner)?,
            caller: owner.configuration.caller_scope.clone(),
            mode,
        })
    }

    fn matches(&self, expected: &Self, owner: &VerifiedOwner) -> bool {
        owner.matches_persisted_owner_digest(&self.owner)
            && self.birth == expected.birth
            && self.generation == expected.generation
            && self.caller == expected.caller
            && self.mode == expected.mode
    }
}

/// Constructed only from the authenticated host record or Worker-owned call.
/// Request JSON cannot create a witness.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Witness {
    owner: String,
    generation: u64,
    primary: String,
    turn: Option<String>,
    call: String,
    root_generation: Option<u64>,
}
impl Witness {
    pub(crate) fn external(
        owner: &VerifiedOwner,
        bound: &crate::host_invocation::BoundInvocation,
    ) -> Result<Self, MachineError> {
        bound.require_owner(owner)?;
        bound.context().validate()?;
        let event = bound.native_event();
        let result = Self {
            owner: owner.configuration_digest.clone(),
            generation: bound.context().state_generation,
            primary: event.session.clone(),
            turn: event.turn.clone(),
            call: event.tool_use.clone(),
            root_generation: None,
        };
        result.verify(owner)?;
        Ok(result)
    }
    pub(crate) fn managed(
        owner: &VerifiedOwner,
        call: &crate::turn::PrimaryToolCall,
        root: Uuid,
        generation: u64,
    ) -> Result<Self, MachineError> {
        use crate::trusted_host::OwnerKind;
        if owner.configuration.owner != (OwnerKind::Managed { session_ref: root })
            || call.run_generation != generation
            || call.context.source_run_id != root
            || call.context.session_id != root
            || call.source_thread_id.is_empty()
            || generation == 0
            || !matches!(call.tool_name.as_str(), MANAGED_NATIVE | MANAGED_CONTROL)
        {
            return Err(refusal());
        }
        let result = Self {
            owner: owner.configuration_digest.clone(),
            generation: crate::host_invocation::preparation_generation(owner)?,
            primary: root.to_string(),
            turn: Some(call.context.source_turn_id.clone()),
            call: call.context.source_tool_call_id.clone(),
            root_generation: Some(generation),
        };
        result.verify(owner)?;
        Ok(result)
    }
    fn verify(&self, owner: &VerifiedOwner) -> Result<(), MachineError> {
        owner.revalidate()?;
        if matches!(
            owner.configuration.owner,
            crate::trusted_host::OwnerKind::External
        ) != self.root_generation.is_none()
        {
            return Err(refusal());
        }
        if let Some(generation) = self.root_generation {
            let root = Uuid::parse_str(&self.primary).map_err(|_| refusal())?;
            if owner.configuration.owner
                != (crate::trusted_host::OwnerKind::Managed { session_ref: root })
                || crate::run::RunStore::new(
                    crate::workspace::SystemWorkspacePlatform,
                    &owner.state_root,
                )
                .load_state_projection(root)?
                .run_generation
                    != generation
            {
                return Err(refusal());
            }
        }
        if !owner.matches_persisted_owner_digest(&self.owner)
            || self.generation != crate::host_invocation::preparation_generation(owner)?
            || !crate::trusted_host::opaque(&self.primary, 512)
            || self
                .turn
                .as_ref()
                .is_some_and(|v| !crate::trusted_host::opaque(v, 512))
            || !crate::trusted_host::opaque(&self.call, 512)
        {
            return Err(refusal());
        }
        Ok(())
    }
    fn scope(&self, setup: &Setup) -> Result<String, MachineError> {
        Ok(hash(&encode(
            &json!({"setup":setup,"primary":self.primary}),
        )?))
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Prepared {
    pub mutation: Mutation,
    pub input: Vec<u8>,
    pub preimage: Value,
    pub evidence_ref: String,
    pub selection_ref: String,
    pub request_digest: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub request_id: String,
    pub intent: Action,
    pub key: String,
    pub state: State,
    pub prepared: Option<Prepared>,
    pub witness: Option<Witness>,
    pub claim: Option<Uuid>,
    pub child: Option<u32>,
    pub effect: Effect,
    pub outcome: Option<Value>,
    pub receipt: Option<Value>,
    pub applicability: Option<String>,
}

fn persistence_ready(owner: &VerifiedOwner, pending_allowed: bool) -> Result<(), MachineError> {
    crate::host_invocation::require_no_sidecars(owner, DATABASE)?;
    crate::host_invocation::require_no_sidecars(owner, STAGING)?;
    if crate::host_invocation::persistence_entry_exists(&owner.root.path.join(STAGING))?
        && !pending_allowed
    {
        return Err(refusal());
    }
    Ok(())
}

pub(crate) fn initialize(owner: &VerifiedOwner, mode: Mode) -> Result<(), MachineError> {
    if let Mode::Delegated { connection } = &mode {
        crate::podway_client::validate_connection(owner, connection)?;
    }
    let setup = Setup::current(owner, mode)?;
    let _lock = owner.root.lock()?;
    owner.revalidate()?;
    persistence_ready(owner, false)?;
    if crate::host_invocation::persistence_entry_exists(&owner.root.path.join(DATABASE))? {
        let (db, _) = crate::host_invocation::database_connection(owner, DATABASE)?;
        let text: Vec<u8> = db
            .query_row(
                "SELECT setup FROM consumer_metadata WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .map_err(|_| refusal())?;
        if !decode::<Setup>(&text, MAX_INPUT)?.matches(&setup, owner) {
            return Err(refusal());
        }
        return Ok(());
    }
    let staging = owner.root.create_file(STAGING)?;
    let (db, _) = crate::host_invocation::database_connection(owner, STAGING)?;
    db.execute_batch("CREATE TABLE consumer_metadata(singleton INTEGER PRIMARY KEY CHECK(singleton=1), setup TEXT NOT NULL);
        CREATE TABLE requests(scope TEXT NOT NULL, request_id TEXT NOT NULL, record BLOB NOT NULL, PRIMARY KEY(scope,request_id));
        CREATE TABLE selections(scope TEXT NOT NULL, reference TEXT NOT NULL, record BLOB NOT NULL, PRIMARY KEY(scope,reference));").map_err(|_|refusal())?;
    db.execute(
        "INSERT INTO consumer_metadata VALUES(1,?1)",
        [encode(&setup)?.as_slice()],
    )
    .map_err(|_| refusal())?;
    db.close().map_err(|_| refusal())?;
    staging.sync_all().map_err(|_| refusal())?;
    persistence_ready(owner, true)?;
    owner.root.open_file(&owner.root.path.join(STAGING))?;
    if crate::host_invocation::persistence_entry_exists(&owner.root.path.join(DATABASE))? {
        return Err(refusal());
    }
    crate::darwin::DarwinSystem
        .renameat_exclusive(
            &owner.root.file,
            std::ffi::OsStr::new(STAGING),
            std::ffi::OsStr::new(DATABASE),
        )
        .map_err(|_| refusal())?;
    owner.root.file.sync_all().map_err(|_| refusal())?;
    Ok(())
}

pub(crate) struct Store<'a> {
    owner: &'a VerifiedOwner,
    pub witness: Witness,
    setup: Setup,
    scope: String,
}
impl<'a> Store<'a> {
    pub(crate) fn open(owner: &'a VerifiedOwner, witness: Witness) -> Result<Self, MachineError> {
        witness.verify(owner)?;
        let setup = {
            let _lock = owner.root.lock()?;
            owner.revalidate()?;
            persistence_ready(owner, false)?;
            let (db, _) = crate::host_invocation::database_connection(owner, DATABASE)?;
            let bytes: Vec<u8> = db
                .query_row(
                    "SELECT setup FROM consumer_metadata WHERE singleton=1",
                    [],
                    |r| r.get(0),
                )
                .map_err(|_| refusal())?;
            decode::<Setup>(&bytes, MAX_INPUT)?
        };
        if !setup.matches(&Setup::current(owner, setup.mode.clone())?, owner) {
            return Err(refusal());
        }
        let scope = witness.scope(&setup)?;
        Ok(Self {
            owner,
            witness,
            setup,
            scope,
        })
    }
    pub(crate) fn connection(&self) -> Result<&PodwayConnection, MachineError> {
        match &self.setup.mode {
            Mode::NativeOnly {} => Err(error("inactive_consumer", "authority", Effect::NotCalled)),
            Mode::Delegated { connection } => Ok(connection),
        }
    }
    pub(crate) fn authorize(&self) -> Result<(), MachineError> {
        self.witness.verify(self.owner)
    }
    pub(crate) fn authorize_source(
        &self,
        owner: &VerifiedOwner,
        managed: Option<&crate::delegation_service::ManagedCallContext<'_>>,
    ) -> Result<(), MachineError> {
        self.authorize()?;
        owner.revalidate()?;
        if owner.configuration_digest != self.owner.configuration_digest
            || owner.state_root != self.owner.state_root
        {
            return Err(refusal());
        }
        match (owner.configuration.owner.clone(), managed) {
            (crate::trusted_host::OwnerKind::External, None) => Ok(()),
            (crate::trusted_host::OwnerKind::Managed { session_ref }, Some(context))
                if context.session_id == session_ref
                    && context.source_run_id == session_ref
                    && Some(context.source_turn_id) == self.witness.turn.as_deref()
                    && context.source_tool_call_id == self.witness.call =>
            {
                Ok(())
            }
            _ => Err(refusal()),
        }
    }
    fn access<T>(
        &self,
        action: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T, MachineError>,
    ) -> Result<T, MachineError> {
        self.authorize()?;
        let _lock = self.owner.root.lock()?;
        self.owner.revalidate()?;
        persistence_ready(self.owner, false)?;
        let (mut db, file) = crate::host_invocation::database_connection(self.owner, DATABASE)?;
        let tx = db.transaction().map_err(|_| refusal())?;
        let setup: Vec<u8> = tx
            .query_row(
                "SELECT setup FROM consumer_metadata WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .map_err(|_| refusal())?;
        if decode::<Setup>(&setup, MAX_INPUT)? != self.setup {
            return Err(refusal());
        }
        let result = action(&tx)?;
        tx.commit().map_err(|_| refusal())?;
        crate::host_invocation::verify_database(self.owner, &file, DATABASE)?;
        persistence_ready(self.owner, false)?;
        Ok(result)
    }
    fn read(
        tx: &rusqlite::Transaction<'_>,
        scope: &str,
        id: &str,
    ) -> Result<Option<Request>, MachineError> {
        let bytes: Option<Vec<u8>> = tx
            .query_row(
                "SELECT record FROM requests WHERE scope=?1 AND request_id=?2",
                params![scope, id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|_| refusal())?;
        bytes.map(|b| decode(&b, 16 * MAX_INPUT)).transpose()
    }
    fn write(tx: &rusqlite::Transaction<'_>, scope: &str, r: &Request) -> Result<(), MachineError> {
        tx.execute(
            "UPDATE requests SET record=?3 WHERE scope=?1 AND request_id=?2",
            params![scope, r.request_id, encode(r)?],
        )
        .map_err(|_| refusal())?;
        Ok(())
    }
    pub(crate) fn request(&self, id: &str) -> Result<Request, MachineError> {
        self.access(|tx| Self::read(tx, &self.scope, id)?.ok_or_else(invalid))
    }
    pub(crate) fn reserve(&self, control: &Control) -> Result<Request, MachineError> {
        control.validate()?;
        if !matches!(
            control.action,
            Action::PrepareAccept { .. } | Action::PrepareRecord { .. }
        ) {
            return Err(invalid());
        }
        let id = control.request_id().ok_or_else(invalid)?;
        self.access(|tx| {
            if let Some(r) = Self::read(tx, &self.scope, id)? {
                if r.intent != control.action {
                    return Err(error("request_conflict", "prepare", Effect::NotCalled));
                }
                return Ok(r);
            }
            let r = Request {
                request_id: id.into(),
                intent: control.action.clone(),
                key: Uuid::now_v7().to_string(),
                state: State::Preparing,
                prepared: None,
                witness: None,
                claim: None,
                child: None,
                effect: Effect::NotCalled,
                outcome: None,
                receipt: None,
                applicability: None,
            };
            tx.execute(
                "INSERT INTO requests VALUES(?1,?2,?3)",
                params![self.scope, id, encode(&r)?],
            )
            .map_err(|_| refusal())?;
            Ok(r)
        })
    }
    pub(crate) fn prepare(
        &self,
        id: &str,
        mut prepared: Prepared,
    ) -> Result<Request, MachineError> {
        if prepared.input.len() > MAX_INPUT
            || prepared.request_digest
                != crate::podway_client::request_digest(prepared.mutation, &prepared.input)?
        {
            return Err(invalid());
        }
        self.access(|tx| {
            let mut r = Self::read(tx, &self.scope, id)?.ok_or_else(invalid)?;
            if r.state != State::Preparing {
                if let Some(existing) = &r.prepared {
                    prepared.selection_ref = existing.selection_ref.clone();
                }
                if r.prepared.as_ref() == Some(&prepared) {
                    return Ok(r);
                }
                return Err(error("request_conflict", "prepare", r.effect));
            }
            prepared.selection_ref = Uuid::now_v7().to_string();
            tx.execute(
                "INSERT INTO selections VALUES(?1,?2,?3)",
                params![
                    self.scope,
                    prepared.selection_ref,
                    encode(&json!({"kind":"request","request_id":id}))?
                ],
            )
            .map_err(|_| refusal())?;
            r.prepared = Some(prepared);
            r.state = State::Prepared;
            Self::write(tx, &self.scope, &r)?;
            Ok(r)
        })
    }
    /// Only the caller receiving this freshly generated token can launch. A
    /// reopened claimed/running request is observed as uncertain, never claimed.
    pub(crate) fn claim(
        &self,
        id: &str,
        mutation: Mutation,
    ) -> Result<(Request, Option<Uuid>), MachineError> {
        self.access(|tx| {
            let mut r = Self::read(tx, &self.scope, id)?.ok_or_else(invalid)?;
            if r.prepared.as_ref().is_none_or(|p| p.mutation != mutation) {
                return Err(invalid());
            }
            if r.state != State::Prepared {
                return Ok((r, None));
            }
            let claim = Uuid::now_v7();
            r.claim = Some(claim);
            r.witness = Some(self.witness.clone());
            r.state = State::Claimed;
            r.effect = Effect::Unknown;
            Self::write(tx, &self.scope, &r)?;
            Ok((r, Some(claim)))
        })
    }
    pub(crate) fn running(&self, id: &str, claim: Uuid, pid: u32) -> Result<(), MachineError> {
        self.change_claim(id, claim, |r| {
            if r.state == State::Committed {
                return Ok(());
            }
            if !matches!(r.state, State::Claimed | State::Uncertain) || pid == 0 {
                return Err(invalid());
            }
            r.state = State::Running;
            r.child = Some(pid);
            Ok(())
        })
    }
    pub(crate) fn finish(
        &self,
        id: &str,
        claim: Uuid,
        effect: Effect,
        outcome: Option<Value>,
    ) -> Result<(), MachineError> {
        self.change_claim(id, claim, |r| {
            if r.state == State::Committed {
                return Ok(());
            }
            if !matches!(r.state, State::Claimed | State::Running | State::Uncertain) {
                return Err(invalid());
            }
            r.state = match effect {
                Effect::Committed => State::Committed,
                Effect::Unknown => State::Uncertain,
                _ => State::Failure,
            };
            r.effect = effect;
            r.outcome = outcome;
            if effect == Effect::Committed {
                // A receipt seen before the original key was proved belongs
                // to the uncertain observation, not this verified outcome.
                r.receipt = None;
            }
            Ok(())
        })
    }
    fn change_claim(
        &self,
        id: &str,
        claim: Uuid,
        f: impl FnOnce(&mut Request) -> Result<(), MachineError>,
    ) -> Result<(), MachineError> {
        self.access(|tx| {
            let mut r = Self::read(tx, &self.scope, id)?.ok_or_else(invalid)?;
            if r.claim != Some(claim) {
                return Err(error("request_conflict", "claim", r.effect));
            }
            f(&mut r)?;
            Self::write(tx, &self.scope, &r)
        })
    }
    pub(crate) fn recover(
        &self,
        id: &str,
        outcome: Option<Value>,
        receipt: Option<Value>,
        applicability: Option<String>,
    ) -> Result<Request, MachineError> {
        self.access(|tx| {
            let mut r = Self::read(tx, &self.scope, id)?.ok_or_else(invalid)?;
            if let Some(outcome) = outcome {
                if r.state != State::Committed {
                    r.receipt = None;
                }
                r.outcome = Some(outcome);
                r.effect = Effect::Committed;
                r.state = State::Committed;
            } else if matches!(r.state, State::Claimed | State::Running | State::Uncertain) {
                r.state = State::Uncertain;
                r.effect = Effect::Unknown;
            }
            if receipt.is_some() {
                r.receipt = receipt;
            }
            r.applicability = applicability;
            Self::write(tx, &self.scope, &r)?;
            Ok(r)
        })
    }
    pub(crate) fn selection<T: DeserializeOwned>(
        &self,
        reference: &str,
    ) -> Result<T, MachineError> {
        self.access(|tx| {
            let bytes: Vec<u8> = tx
                .query_row(
                    "SELECT record FROM selections WHERE scope=?1 AND reference=?2",
                    params![self.scope, reference],
                    |r| r.get(0),
                )
                .map_err(|_| invalid())?;
            decode(&bytes, MAX_INPUT)
        })
    }
    pub(crate) fn retain_selection(&self, value: &impl Serialize) -> Result<String, MachineError> {
        let bytes = encode(value)?;
        if bytes.len() > MAX_INPUT {
            return Err(invalid());
        }
        let reference = Uuid::now_v7().to_string();
        self.access(|tx| {
            tx.execute(
                "INSERT INTO selections VALUES(?1,?2,?3)",
                params![self.scope, reference, bytes],
            )
            .map_err(|_| refusal())?;
            Ok(reference)
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::trusted_host::{HostRoute, tests::Fixture};

    #[test]
    fn refresh_retains_legacy_consumer_scope_and_accepts_new_witnesses() {
        let mut fixture = Fixture::new(HostRoute::Codex);
        fixture.complete_legacy();
        let old = fixture.owner();
        initialize(&old, Mode::NativeOnly {}).unwrap();
        let original_witness = witness(&old);
        let store = Store::open(&old, original_witness.clone()).unwrap();
        let original_scope = store.scope.clone();
        let path = old.root.path.join(DATABASE);
        let original_bytes = std::fs::read(&path).unwrap();
        let replacement = fixture.refreshed_image("0.172.1");
        fixture
            .refresh(&fixture.path(), &replacement, false)
            .unwrap();
        let current = fixture.owner();
        initialize(&current, Mode::NativeOnly {}).unwrap();
        let current_witness = witness(&current);
        assert_eq!(current_witness.owner, current.configuration_digest);
        assert_ne!(current_witness.owner, original_witness.owner);
        assert_eq!(
            Store::open(&current, original_witness).unwrap().scope,
            original_scope
        );
        assert_eq!(
            Store::open(&current, current_witness).unwrap().scope,
            original_scope
        );
        assert_eq!(std::fs::read(path).unwrap(), original_bytes);
    }
    pub(crate) fn witness(owner: &VerifiedOwner) -> Witness {
        Witness {
            owner: owner.configuration_digest.clone(),
            generation: crate::host_invocation::preparation_generation(owner).unwrap(),
            primary: match owner.configuration.owner {
                crate::trusted_host::OwnerKind::External => "original-session".into(),
                crate::trusted_host::OwnerKind::Managed { session_ref } => session_ref.to_string(),
            },
            turn: Some(
                match owner.configuration.owner {
                    crate::trusted_host::OwnerKind::External => "turn-1",
                    _ => "root-turn",
                }
                .into(),
            ),
            call: match owner.configuration.owner {
                crate::trusted_host::OwnerKind::External => "call-1",
                _ => "root-call",
            }
            .into(),
            root_generation: match owner.configuration.owner {
                crate::trusted_host::OwnerKind::External => None,
                crate::trusted_host::OwnerKind::Managed { session_ref } => Some(
                    crate::run::RunStore::new(
                        crate::workspace::SystemWorkspacePlatform,
                        &owner.state_root,
                    )
                    .load_state_projection(session_ref)
                    .unwrap()
                    .run_generation,
                ),
            },
        }
    }
    fn control(action: Value) -> Vec<u8> {
        let mut value = action;
        value["schema"] = json!("dolgorae.consumer-control/v1");
        encode(&value).unwrap()
    }
    #[test]
    fn closed_control_grammar_all_six_arms_and_nested_failures() {
        let good = [
            json!({"action":"status","target":{"kind":"context"}}),
            json!({"action":"inspect","selection_ref":"s1","view":{"kind":"change","index":0,"side":"before"},"range":{"kind":"start","offset":0}}),
            json!({"action":"prepare_record","request_id":"r1","context_ref":"c1","evidence_ref":"e1","operations":[{"item_id":"summary","clear":true}]}),
            json!({"action":"record","request_id":"r1","decision":"record"}),
            json!({"action":"prepare_accept","request_id":"r2","evidence_ref":"e1","summary":"Accepted."}),
            json!({"action":"accept","request_id":"r2","decision":"accept"}),
        ];
        for value in good {
            assert!(Control::parse(&control(value.clone())).is_ok(), "{value}");
            let mut extra = value.clone();
            extra["arbitrary"] = json!(0);
            assert!(Control::parse(&control(extra)).is_err());
            for key in value.as_object().unwrap().keys() {
                let mut missing = value.clone();
                missing.as_object_mut().unwrap().remove(key);
                assert!(Control::parse(&control(missing)).is_err(), "missing {key}");
                let mut null = value.clone();
                null[key] = Value::Null;
                assert!(Control::parse(&control(null)).is_err(), "null {key}");
            }
        }
        for raw in [b"\xff".as_slice(),b"{} {}",b"{\"schema\":\"dolgorae.consumer-control/v1\",\"action\":\"status\",\"action\":\"status\",\"target\":{\"kind\":\"context\"}}",
            b"{\"schema\":\"dolgorae.consumer-control/v1\",\"action\":\"status\",\"target\":{\"kind\":\"context\",\"kind\":\"context\"}}"]{assert!(Control::parse(raw).is_err());}
        for bad in [
            json!({"action":"record","request_id":"r","decision":"accept"}),
            json!({"action":"status","target":{"kind":"context","request_id":"r"}}),
            json!({"action":"inspect","selection_ref":"e","view":{"kind":"package","index":0},"range":{"kind":"start","offset":0}}),
            json!({"action":"inspect","selection_ref":"e","view":{"kind":"package"},"range":{"kind":"start","offset":-1}}),
            json!({"action":"inspect","selection_ref":"e","view":{"kind":"package"},"range":{"kind":"start","offset":1.0}}),
        ] {
            assert!(Control::parse(&control(bad)).is_err());
        }
    }
    #[test]
    fn operation_counts_ids_disjoint_values_and_full_integer_domain() {
        for count in [0, 1, 128, 129] {
            let ops = (0..count)
                .map(|n| json!({"item_id":format!("item-{n}"),"clear":true}))
                .collect::<Vec<_>>();
            let value = json!({"action":"prepare_record","request_id":"r","context_ref":"c","evidence_ref":"e","operations":ops});
            assert_eq!(
                Control::parse(&control(value)).is_ok(),
                matches!(count, 1 | 128)
            );
        }
        for value in [i64::MIN.to_string(), i64::MAX.to_string(), "0".into()] {
            assert!(RecordValue::Integer { value }.validate().is_ok());
        }
        for value in [
            "-0",
            "+1",
            "01",
            "1.0",
            "9223372036854775808",
            "-9223372036854775809",
        ] {
            assert!(
                RecordValue::Integer {
                    value: value.into()
                }
                .validate()
                .is_err()
            );
        }
        for op in [
            json!({"item_id":"x","clear":false}),
            json!({"item_id":"x","clear":true,"record":{"type":"confirm","value":true}}),
            json!({"item_id":"x","record":{"type":"artifact","path":"/tmp/model"}}),
            json!({"item_id":"x","record":{"type":"integer","value":1}}),
        ] {
            let bytes = control(
                json!({"action":"prepare_record","request_id":"r","context_ref":"c","evidence_ref":"e","operations":[op]}),
            );
            assert!(Control::parse(&bytes).is_err());
        }
        let duplicate = json!({"action":"prepare_record","request_id":"r","context_ref":"c","evidence_ref":"e","operations":[{"item_id":"x","clear":true},{"item_id":"x","clear":true}]});
        assert!(Control::parse(&control(duplicate)).is_err());
    }
    fn prepare(store: &Store<'_>, id: &str) -> Request {
        let control = Control {
            action: Action::PrepareAccept {
                request_id: id.into(),
                evidence_ref: "evidence".into(),
                summary: "Accepted".into(),
            },
        };
        let r = store.reserve(&control).unwrap();
        let input=encode(&json!({"workspace_uuid":"w","session_id":"s","attempt_id":"a","session_revision":u64::MAX,"idempotency_key":r.key})).unwrap();
        store
            .prepare(
                id,
                Prepared {
                    mutation: Mutation::Accept,
                    request_digest: crate::podway_client::request_digest(Mutation::Accept, &input)
                        .unwrap(),
                    input,
                    preimage: json!({}),
                    evidence_ref: "evidence".into(),
                    selection_ref: "selection".into(),
                },
            )
            .unwrap()
    }
    #[test]
    fn setup_is_explicit_native_only_immutable_and_authority_scoped() {
        let f = Fixture::new(HostRoute::Codex);
        f.complete();
        let owner = f.owner();
        let w = witness(&owner);
        assert!(Store::open(&owner, w.clone()).is_err());
        initialize(&owner, Mode::NativeOnly {}).unwrap();
        initialize(&owner, Mode::NativeOnly {}).unwrap();
        let store = Store::open(&owner, w.clone()).unwrap();
        assert!(store.connection().is_err());
        let r = prepare(&store, "request-1");
        assert_eq!(r.state, State::Prepared);
        for field in 0..5 {
            let mut wrong = w.clone();
            match field {
                0 => wrong.owner.push('x'),
                1 => wrong.generation += 1,
                2 => wrong.primary.clear(),
                3 => wrong.turn = Some(String::new()),
                _ => wrong.call.clear(),
            };
            assert!(Store::open(&owner, wrong).is_err());
        }
        let mut foreign = w;
        foreign.primary = "other-session".into();
        let store = Store::open(&owner, foreign).unwrap();
        assert!(store.request("request-1").is_err());
    }
    #[test]
    fn first_reply_loss_retains_key_and_changed_intent_conflicts() {
        let f = Fixture::new(HostRoute::Codex);
        f.complete();
        let owner = f.owner();
        initialize(&owner, Mode::NativeOnly {}).unwrap();
        let control = Control {
            action: Action::PrepareAccept {
                request_id: "first".into(),
                evidence_ref: "e".into(),
                summary: "Accepted".into(),
            },
        };
        let first = Store::open(&owner, witness(&owner))
            .unwrap()
            .reserve(&control)
            .unwrap();
        let store = Store::open(&owner, witness(&owner)).unwrap();
        assert_eq!(store.reserve(&control).unwrap(), first);
        let mut changed = control.clone();
        if let Action::PrepareAccept { summary, .. } = &mut changed.action {
            summary.push('!');
        }
        assert!(store.reserve(&changed).is_err());
        let changed = Control {
            action: Action::PrepareRecord {
                request_id: "first".into(),
                context_ref: "c".into(),
                evidence_ref: "e".into(),
                operations: vec![RecordOperation::Clear(ClearOperation {
                    item_id: "x".into(),
                    clear: true,
                })],
            },
        };
        assert!(store.reserve(&changed).is_err());
        assert_eq!(store.request("first").unwrap().key, first.key);
    }
    #[test]
    fn duplicate_status_during_live_child_cannot_lose_original_completion() {
        let f = Fixture::new(HostRoute::Codex);
        f.complete();
        let owner = f.owner();
        initialize(&owner, Mode::NativeOnly {}).unwrap();
        let store = Store::open(&owner, witness(&owner)).unwrap();
        prepare(&store, "live");
        let (_, claim) = store.claim("live", Mutation::Accept).unwrap();
        let claim = claim.unwrap();
        store.running("live", claim, 42).unwrap();
        let duplicate = Store::open(&owner, witness(&owner)).unwrap();
        assert!(
            duplicate
                .claim("live", Mutation::Accept)
                .unwrap()
                .1
                .is_none()
        );
        assert_eq!(
            duplicate
                .recover(
                    "live",
                    None,
                    Some(json!({"accepted":"same-input-receipt"})),
                    Some("historical".into())
                )
                .unwrap()
                .state,
            State::Uncertain
        );
        assert_eq!(duplicate.request("live").unwrap().effect, Effect::Unknown);
        store
            .finish(
                "live",
                claim,
                Effect::Committed,
                Some(json!({"actual":"child-success"})),
            )
            .unwrap();
        let r = duplicate.request("live").unwrap();
        assert_eq!(r.state, State::Committed);
        assert_eq!(r.effect, Effect::Committed);
        assert_eq!(r.outcome, Some(json!({"actual":"child-success"})));
        assert!(r.receipt.is_none());
        prepare(&store, "lookup-live");
        store.claim("lookup-live", Mutation::Accept).unwrap();
        store
            .recover("lookup-live", None, Some(json!({"unverified":true})), None)
            .unwrap();
        let looked_up = store
            .recover(
                "lookup-live",
                Some(json!({"actual":"looked-up-success"})),
                None,
                None,
            )
            .unwrap();
        assert_eq!(looked_up.effect, Effect::Committed);
        assert!(looked_up.receipt.is_none());
        assert!(
            duplicate
                .claim("live", Mutation::Accept)
                .unwrap()
                .1
                .is_none()
        );
        assert!(
            duplicate
                .finish("live", Uuid::now_v7(), Effect::Rejected, None)
                .is_err()
        );
    }
    #[test]
    fn crash_boundaries_do_not_authorize_a_second_launch() {
        for boundary in 0..4 {
            let f = Fixture::new(HostRoute::Codex);
            f.complete();
            let owner = f.owner();
            initialize(&owner, Mode::NativeOnly {}).unwrap();
            let store = Store::open(&owner, witness(&owner)).unwrap();
            prepare(&store, "crash");
            let (_, claim) = store.claim("crash", Mutation::Accept).unwrap();
            let claim = claim.unwrap();
            if boundary >= 1 {
                store.running("crash", claim, 42).unwrap();
            }
            if boundary == 2 {
                store.finish("crash", claim, Effect::Unknown, None).unwrap();
            }
            if boundary == 3 {
                store
                    .finish(
                        "crash",
                        claim,
                        Effect::Committed,
                        Some(json!({"historical":true})),
                    )
                    .unwrap();
            }
            let restarted = Store::open(&owner, witness(&owner)).unwrap();
            let r = restarted
                .recover("crash", None, None, Some("historical".into()))
                .unwrap();
            assert_eq!(
                r.effect,
                if boundary == 3 {
                    Effect::Committed
                } else {
                    Effect::Unknown
                }
            );
            assert!(
                restarted
                    .claim("crash", Mutation::Accept)
                    .unwrap()
                    .1
                    .is_none()
            );
        }
    }
    #[test]
    fn concurrent_commit_claim_is_atomic() {
        let f = Fixture::new(HostRoute::Codex);
        f.complete();
        let owner = f.owner();
        initialize(&owner, Mode::NativeOnly {}).unwrap();
        prepare(&Store::open(&owner, witness(&owner)).unwrap(), "race");
        let home = f.home.root().parent().unwrap().to_owned();
        let path = f.path();
        let barrier = std::sync::Barrier::new(3);
        let claims = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for _ in 0..2 {
                let home = &home;
                let path = &path;
                let barrier = &barrier;
                handles.push(scope.spawn(move || {
                    barrier.wait();
                    let home =
                        crate::paths::DolgoraeHome::from_canonical_home(home.clone()).unwrap();
                    let owner = VerifiedOwner::load(&home, path).unwrap();
                    let store = Store::open(&owner, witness(&owner)).unwrap();
                    store.claim("race", Mutation::Accept).unwrap().1
                }));
            }
            barrier.wait();
            handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(claims.iter().filter(|c| c.is_some()).count(), 1);
    }
    #[test]
    fn selection_handles_are_scoped_and_no_source_bytes_are_spooled() {
        let f = Fixture::new(HostRoute::Codex);
        f.complete();
        let owner = f.owner();
        initialize(&owner, Mode::NativeOnly {}).unwrap();
        let store = Store::open(&owner, witness(&owner)).unwrap();
        let reference = store
            .retain_selection(&json!({"task":"original","digest":"reference-only"}))
            .unwrap();
        assert_eq!(
            store.selection::<Value>(&reference).unwrap()["task"],
            "original"
        );
        let mut w = witness(&owner);
        w.primary = "foreign".into();
        assert!(
            Store::open(&owner, w)
                .unwrap()
                .selection::<Value>(&reference)
                .is_err()
        );
        assert!(store.selection::<Value>("unknown").is_err());
    }
    #[test]
    fn consumer_partial_state_is_preserved_for_both_names_and_all_sidecars() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};
        let names = [
            DATABASE,
            STAGING,
            "consumer.sqlite-wal",
            "consumer.sqlite-shm",
            "consumer.sqlite-journal",
            "consumer.pending-wal",
            "consumer.pending-shm",
            "consumer.pending-journal",
        ];
        for ready in [false, true] {
            for name in names {
                for object in ["file", "directory", "dangling_link"] {
                    if ready && name == DATABASE {
                        continue;
                    }
                    let f = Fixture::new(HostRoute::Codex);
                    f.complete();
                    let owner = f.owner();
                    if ready {
                        initialize(&owner, Mode::NativeOnly {}).unwrap();
                    }
                    let store = ready.then(|| Store::open(&owner, witness(&owner)).unwrap());
                    let path = owner.root.path.join(name);
                    match object {
                        "file" => {
                            std::fs::write(&path, b"preserve this evidence").unwrap();
                            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                                .unwrap();
                        }
                        "directory" => std::fs::create_dir(&path).unwrap(),
                        _ => symlink("missing-original", &path).unwrap(),
                    }
                    let inventory = || {
                        let mut names = std::fs::read_dir(&owner.root.path)
                            .unwrap()
                            .map(|e| e.unwrap().file_name())
                            .collect::<Vec<_>>();
                        names.sort();
                        names
                    };
                    let before = inventory();
                    assert!(
                        initialize(&owner, Mode::NativeOnly {}).is_err(),
                        "{ready}/{name}/{object}"
                    );
                    assert!(Store::open(&owner, witness(&owner)).is_err());
                    if let Some(store) = store {
                        assert!(store.request("unread").is_err());
                    }
                    assert_eq!(
                        inventory(),
                        before,
                        "refusal changed {ready}/{name}/{object}"
                    );
                    match object {
                        "file" => {
                            assert_eq!(std::fs::read(path).unwrap(), b"preserve this evidence")
                        }
                        "directory" => assert!(path.is_dir()),
                        _ => assert_eq!(
                            std::fs::read_link(path).unwrap(),
                            std::path::Path::new("missing-original")
                        ),
                    }
                }
            }
        }
        let f = Fixture::new(HostRoute::Codex);
        f.complete();
        let owner = f.owner();
        initialize(&owner, Mode::NativeOnly {}).unwrap();
        initialize(&owner, Mode::NativeOnly {}).unwrap();
        assert!(Store::open(&owner, witness(&owner)).is_ok());
    }
}
