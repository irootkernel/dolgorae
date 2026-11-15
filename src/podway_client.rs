//! Pinned public Podway generation-1 invocation, byte identity and recovery.
use crate::machine::MachineError;
use crate::native_consumer::{
    Effect, Mutation, PodwayConnection, Prepared, Request, decode, encode, error, hash, invalid,
};
use crate::trusted_host::{VerifiedOwner, is_digest, refusal};
use serde_json::{Value, json};
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use uuid::Uuid;

pub(crate) fn item_id(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 64
        && text.as_bytes()[0].is_ascii_lowercase()
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !text.ends_with('-')
        && !text.contains("--")
}
pub(crate) fn validate_connection(
    owner: &VerifiedOwner,
    c: &PodwayConnection,
) -> Result<(), MachineError> {
    owner.revalidate()?;
    let caller = owner
        .configuration
        .caller_scope
        .as_ref()
        .ok_or_else(refusal)?;
    if c.workspace.to_string() != caller.podway_workspace_ref
        || c.session.to_string() != caller.podway_session_ref
        || c.attempt.to_string() != caller.podway_attempt_ref
        || c.placement != caller.delegated_placement_ref
        || !item_id(&c.placement)
        || c.adapter_id != owner.configuration.adapter_id
        || c.adapter_version != owner.configuration.adapter_version
        || !ascii_identity(&c.adapter_id)
        || !ascii_identity(&c.adapter_version)
        || !is_digest(&c.manifest_digest)
        || c.permitted_items.len() > 128
        || c.permitted_items.iter().any(|id| !item_id(id))
        || std::fs::canonicalize(&c.worktree).map_err(|_| refusal())? != c.worktree
    {
        return Err(refusal());
    }
    executable(c)?;
    Ok(())
}
fn ascii_identity(text: &str) -> bool {
    (1..=128).contains(&text.len()) && text.bytes().all(|b| (0x21..=0x7e).contains(&b))
}
fn executable(c: &PodwayConnection) -> Result<File, MachineError> {
    if !c.executable.is_absolute()
        || std::fs::canonicalize(&c.executable).map_err(|_| refusal())? != c.executable
    {
        return Err(refusal());
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&c.executable)
        .map_err(|_| refusal())?;
    let meta = file.metadata().map_err(|_| refusal())?;
    if !meta.is_file()
        || meta.len() > 256 * 1024 * 1024
        || meta.mode() & 0o022 != 0
        || meta.mode() & 0o111 == 0
    {
        return Err(refusal());
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|_| refusal())?;
    if hash(&bytes) != c.executable_sha256 {
        return Err(refusal());
    }
    Ok(file)
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ReadCommand {
    Manifest,
    Observe,
    Package,
    Outcome(String),
    Receipt,
    Evidence {
        source: String,
        item: String,
        page: Option<String>,
    },
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Invocation {
    Read(ReadCommand),
    Mutate(Mutation),
}
impl Invocation {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Mutate(m) => m.command(),
            Self::Read(c) => match c {
                ReadCommand::Manifest => "version",
                ReadCommand::Observe => "session.observe",
                ReadCommand::Package => "delegation.package",
                ReadCommand::Outcome(_) => "outcome.lookup",
                ReadCommand::Receipt => "delegation.receipt",
                ReadCommand::Evidence { .. } => "evidence.read",
            },
        }
    }
    pub(crate) fn argv(&self, c: &PodwayConnection) -> Vec<String> {
        let mut a = vec![
            "--contract-generation".into(),
            "1".into(),
            "--worktree".into(),
            c.worktree.to_string_lossy().into_owned(),
            "--json".into(),
        ];
        match self {
            Self::Mutate(Mutation::Record) => a.extend(["record".into(), "--stdin".into()]),
            Self::Mutate(Mutation::Accept) => {
                a.extend(["delegation".into(), "accept".into(), "--stdin".into()])
            }
            Self::Read(ReadCommand::Manifest) => {
                a = vec![
                    "--contract-generation".into(),
                    "1".into(),
                    "--json".into(),
                    "version".into(),
                    "--identity".into(),
                ];
            }
            Self::Read(ReadCommand::Observe) => a.push("observe".into()),
            Self::Read(ReadCommand::Package) => a.extend(["delegation".into(), "package".into()]),
            Self::Read(ReadCommand::Outcome(key)) => a.extend([
                "--idempotency-key".into(),
                key.clone(),
                "outcome".into(),
                "lookup".into(),
            ]),
            Self::Read(ReadCommand::Receipt) => a.extend([
                "delegation".into(),
                "receipt".into(),
                "--session".into(),
                c.session.to_string(),
                "--attempt".into(),
                c.attempt.to_string(),
            ]),
            Self::Read(ReadCommand::Evidence { source, item, page }) => {
                a.extend([
                    "evidence".into(),
                    "read".into(),
                    "--source".into(),
                    source.clone(),
                    "--item".into(),
                    item.clone(),
                ]);
                if let Some(page) = page {
                    a.extend(["--page-token".into(), page.clone()]);
                }
            }
        }
        a
    }
}
#[derive(Debug)]
pub(crate) struct Reply {
    pub value: Value,
    pub success: bool,
}
/// Offline tests substitute only the child boundary; production input/association
/// code remains the same. No lifecycle or arbitrary argv is expressible here.
pub(crate) trait Client {
    fn invoke(
        &mut self,
        c: &PodwayConnection,
        invocation: Invocation,
        input: &[u8],
        launched: &mut dyn FnMut(u32) -> Result<(), MachineError>,
    ) -> Result<Reply, MachineError>;
}
pub(crate) struct PublicCli;
struct Capture {
    root: PathBuf,
}
impl Drop for Capture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
impl Client for PublicCli {
    fn invoke(
        &mut self,
        c: &PodwayConnection,
        invocation: Invocation,
        input: &[u8],
        launched: &mut dyn FnMut(u32) -> Result<(), MachineError>,
    ) -> Result<Reply, MachineError> {
        let _binary = executable(c)?;
        if input.len() > crate::native_consumer::MAX_INPUT {
            return Err(invalid());
        }
        let path = std::env::temp_dir().join(format!("dolgorae-consumer-{}", Uuid::now_v7()));
        std::fs::create_dir(&path).map_err(|_| refusal())?;
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| refusal())?;
        let capture = Capture {
            root: std::fs::canonicalize(path).map_err(|_| refusal())?,
        };
        let open = |name| {
            OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(capture.root.join(name))
                .map_err(|_| refusal())
        };
        let mut stdin = open("input")?;
        stdin.write_all(input).map_err(|_| refusal())?;
        use std::io::Seek as _;
        stdin.rewind().map_err(|_| refusal())?;
        let stdout = open("output")?;
        let stderr = open("error")?;
        let mut child = Command::new(&c.executable)
            .args(invocation.argv(c))
            .current_dir(&c.worktree)
            .stdin(stdin)
            .stdout(Stdio::from(stdout.try_clone().map_err(|_| refusal())?))
            .stderr(Stdio::from(stderr.try_clone().map_err(|_| refusal())?))
            .spawn()
            .map_err(|_| error("consumer_rejection", "spawn", Effect::Unknown))?;
        let uncertain = || error("uncertain_outcome", "child", Effect::Unknown);
        if launched(child.id()).is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(uncertain());
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(uncertain());
                }
            }
            if Instant::now() >= deadline
                || stdout.metadata().map_err(|_| uncertain())?.len() > 4 * 1024 * 1024
                || stderr.metadata().map_err(|_| uncertain())?.len() > 1024 * 1024
            {
                let _ = child.kill();
                let _ = child.wait();
                return Err(uncertain());
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        // Exit code alone cannot establish a semantic outcome.
        let mut bytes = Vec::new();
        File::open(capture.root.join("output"))
            .map_err(|_| uncertain())?
            .take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| uncertain())?;
        if bytes.len() > 4 * 1024 * 1024 {
            return Err(uncertain());
        }
        parse_reply(&bytes, status.code(), invocation.name(), c)
    }
}
pub(crate) fn parse_reply(
    bytes: &[u8],
    exit: Option<i32>,
    command: &str,
    c: &PodwayConnection,
) -> Result<Reply, MachineError> {
    let v: Value = decode(bytes, 4 * 1024 * 1024)
        .map_err(|_| error("uncertain_outcome", "decode_reply", Effect::Unknown))?;
    let success = v["schema"] == "podway.output/v4";
    let allowed = if success {
        vec![
            "schema",
            "request_id",
            "command",
            "generated_at",
            "workspace",
            "session",
            "result",
            "warnings",
        ]
    } else {
        vec![
            "schema",
            "request_id",
            "command",
            "generated_at",
            "workspace",
            "code",
            "message",
            "retryable",
            "exit_code",
            "details",
        ]
    };
    if v.as_object()
        .is_none_or(|o| o.keys().any(|k| !allowed.contains(&k.as_str())))
        || v["command"] != command
        || v["request_id"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .is_none()
        || v["generated_at"].as_str().is_none()
        || (success && (exit != Some(0) || !v["result"].is_object() || !v["warnings"].is_array()))
        || (!success
            && (v["schema"] != "podway.error/v2"
                || !matches!(exit, Some(1 | 2 | 4 | 5 | 6))
                || v["exit_code"].as_i64() != exit.map(i64::from)
                || v["code"].as_str().is_none()
                || v["message"].as_str().is_none()
                || v["retryable"].as_bool().is_none()
                || !v["details"].is_object()))
        || (success && command != "version" && v["workspace"]["uuid"] != c.workspace.to_string())
        || (command == "version" && v.get("workspace").is_some_and(|w| !w.is_null()))
        || v.get("workspace")
            .is_some_and(|w| !w.is_null() && w["uuid"] != c.workspace.to_string())
    {
        return Err(error("uncertain_outcome", "decode_reply", Effect::Unknown));
    }
    Ok(Reply { value: v, success })
}
/// Selected Podway Canonical JSON v1: integer-only sorted UTF-8 keys.
/// This is deliberately not the public Dolgorae JCS digest domain.
pub(crate) fn request_digest(m: Mutation, input: &[u8]) -> Result<String, MachineError> {
    let v: Value = decode(input, crate::native_consumer::MAX_INPUT)?;
    let pre = json!({"session_id":v["session_id"],"session_revision":v["session_revision"],"attempt_id":v["attempt_id"]});
    let payload = match m {
        Mutation::Accept => v.clone(),
        Mutation::Record => json!({"operations":v["operations"]}),
    };
    Ok(hash(&encode(
        &json!({"protocol_major":1,"command":m.command(),"workspace_id":v["workspace_uuid"],"preconditions":pre,"payload":payload}),
    )?))
}
pub(crate) fn verify_success(p: &Prepared, value: &Value) -> Result<(), MachineError> {
    let input: Value = decode(&p.input, crate::native_consumer::MAX_INPUT)?;
    let before = input["session_revision"].as_u64().ok_or_else(invalid)?;
    let after = value["session"]["revision_after"]
        .as_u64()
        .ok_or_else(invalid)?;
    if value["schema"] != "podway.output/v4"
        || value["command"] != p.mutation.command()
        || value.as_object().is_none_or(|o| o.len() != 8)
        || value["request_id"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .is_none()
        || value["generated_at"].as_str().is_none_or(str::is_empty)
        || !value["warnings"].is_array()
        || value["workspace"]["uuid"] != input["workspace_uuid"]
        || value["workspace"]["root"]
            .as_str()
            .is_none_or(|s| !s.starts_with('/'))
        || value["workspace"]["latest_workspace_sequence"]
            .as_u64()
            .is_none()
        || value["session"]["id"] != input["session_id"]
        || value["session"]["title"]
            .as_str()
            .is_none_or(|s| s.chars().count() > 256)
        || value["session"]["lifecycle"] != "running"
        || value["session"]["revision_before"].as_u64() != Some(before)
    {
        return Err(invalid());
    }
    let r = &value["result"];
    if r["attempt_id"] != input["attempt_id"] {
        return Err(invalid());
    }
    match p.mutation {
        Mutation::Accept => {
            let expected = before.checked_add(1).ok_or_else(invalid)?;
            let revision = r["session_revision"].as_u64().ok_or_else(invalid)?;
            if r.as_object().is_none_or(|o| o.len() != 9)
                || after != expected
                || revision != expected
                || r["schema"] != "podway.delegation-accept-result/v2"
                || r["acceptance_state"] != "accepted"
                || r["receipt_id"]
                    .as_str()
                    .and_then(|s| Uuid::parse_str(s).ok())
                    .is_none()
                || [
                    "workspace_uuid",
                    "session_id",
                    "package_digest",
                    "result_digest",
                ]
                .iter()
                .any(|k| r[*k] != input[*k])
            {
                return Err(invalid());
            }
        }
        Mutation::Record => {
            let ops = input["operations"].as_array().ok_or_else(invalid)?;
            let items = r["items"].as_array().ok_or_else(invalid)?;
            let changed = r["changed"].as_bool().ok_or_else(invalid)?;
            let revision = r["revision"]
                .as_u64()
                .filter(|n| *n > 0)
                .ok_or_else(invalid)?;
            let attempt = p.preimage["current"]["attempt"]["attempt_number"]
                .as_u64()
                .filter(|n| *n > 0)
                .ok_or_else(invalid)?;
            let placement = p.preimage["current"]["node"]["graph_node_id"]
                .as_str()
                .filter(|v| item_id(v))
                .ok_or_else(invalid)?;
            let expected = if changed {
                before.checked_add(1).ok_or_else(invalid)?
            } else {
                before
            };
            if r.as_object().is_none_or(|o| o.len() != 7)
                || r["schema"] != "podway.item-record-many-result/v2"
                || r["graph_node_id"] != placement
                || r["attempt_number"].as_u64() != Some(attempt)
                || items.len() != ops.len()
                || !(1..=128).contains(&items.len())
                || revision != expected
                || after != expected
            {
                return Err(invalid());
            }
            let mut any_changed = false;
            for (op, item) in ops.iter().zip(items) {
                let previous = op["expected_item_revision"].as_u64().ok_or_else(invalid)?;
                let item_changed = item["changed"].as_bool().ok_or_else(invalid)?;
                let next = item["item_revision"].as_u64().ok_or_else(invalid)?;
                let expected = if item_changed {
                    previous.checked_add(1).ok_or_else(invalid)?
                } else {
                    previous
                };
                if item.as_object().is_none_or(|o| {
                    o.keys().any(|k| {
                        ![
                            "item_id",
                            "expected_item_revision",
                            "changed",
                            "item_revision",
                            "value_digest",
                        ]
                        .contains(&k.as_str())
                    })
                }) || op["item_id"] != item["item_id"]
                    || item["expected_item_revision"].as_u64() != Some(previous)
                    || next != expected
                    || item
                        .get("value_digest")
                        .is_some_and(|v| v.as_str().is_none_or(|s| !is_digest(s)))
                {
                    return Err(invalid());
                }
                any_changed |= item_changed;
            }
            if changed != any_changed {
                return Err(invalid());
            }
        }
    }
    Ok(())
}
/// Missing/expired lookups preserve uncertainty. A found response must match the
/// selected semantic request digest as well as its original key and result.
pub(crate) fn outcome(r: &Request, reply: &Reply) -> Result<Option<Value>, MachineError> {
    if !reply.success {
        return Err(error("post_read_failure", "outcome", r.effect));
    }
    let result = &reply.value["result"];
    if result["schema"] != "podway.outcome-lookup-result/v1" || result["idempotency_key"] != r.key {
        return Err(invalid());
    }
    match result["found"].as_bool() {
        Some(false) if result["outcome"].is_null() => Ok(None),
        Some(true) => {
            let p = r.prepared.as_ref().ok_or_else(invalid)?;
            let o = &result["outcome"];
            if o["scope"] != "session" || o["request_digest"] != p.request_digest {
                return Err(invalid());
            }
            verify_success(p, &o["response"])?;
            Ok(Some(o["response"].clone()))
        }
        _ => Err(invalid()),
    }
}
pub(crate) fn receipt(
    r: &Request,
    c: &PodwayConnection,
    reply: &Reply,
) -> Result<(Value, String), MachineError> {
    if !reply.success {
        return Err(error("post_read_failure", "receipt", r.effect));
    }
    let result = &reply.value["result"];
    let receipt = &result["receipt"];
    let p = r.prepared.as_ref().ok_or_else(invalid)?;
    let input: Value = decode(&p.input, 32768)?;
    if result["schema"] != "podway.delegation-receipt-result/v1"
        || result["session_id"] != c.session.to_string()
        || result["attempt_id"] != c.attempt.to_string()
        || receipt["schema"] != "podway.delegation-receipt/v1"
        || receipt["placement_id"] != c.placement
        || receipt["accepted"] != true
        || [
            "workspace_uuid",
            "session_id",
            "attempt_id",
            "package_digest",
            "execution",
            "rounds_used",
            "result_digest",
            "summary",
            "worker_disposition",
        ]
        .iter()
        .any(|k| receipt[*k] != input[*k])
        || (r.state == crate::native_consumer::State::Committed
            && r.outcome
                .as_ref()
                .is_some_and(|v| v["result"]["receipt_id"] != receipt["receipt_id"]))
        || receipt["accepted_items_digest"]
            .as_str()
            .is_none_or(|s| !is_digest(s))
    {
        return Err(invalid());
    }
    let applicability = result["applicability"].as_str().ok_or_else(invalid)?;
    if !matches!(
        (applicability, result["reason"].as_str()),
        ("accepted", Some("current_receipt"))
            | (
                "stale_acceptance",
                Some("package_changed" | "items_changed")
            )
            | ("historical", Some("historical_attempt"))
    ) {
        return Err(invalid());
    }
    Ok((reply.value.clone(), applicability.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn connection() -> PodwayConnection {
        PodwayConnection {
            executable: "/selected/podway".into(),
            executable_sha256: hash(b"binary"),
            manifest_digest: hash(b"manifest"),
            worktree: "/selected/worktree".into(),
            workspace: Uuid::now_v7(),
            session: Uuid::now_v7(),
            attempt: Uuid::now_v7(),
            placement: "review".into(),
            adapter_id: "qualified-adapter".into(),
            adapter_version: "1".into(),
            permitted_items: Default::default(),
        }
    }
    fn prepared() -> Prepared {
        let input=encode(&json!({"schema":"podway.delegation-accept-input/v1","workspace_uuid":"00000000-0000-4000-8000-000000000001","session_id":"00000000-0000-4000-8000-000000000002",
            "session_revision":5,"attempt_id":"00000000-0000-4000-8000-000000000003","package_digest":hash(b"package"),"execution":{"task_ref":"original-task"},
            "idempotency_key":"original-key","rounds_used":1,"result_digest":hash(b"result"),"summary":"Accepted","worker_disposition":"idle","accepted":true})).unwrap();
        Prepared {
            mutation: Mutation::Accept,
            request_digest: request_digest(Mutation::Accept, &input).unwrap(),
            input,
            preimage: json!({}),
            evidence_ref: "e".into(),
            selection_ref: "s".into(),
        }
    }
    fn success(p: &Prepared) -> Value {
        let i: Value = decode(&p.input, 32768).unwrap();
        json!({"schema":"podway.output/v4","command":"delegation.accept","request_id":Uuid::now_v7(),"generated_at":"2026-10-27T00:00:00Z","warnings":[],
            "workspace":{"uuid":i["workspace_uuid"],"root":"/selected/worktree","latest_workspace_sequence":8},"session":{"id":i["session_id"],"title":"Work","lifecycle":"running","revision_before":i["session_revision"],"revision_after":6},"result":{"schema":"podway.delegation-accept-result/v2","receipt_id":Uuid::now_v7(),"workspace_uuid":i["workspace_uuid"],"session_id":i["session_id"],
            "session_revision":6,"attempt_id":i["attempt_id"],"package_digest":i["package_digest"],"result_digest":i["result_digest"],"acceptance_state":"accepted"}})
    }
    fn request(p: Prepared) -> Request {
        use crate::native_consumer::{Action, State};
        Request {
            request_id: "r".into(),
            intent: Action::PrepareAccept {
                request_id: "r".into(),
                evidence_ref: "e".into(),
                summary: "Accepted".into(),
            },
            key: "original-key".into(),
            state: State::Uncertain,
            prepared: Some(p),
            witness: None,
            claim: None,
            child: None,
            effect: Effect::Unknown,
            outcome: None,
            receipt: None,
            applicability: None,
        }
    }
    #[test]
    fn pinned_generation_and_exact_selected_argv_have_no_invented_flags() {
        let c = connection();
        for invocation in [
            Invocation::Mutate(Mutation::Accept),
            Invocation::Mutate(Mutation::Record),
            Invocation::Read(ReadCommand::Observe),
            Invocation::Read(ReadCommand::Package),
            Invocation::Read(ReadCommand::Receipt),
            Invocation::Read(ReadCommand::Outcome("original-key".into())),
        ] {
            let a = invocation.argv(&c);
            assert_eq!(&a[..2], ["--contract-generation", "1"]);
            assert_eq!(
                &a[2..5],
                ["--worktree", c.worktree.to_str().unwrap(), "--json"]
            );
            assert!(
                !a.iter()
                    .any(|a| a == "--attempt-id" || a.starts_with("--if-"))
            );
        }
        assert!(Invocation::Read(ReadCommand::Receipt).argv(&c).ends_with(&[
            "--session".into(),
            c.session.to_string(),
            "--attempt".into(),
            c.attempt.to_string()
        ]));
        assert!(
            Invocation::Read(ReadCommand::Outcome("original-key".into()))
                .argv(&c)
                .ends_with(&[
                    "--idempotency-key".into(),
                    "original-key".into(),
                    "outcome".into(),
                    "lookup".into()
                ])
        );
        assert_eq!(
            Invocation::Read(ReadCommand::Manifest).argv(&c),
            [
                "--contract-generation",
                "1",
                "--json",
                "version",
                "--identity"
            ]
        );
    }
    #[test]
    fn reply_parser_distinguishes_success_rejection_and_ambiguous_output() {
        let p = prepared();
        let success = success(&p);
        let mut c = connection();
        c.workspace = Uuid::parse_str(success["workspace"]["uuid"].as_str().unwrap()).unwrap();
        assert!(
            parse_reply(&encode(&success).unwrap(), Some(0), "delegation.accept", &c)
                .unwrap()
                .success
        );
        for (bytes, exit) in [
            (b"not-json".to_vec(), Some(0)),
            (encode(&success).unwrap(), Some(1)),
            (encode(&success).unwrap(), None),
        ] {
            assert!(parse_reply(&bytes, exit, "delegation.accept", &c).is_err());
        }
        for field in [
            "schema",
            "request_id",
            "command",
            "generated_at",
            "result",
            "warnings",
        ] {
            let mut bad = success.clone();
            bad.as_object_mut().unwrap().remove(field);
            assert!(
                parse_reply(&encode(&bad).unwrap(), Some(0), "delegation.accept", &c).is_err(),
                "{field}"
            );
        }
        let rejected = json!({"schema":"podway.error/v2","request_id":Uuid::now_v7(),"command":"delegation.accept","generated_at":"2026-10-27T00:00:00Z","code":"SESSION_REVISION_CONFLICT","message":"stale","retryable":true,"exit_code":4,"details":{}});
        assert!(
            !parse_reply(
                &encode(&rejected).unwrap(),
                Some(4),
                "delegation.accept",
                &c
            )
            .unwrap()
            .success
        );
    }
    #[test]
    fn found_outcome_requires_original_key_semantic_digest_and_result_association() {
        let p = prepared();
        let response = success(&p);
        verify_success(&p, &response).unwrap();
        let r = request(p.clone());
        let value = json!({"result":{"schema":"podway.outcome-lookup-result/v1","idempotency_key":r.key,"found":true,"outcome":{"request_digest":p.request_digest,"recorded_at":"2026-10-27T00:00:00Z","scope":"session","response":response}}});
        assert!(
            outcome(
                &r,
                &Reply {
                    success: true,
                    value: value.clone()
                }
            )
            .unwrap()
            .is_some()
        );
        for pointer in [
            "/result/idempotency_key",
            "/result/outcome/request_digest",
            "/result/outcome/scope",
            "/result/outcome/response/result/attempt_id",
            "/result/outcome/response/result/result_digest",
            "/result/outcome/response/result/package_digest",
            "/result/outcome/response/result/session_id",
            "/result/outcome/response/result/workspace_uuid",
            "/result/outcome/response/result/session_revision",
        ] {
            let mut bad = value.clone();
            *bad.pointer_mut(pointer).unwrap() = json!("foreign");
            assert!(
                outcome(
                    &r,
                    &Reply {
                        success: true,
                        value: bad
                    }
                )
                .is_err(),
                "{pointer}"
            );
        }
        let missing = json!({"result":{"schema":"podway.outcome-lookup-result/v1","idempotency_key":r.key,"found":false,"outcome":null}});
        assert_eq!(
            outcome(
                &r,
                &Reply {
                    success: true,
                    value: missing
                }
            )
            .unwrap(),
            None
        );
        assert!(
            outcome(
                &r,
                &Reply {
                    success: false,
                    value: json!({"code":"expired"})
                }
            )
            .is_err()
        );
    }
    #[test]
    fn receipt_binds_all_frozen_inputs_and_preserves_historical_applicability() {
        let p = prepared();
        let input: Value = decode(&p.input, 32768).unwrap();
        let mut r = request(p);
        let mut c = connection();
        c.workspace = Uuid::parse_str(input["workspace_uuid"].as_str().unwrap()).unwrap();
        c.session = Uuid::parse_str(input["session_id"].as_str().unwrap()).unwrap();
        c.attempt = Uuid::parse_str(input["attempt_id"].as_str().unwrap()).unwrap();
        let mut accepted = input.clone();
        accepted.as_object_mut().unwrap().remove("idempotency_key");
        accepted.as_object_mut().unwrap().remove("session_revision");
        accepted["schema"] = json!("podway.delegation-receipt/v1");
        accepted["receipt_id"] = json!(Uuid::now_v7());
        accepted["placement_id"] = json!(c.placement);
        accepted["accepted_items_digest"] = json!(hash(b"items"));
        let mut response = success(r.prepared.as_ref().unwrap());
        response["result"]["receipt_id"] = accepted["receipt_id"].clone();
        r.outcome = Some(response);
        r.state = crate::native_consumer::State::Committed;
        let value = json!({"result":{"schema":"podway.delegation-receipt-result/v1","session_id":c.session,"attempt_id":c.attempt,"receipt":accepted,"applicability":"historical","reason":"historical_attempt"}});
        assert_eq!(
            receipt(
                &r,
                &c,
                &Reply {
                    success: true,
                    value: value.clone()
                }
            )
            .unwrap()
            .1,
            "historical"
        );
        for field in [
            "workspace_uuid",
            "session_id",
            "attempt_id",
            "placement_id",
            "package_digest",
            "execution",
            "rounds_used",
            "result_digest",
            "summary",
            "worker_disposition",
            "receipt_id",
            "accepted_items_digest",
        ] {
            let mut bad = value.clone();
            bad["result"]["receipt"][field] = json!("foreign");
            assert!(
                receipt(
                    &r,
                    &c,
                    &Reply {
                        success: true,
                        value: bad
                    }
                )
                .is_err(),
                "{field}"
            );
        }
        assert!(
            receipt(
                &r,
                &c,
                &Reply {
                    success: false,
                    value: json!({})
                }
            )
            .is_err()
        );
    }
    #[test]
    fn integer_record_stdin_and_semantic_digest_preserve_full_domain() {
        let input=encode(&json!({"workspace_uuid":"w","session_id":"s","session_revision":u64::MAX,"attempt_id":"a","idempotency_key":"k",
            "operations":[{"item_id":"value","expected_item_revision":u64::MAX,"record":{"type":"integer","value":i64::MIN}}]})).unwrap();
        let text = std::str::from_utf8(&input).unwrap();
        assert!(text.contains("18446744073709551615"));
        assert!(text.contains("-9223372036854775808"));
        let actual = request_digest(Mutation::Record, &input).unwrap();
        // Literal independently reconstructed from selected slice.rs:1347-1471.
        let known = concat!(
            "{\"command\":\"item.record_many\",\"payload\":{\"operations\":[{\"expected_item_revision\":18446744073709551615,\"item_id\":\"value\",\"record\":{\"type\":\"integer\",\"value\":-9223372036854775808}}]},",
            "\"preconditions\":{\"attempt_id\":\"a\",\"session_id\":\"s\",\"session_revision\":18446744073709551615},\"protocol_major\":1,\"workspace_id\":\"w\"}"
        );
        assert_eq!(actual, hash(known.as_bytes()));
    }
    #[test]
    fn local_identity_null_workspace_and_all_selected_error_exit_classes() {
        let c = connection();
        let base = json!({"schema":"podway.output/v4","request_id":Uuid::now_v7(),"command":"version","generated_at":"2026-10-27T00:00:00Z", "result":{"schema":"podway.version-result/v2","product":"podway","version":"v0.2.15","target":"offline-test","build_identity":hash(b"selected image"),"source_commit":null,"contract_manifest_schema":"podway.contract-manifest/v2","contract_manifest_digest":c.manifest_digest},"warnings":[]});
        for explicit_null in [false, true] {
            let mut value = base.clone();
            if explicit_null {
                value["workspace"] = Value::Null;
                value["session"] = Value::Null;
            }
            assert!(
                parse_reply(&encode(&value).unwrap(), Some(0), "version", &c)
                    .unwrap()
                    .success
            );
            value["command"] = json!("session.observe");
            assert!(parse_reply(&encode(&value).unwrap(), Some(0), "session.observe", &c).is_err());
        }
        for (exit, code) in [
            (1, "SESSION_NOT_RUNNING"),
            (2, "REQUEST_INVALID"),
            (4, "SESSION_REVISION_CONFLICT"),
            (5, "PATH_OUTSIDE_WORKTREE"),
            (6, "INTERNAL_ERROR"),
        ] {
            let value = json!({"schema":"podway.error/v2","request_id":Uuid::now_v7(),"command":"session.observe","generated_at":"2026-10-27T00:00:00Z","code":code,"message":"refused","retryable":false,"exit_code":exit,"details":{},"workspace":null});
            assert!(
                !parse_reply(&encode(&value).unwrap(), Some(exit), "session.observe", &c)
                    .unwrap()
                    .success
            );
        }
        assert!(parse_reply(b"", Some(6), "version", &c).is_err());
    }
    fn record_prepared() -> Prepared {
        let mut p = prepared();
        let mut input: Value = decode(&p.input, 32768).unwrap();
        input = json!({"schema":"podway.item-record-many-input/v1","workspace_uuid":input["workspace_uuid"],"session_id":input["session_id"],"session_revision":5,"attempt_id":input["attempt_id"],"idempotency_key":input["idempotency_key"],"operations":[{"item_id":"summary","expected_item_revision":2,"record":{"type":"text","value":"Verified."}}]});
        p.mutation = Mutation::Record;
        p.input = encode(&input).unwrap();
        p.request_digest = request_digest(p.mutation, &p.input).unwrap();
        p.preimage =
            json!({"current":{"node":{"graph_node_id":"review"},"attempt":{"attempt_number":3}}});
        p
    }
    fn record_success(p: &Prepared) -> Value {
        let mut value = success(p);
        let input: Value = decode(&p.input, 32768).unwrap();
        value["command"] = json!("item.record_many");
        value["result"] = json!({"schema":"podway.item-record-many-result/v2","changed":true,"graph_node_id":"review","attempt_id":input["attempt_id"],"attempt_number":3,"revision":6,
            "items":[{"item_id":"summary","expected_item_revision":2,"changed":true,"item_revision":3,"value_digest":hash(b"value")}]});
        value
    }
    fn recovered_response(r: &Request, response: Value) -> Reply {
        Reply {
            success: true,
            value: json!({"result":{"schema":"podway.outcome-lookup-result/v1","idempotency_key":r.key,"found":true,"outcome":{"scope":"session","request_digest":r.prepared.as_ref().unwrap().request_digest,"recorded_at":"2026-10-27T00:00:00Z","response":response}}}),
        }
    }
    #[test]
    fn selected_record_and_accept_success_require_every_result_field_and_finite_revisions() {
        for p in [record_prepared(), prepared()] {
            let good = if p.mutation == Mutation::Record {
                record_success(&p)
            } else {
                success(&p)
            };
            verify_success(&p, &good).unwrap();
            let r = request(p.clone());
            assert!(
                outcome(&r, &recovered_response(&r, good.clone()))
                    .unwrap()
                    .is_some()
            );
            for field in good["result"].as_object().unwrap().keys() {
                for bad in [Value::Null, json!("wrong-type")] {
                    let mut value = good.clone();
                    value["result"][field] = bad;
                    assert!(
                        verify_success(&p, &value).is_err(),
                        "{field}/{:?}",
                        p.mutation
                    );
                    assert!(outcome(&r, &recovered_response(&r, value)).is_err());
                }
                let mut value = good.clone();
                value["result"].as_object_mut().unwrap().remove(field);
                assert!(verify_success(&p, &value).is_err(), "missing {field}");
                assert!(outcome(&r, &recovered_response(&r, value)).is_err());
            }
        }
        let p = record_prepared();
        let good = record_success(&p);
        for (field, bad) in [
            ("graph_node_id", json!("foreign")),
            ("attempt_number", json!(0)),
            ("attempt_number", json!(4)),
            ("revision", json!(7)),
        ] {
            let mut value = good.clone();
            value["result"][field] = bad;
            assert!(verify_success(&p, &value).is_err());
        }
        for field in [
            "item_id",
            "expected_item_revision",
            "changed",
            "item_revision",
        ] {
            let mut value = good.clone();
            value["result"]["items"][0]
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(verify_success(&p, &value).is_err(), "item.{field}");
        }
        for changed in [false, true] {
            let mut p = record_prepared();
            let mut input: Value = decode(&p.input, 32768).unwrap();
            let before = if changed { u64::MAX - 1 } else { u64::MAX };
            input["session_revision"] = json!(before);
            input["operations"][0]["expected_item_revision"] = json!(before);
            p.input = encode(&input).unwrap();
            p.preimage["current"]["attempt"]["attempt_number"] = json!(u64::MAX);
            let mut value = record_success(&p);
            value["session"]["revision_before"] = json!(before);
            value["session"]["revision_after"] = json!(u64::MAX);
            value["result"]["revision"] = json!(u64::MAX);
            value["result"]["attempt_number"] = json!(u64::MAX);
            value["result"]["changed"] = json!(changed);
            value["result"]["items"][0]["expected_item_revision"] = json!(before);
            value["result"]["items"][0]["item_revision"] = json!(u64::MAX);
            value["result"]["items"][0]["changed"] = json!(changed);
            verify_success(&p, &value).unwrap();
            let r = request(p.clone());
            assert!(
                outcome(&r, &recovered_response(&r, value.clone()))
                    .unwrap()
                    .is_some()
            );
            if !changed {
                value["result"]["changed"] = json!(true);
                value["result"]["items"][0]["changed"] = json!(true);
                assert!(verify_success(&p, &value).is_err());
                assert!(outcome(&r, &recovered_response(&r, value)).is_err());
            }
        }
        for before in [u64::MAX - 1, u64::MAX] {
            let mut p = prepared();
            let mut input: Value = decode(&p.input, 32768).unwrap();
            input["session_revision"] = json!(before);
            p.input = encode(&input).unwrap();
            let mut value = success(&p);
            value["session"]["revision_before"] = json!(before);
            value["session"]["revision_after"] =
                before.checked_add(1).map_or(Value::Null, |n| json!(n));
            value["result"]["session_revision"] = value["session"]["revision_after"].clone();
            assert_eq!(verify_success(&p, &value).is_ok(), before != u64::MAX);
            value["session"]["revision_after"] = Value::Null;
            value["result"]["session_revision"] = Value::Null;
            assert!(verify_success(&p, &value).is_err());
        }
    }
    #[test]
    fn selected_read_routes_and_evidence_argv_match_source_known_answers() {
        let c = connection();
        // Selected command/grammar.rs canonical names and help argv, read
        // independently of Invocation's implementation.
        for (invocation, name, tail) in [
            (
                Invocation::Read(ReadCommand::Observe),
                "session.observe",
                vec!["observe".to_owned()],
            ),
            (
                Invocation::Read(ReadCommand::Package),
                "delegation.package",
                vec!["delegation".into(), "package".into()],
            ),
            (
                Invocation::Read(ReadCommand::Receipt),
                "delegation.receipt",
                vec![
                    "delegation".into(),
                    "receipt".into(),
                    "--session".into(),
                    c.session.to_string(),
                    "--attempt".into(),
                    c.attempt.to_string(),
                ],
            ),
            (
                Invocation::Read(ReadCommand::Outcome("original-key".into())),
                "outcome.lookup",
                vec![
                    "--idempotency-key".into(),
                    "original-key".into(),
                    "outcome".into(),
                    "lookup".into(),
                ],
            ),
            (
                Invocation::Mutate(Mutation::Record),
                "item.record_many",
                vec!["record".into(), "--stdin".into()],
            ),
            (
                Invocation::Mutate(Mutation::Accept),
                "delegation.accept",
                vec!["delegation".into(), "accept".into(), "--stdin".into()],
            ),
        ] {
            assert_eq!(invocation.name(), name);
            assert_eq!(&invocation.argv(&c)[5..], tail);
        }
        let evidence = Invocation::Read(ReadCommand::Evidence {
            source: "producer".into(),
            item: "report".into(),
            page: Some("page-token".into()),
        });
        assert_eq!(evidence.name(), "evidence.read");
        assert_eq!(
            &evidence.argv(&c)[5..],
            [
                "evidence",
                "read",
                "--source",
                "producer",
                "--item",
                "report",
                "--page-token",
                "page-token"
            ]
        );
        let observation = Invocation::Read(ReadCommand::Observe);
        assert_eq!(observation.name(), "session.observe");
        let value = json!({"schema":"podway.output/v4","request_id":Uuid::now_v7(),"command":"session.observe","generated_at":"2026-10-27T00:00:00Z","workspace":{"uuid":c.workspace},"result":{"schema":"podway.observation-result/v5"},"warnings":[]});
        assert!(parse_reply(&encode(&value).unwrap(), Some(0), observation.name(), &c).is_ok());
        let mut old = value;
        old["command"] = json!("observe");
        assert!(parse_reply(&encode(&old).unwrap(), Some(0), observation.name(), &c).is_err());
    }
}
