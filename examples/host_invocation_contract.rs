//! Account-free pre-contract identity prototype; not a production MCP facade.

use dolgorae::jcs;
use dolgorae::workspace::{SystemWorkspacePlatform, WorkspacePlatform as _};
use rusqlite::{Connection, OpenFlags, OptionalExtension as _, TransactionBehavior, params};
use serde_json::{Value, json};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead as _, Read as _, Write as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;

const PRIVATE: &str = "_dolgorae_host_binding";
const TOOL: &str = "dolgorae_identity_probe";
const HOOK_TOOL: &str = "mcp__dolgorae__dolgorae_identity_probe";
const LIMIT: u64 = 1024 * 1024;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn reject<T>(message: &'static str) -> Result<T> {
    Err(message.into())
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 4096)
        .ok_or_else(|| "required native field missing".into())
}

fn protected(path: &Path, directory: bool) -> Result<()> {
    let m = fs::symlink_metadata(path)?;
    let expected = if directory { 0o700 } else { 0o600 };
    if m.uid() != SystemWorkspacePlatform.current_uid()
        || m.permissions().mode() & 0o777 != expected
        || (directory && !m.is_dir())
        || (!directory && !m.is_file())
        || (!directory && (m.nlink() != 1 || m.len() > 4 * LIMIT))
    {
        return reject("protected state identity invalid");
    }
    Ok(())
}

fn root_path(root: &Path) -> Result<PathBuf> {
    if !root.is_absolute() || root.canonicalize()? != root {
        return reject("state root must be canonical and absolute");
    }
    protected(root, true)?;
    Ok(root.join("invocations.sqlite"))
}

fn initialize(root: &Path, route: &str) -> Result<()> {
    if !matches!(route, "codex-0.160.0" | "claude-code-2.1.295") {
        return reject("host route unsupported");
    }
    let path = root_path(root)?;
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)?;
    file.sync_all()?;
    let db = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    db.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;
        CREATE TABLE config (singleton INTEGER PRIMARY KEY CHECK(singleton=1), route TEXT NOT NULL, generation TEXT NOT NULL);
        CREATE TABLE calls (native_key TEXT PRIMARY KEY, native_call TEXT NOT NULL, native_session TEXT NOT NULL,
            request_ref TEXT NOT NULL UNIQUE, input_digest TEXT NOT NULL,
            private_binding TEXT NOT NULL UNIQUE, status TEXT NOT NULL CHECK(status IN ('prepared','accepted')),
            receipt TEXT, UNIQUE(native_session,native_call));
        PRAGMA user_version=1;")?;
    db.execute(
        "INSERT INTO config VALUES (1,?1,?2)",
        params![route, Uuid::now_v7().to_string()],
    )?;
    File::open(root)?.sync_all()?;
    Ok(())
}

fn open(root: &Path, route: &str) -> Result<Connection> {
    let path = root_path(root)?;
    protected(&path, false)?;
    let db = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    db.busy_timeout(Duration::from_secs(3))?;
    db.execute_batch("PRAGMA synchronous=FULL;")?;
    let version: u32 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let check: String = db.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    let stored: String = db.query_row("SELECT route FROM config WHERE singleton=1", [], |r| {
        r.get(0)
    })?;
    if version != 1 || check != "ok" || stored != route {
        return reject("state generation invalid");
    }
    Ok(db)
}

fn public_digest(input: &Value) -> Result<String> {
    let object = input.as_object().ok_or("public input not an object")?;
    if object.len() != 1
        || !input
            .get("payload")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty() && s.len() <= 65536)
    {
        return reject("public input outside contract");
    }
    let encoded = serde_json::to_string(input)?;
    let canonical = jcs::canonicalize(&jcs::parse(&encoded).map_err(|_| "input invalid")?)
        .map_err(|_| "input not canonicalizable")?;
    let mut bytes = b"dolgorae.host-probe-input/v1\n".to_vec();
    bytes.extend(canonical);
    Ok(jcs::sha256_hex(&bytes))
}

fn hook(root: &Path, route: &str, event: Value) -> Result<Value> {
    if text(&event, "hook_event_name")? != "PreToolUse" || text(&event, "tool_name")? != HOOK_TOOL {
        return reject("native event binding mismatch");
    }
    if route == "claude-code-2.1.295"
        && (event.pointer("/mcp_server/name").and_then(Value::as_str) != Some("dolgorae")
            || event.pointer("/mcp_server/source").and_then(Value::as_str) != Some("dynamic"))
    {
        return reject("native server provenance mismatch");
    }
    let session = text(&event, "session_id")?;
    let call = text(&event, "tool_use_id")?;
    let turn = event.get("turn_id").and_then(Value::as_str).unwrap_or("");
    let input = event.get("tool_input").ok_or("input missing")?;
    if input.get(PRIVATE).is_some() {
        return reject("model private binding forbidden");
    }
    let digest = public_digest(input)?;
    // This prototype binds one explicitly configured stdio server and one tool.
    let key = serde_json::to_string(&json!([route, session, turn, call, "dolgorae", TOOL]))?;
    let mut db = open(root, route)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing: Option<(String, String)> = tx
        .query_row(
            "SELECT input_digest,private_binding FROM calls WHERE native_key=?1",
            [&key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let binding = if let Some((accepted, binding)) = existing {
        if accepted != digest {
            return reject("IDEMPOTENCY_CONFLICT");
        }
        binding
    } else {
        let mut random = [0_u8; 32];
        File::open("/dev/urandom")?.read_exact(&mut random)?;
        let binding = jcs::sha256_hex(&random);
        tx.execute(
            "INSERT INTO calls VALUES (?1,?2,?3,?4,?5,?6,'prepared',NULL)",
            params![
                key,
                call,
                session,
                Uuid::now_v7().to_string(),
                digest,
                binding
            ],
        )?;
        binding
    };
    tx.commit()?;
    if route == "codex-0.160.0" {
        return Ok(json!({"hookSpecificOutput":{"hookEventName":"PreToolUse",
            "permissionDecision":"allow"}}));
    }
    let mut updated = input.clone();
    updated
        .as_object_mut()
        .ok_or("input not object")?
        .insert(PRIVATE.into(), Value::String(binding));
    Ok(json!({"hookSpecificOutput":{"hookEventName":"PreToolUse",
        "permissionDecision":"allow","updatedInput":updated}}))
}

fn accept(root: &Path, route: &str, request: &Value) -> Result<Value> {
    let mut input = request
        .get("arguments")
        .cloned()
        .ok_or("arguments missing")?;
    let binding = if route == "codex-0.160.0" {
        if input.get(PRIVATE).is_some() {
            return reject("private public argument forbidden");
        }
        None
    } else {
        Some(
            input
                .as_object_mut()
                .ok_or("arguments not object")?
                .remove(PRIVATE)
                .and_then(|v| v.as_str().map(str::to_owned))
                .ok_or("trusted binding missing")?,
        )
    };
    let digest = public_digest(&input)?;
    let mut db = open(root, route)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let binding = match binding {
        Some(binding) => binding,
        None => {
            let session = request
                .pointer("/_meta/threadId")
                .and_then(Value::as_str)
                .ok_or("native session missing")?;
            let call = request
                .pointer("/_meta/callId")
                .and_then(Value::as_str)
                .ok_or("native call missing")?;
            let mut query = tx.prepare(
                "SELECT private_binding FROM calls WHERE native_session=?1 AND native_call=?2",
            )?;
            let bindings = query
                .query_map(params![session, call], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            if bindings.len() != 1 {
                return reject("native binding unavailable or ambiguous");
            }
            bindings
                .into_iter()
                .next()
                .ok_or("native binding unavailable")?
        }
    };
    let row: Option<(String,String,String,String,Option<String>)> = tx.query_row(
        "SELECT request_ref,input_digest,native_call,status,receipt FROM calls WHERE private_binding=?1", [&binding],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    let (reference, accepted, native_call, status, old_receipt) = row.ok_or("unknown binding")?;
    if accepted != digest {
        return reject("IDEMPOTENCY_CONFLICT");
    }
    if route == "codex-0.160.0"
        && request.pointer("/_meta/callId").and_then(Value::as_str) != Some(native_call.as_str())
    {
        return reject("native call metadata mismatch");
    }
    let receipt = if status == "accepted" {
        serde_json::from_str(&old_receipt.ok_or("accepted receipt missing")?)?
    } else {
        let value = json!({"schema":"dolgorae.host-probe-receipt/v1",
            "external_request_ref":reference,"input_digest":digest,"acceptances":1});
        tx.execute(
            "UPDATE calls SET status='accepted',receipt=?1 WHERE private_binding=?2",
            params![serde_json::to_string(&value)?, binding],
        )?;
        value
    };
    tx.commit()?;
    Ok(receipt)
}

fn lookup(root: &Path, route: &str, reference: &str) -> Result<Value> {
    let db = open(root, route)?;
    let row: Option<(String, Option<String>)> = db
        .query_row(
            "SELECT status,receipt FROM calls WHERE request_ref=?1",
            [reference],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match row {
        Some((status, Some(receipt))) if status == "accepted" => {
            Ok(serde_json::from_str(&receipt)?)
        }
        Some(_) => Ok(json!({"status":"unresolved_effect"})),
        // This bounded prototype has no caller-supplied retention-generation
        // fence. It cannot certify absence in a complete original namespace.
        None => reject("retained identity not found; absence is not authoritative"),
    }
}

fn lookup_native(root: &Path, route: &str, session: &str, call: &str) -> Result<Value> {
    let db = open(root, route)?;
    let reference: Option<String> = db
        .query_row(
            "SELECT request_ref FROM calls WHERE native_session=?1 AND native_call=?2",
            params![session, call],
            |r| r.get(0),
        )
        .optional()?;
    let reference =
        reference.ok_or("native identity not retained; absence is not authoritative")?;
    drop(db);
    lookup(root, route, &reference)
}

fn rpc(root: &Path, route: &str, request: Value) -> Value {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let result = match method {
        "initialize" => Ok(json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}},
            "serverInfo":{"name":"dolgorae-host-contract-prototype","version":"0.1.0"}})),
        "tools/list" => Ok(json!({"tools":[{"name":TOOL,
            "description":"Account-free identity contract probe; returns a non-secret acceptance receipt.",
            "inputSchema":{"type":"object","properties":{"payload":{"type":"string"}},
                "required":["payload"],"additionalProperties":false}}]})),
        "tools/call" if request.pointer("/params/name").and_then(Value::as_str) == Some(TOOL) => {
            accept(root, route, &request["params"]).and_then(|receipt| Ok(json!({
                "content":[{"type":"text","text":serde_json::to_string(&receipt)?}],"isError":false})))
        }
        "ping" => Ok(json!({})),
        _ => reject("request unsupported"),
    };
    match result {
        Ok(value) => json!({"jsonrpc":"2.0","id":id,"result":value}),
        Err(_) => json!({"jsonrpc":"2.0","id":id,"error":{"code":-32001,
            "message":"trusted invocation refused; retain original identity"}}),
    }
}

fn read_json() -> Result<Value> {
    let mut bytes = Vec::new();
    io::stdin().take(LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        return reject("input too large");
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn run() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() < 3 {
        return reject("expected command root route");
    }
    let root = Path::new(&args[1]);
    let route = &args[2];
    match args[0].as_str() {
        "init" => {
            initialize(root, route)?;
            println!("{{\"initialized\":true}}");
        }
        "host-hook" => {
            let response = hook(root, route, read_json()?).unwrap_or_else(|_| json!({
                "hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny",
                    "permissionDecisionReason":"trusted invocation state unavailable or conflicting"}}));
            println!("{}", serde_json::to_string(&response)?);
        }
        "host-mcp" => {
            let mut reader = io::BufReader::new(io::stdin());
            let mut output = io::stdout().lock();
            loop {
                let mut frame = Vec::new();
                let length = reader
                    .by_ref()
                    .take(LIMIT + 1)
                    .read_until(b'\n', &mut frame)?;
                if length == 0 {
                    break;
                }
                if length as u64 > LIMIT || !frame.ends_with(b"\n") {
                    return reject("frame outside bound");
                }
                let request: Value = serde_json::from_slice(&frame)?;
                if request.get("id").is_none() {
                    continue;
                }
                writeln!(
                    output,
                    "{}",
                    serde_json::to_string(&rpc(root, route, request))?
                )?;
                output.flush()?;
            }
        }
        "lookup" => {
            println!(
                "{}",
                lookup(root, route, args.get(3).ok_or("reference missing")?)?
            );
        }
        "host-lookup" => {
            println!(
                "{}",
                lookup_native(
                    root,
                    route,
                    args.get(3).ok_or("native session missing")?,
                    args.get(4).ok_or("native call missing")?
                )?
            );
        }
        _ => return reject("command unsupported"),
    }
    Ok(())
}

fn main() {
    if run().is_err() {
        eprintln!("host invocation contract probe failed; no replacement identity is authorized");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    const ROUTE: &str = "codex-0.160.0";

    struct State(PathBuf);

    impl State {
        fn new() -> Self {
            Self::for_route(ROUTE)
        }

        fn for_route(route: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("dolgorae-host-contract-{}", Uuid::now_v7()));
            fs::create_dir(&root).unwrap();
            let root = root.canonicalize().unwrap();
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
            initialize(&root, route).unwrap();
            Self(root)
        }
    }

    impl Drop for State {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn event(call: &str, payload: &str) -> Value {
        json!({"hook_event_name":"PreToolUse","tool_name":HOOK_TOOL,
            "session_id":"native-session","turn_id":"native-turn","tool_use_id":call,
            "tool_input":{"payload":payload}})
    }

    fn prepared(root: &Path, call: &str) -> Value {
        hook(root, ROUTE, event(call, "probe")).unwrap();
        json!({"name":TOOL,"arguments":{"payload":"probe"},
            "_meta":{"callId":call,"threadId":"native-session"}})
    }

    #[test]
    fn native_replay_retains_identity_and_conflicting_input_is_refused() {
        let state = State::new();
        let first = prepared(&state.0, "one");
        assert_eq!(first, prepared(&state.0, "one"));
        assert!(hook(&state.0, ROUTE, event("one", "changed")).is_err());
        let receipt = accept(&state.0, ROUTE, &first).unwrap();
        assert_eq!(receipt, accept(&state.0, ROUTE, &first).unwrap());
        assert_eq!(
            receipt,
            lookup(
                &state.0,
                ROUTE,
                receipt["external_request_ref"].as_str().unwrap()
            )
            .unwrap()
        );
        assert!(hook(&state.0, ROUTE, event("one", "changed")).is_err());
        let second = accept(&state.0, ROUTE, &prepared(&state.0, "two")).unwrap();
        assert_ne!(
            receipt["external_request_ref"],
            second["external_request_ref"]
        );
        assert!(receipt.get(PRIVATE).is_none());
    }

    #[test]
    fn concurrent_acceptance_and_lost_ack_recovery_share_one_receipt() {
        let state = State::new();
        let request = prepared(&state.0, "concurrent");
        let barrier = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let root = state.0.clone();
                let request = request.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    accept(&root, ROUTE, &request).unwrap()
                })
            })
            .collect();
        let receipts: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(receipts.iter().all(|r| r == &receipts[0]));
        // Each call reopens the database: no process-local cache repairs a lost ACK.
        assert_eq!(receipts[0], accept(&state.0, ROUTE, &request).unwrap());
        let db = open(&state.0, ROUTE).unwrap();
        let count: u32 = db
            .query_row(
                "SELECT count(*) FROM calls WHERE status='accepted'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn prepared_work_is_unknown_until_acceptance() {
        let state = State::new();
        let request = prepared(&state.0, "prepared");
        let db = open(&state.0, ROUTE).unwrap();
        let reference: String = db
            .query_row("SELECT request_ref FROM calls", [], |r| r.get(0))
            .unwrap();
        drop(db);
        assert_eq!(
            lookup(&state.0, ROUTE, &reference).unwrap()["status"],
            "unresolved_effect"
        );
        assert!(lookup(&state.0, ROUTE, &Uuid::now_v7().to_string()).is_err());
        assert_eq!(
            lookup_native(&state.0, ROUTE, "native-session", "prepared").unwrap()["status"],
            "unresolved_effect"
        );
        accept(&state.0, ROUTE, &request).unwrap();
        assert_eq!(
            lookup(&state.0, ROUTE, &reference).unwrap(),
            lookup_native(&state.0, ROUTE, "native-session", "prepared").unwrap()
        );
        assert!(lookup_native(&state.0, ROUTE, "foreign-session", "prepared").is_err());
        assert!(
            lookup(&state.0, ROUTE, &reference)
                .unwrap()
                .get("external_request_ref")
                .is_some()
        );
    }

    #[test]
    fn public_injection_and_metadata_mismatch_do_not_accept() {
        let state = State::new();
        let mut injected = event("one", "probe");
        injected["tool_input"][PRIVATE] = json!("model-selected");
        assert!(hook(&state.0, ROUTE, injected).is_err());
        let mut request = prepared(&state.0, "one");
        request["_meta"]["callId"] = json!("other");
        assert!(accept(&state.0, ROUTE, &request).is_err());
        request["_meta"]["callId"] = json!("one");
        request["arguments"]["payload"] = json!("changed");
        assert!(accept(&state.0, ROUTE, &request).is_err());
        request["arguments"]["payload"] = json!("probe");
        request.as_object_mut().unwrap().remove("_meta");
        assert!(accept(&state.0, ROUTE, &request).is_err());
        let db = open(&state.0, ROUTE).unwrap();
        let count: u32 = db
            .query_row(
                "SELECT count(*) FROM calls WHERE status='accepted'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn unavailable_or_wrong_route_state_never_becomes_authoritative_absence() {
        let state = State::new();
        assert!(open(&state.0, "claude-code-2.1.295").is_err());
        assert!(initialize(&state.0, ROUTE).is_err());
        let path = state.0.join("invocations.sqlite");
        fs::write(&path, b"corrupt").unwrap();
        assert!(lookup(&state.0, ROUTE, "unknown").is_err());
        fs::remove_file(&path).unwrap();
        assert!(lookup(&state.0, ROUTE, "unknown").is_err());
        assert!(!path.exists());
        std::os::unix::fs::symlink("missing", &path).unwrap();
        assert!(open(&state.0, ROUTE).is_err());
    }

    #[test]
    fn exact_utf8_payload_bound_is_enforced_without_normalization() {
        assert!(public_digest(&json!({"payload":"a".repeat(65536)})).is_ok());
        assert!(public_digest(&json!({"payload":"a".repeat(65537)})).is_err());
        assert!(public_digest(&json!({"payload":"é".repeat(32769)})).is_err());
        assert_ne!(
            public_digest(&json!({"payload":"probe\n"})).unwrap(),
            public_digest(&json!({"payload":"probe"})).unwrap()
        );
    }

    #[test]
    fn codex_uses_native_metadata_without_rewritten_public_arguments() {
        let state = State::new();
        let response = hook(&state.0, ROUTE, event("one", "probe")).unwrap();
        assert!(response["hookSpecificOutput"].get("updatedInput").is_none());
        let mut changed_turn = event("one", "probe");
        changed_turn["turn_id"] = json!("different-native-turn");
        assert!(hook(&state.0, ROUTE, changed_turn).is_err());
        accept(&state.0, ROUTE, &prepared(&state.0, "one")).unwrap();
    }

    #[test]
    fn claude_requires_pinned_server_provenance_and_protected_binding() {
        let route = "claude-code-2.1.295";
        let state = State::for_route(route);
        let mut native = event("one", "probe");
        assert!(hook(&state.0, route, native.clone()).is_err());
        native["mcp_server"] = json!({"name":"dolgorae","source":"dynamic"});
        let response = hook(&state.0, route, native.clone()).unwrap();
        let request =
            json!({"name":TOOL,"arguments":response["hookSpecificOutput"]["updatedInput"]});
        let receipt = accept(&state.0, route, &request).unwrap();
        assert_eq!(receipt, accept(&state.0, route, &request).unwrap());
        native["mcp_server"]["source"] = json!("project");
        assert!(hook(&state.0, route, native).is_err());
        assert!(receipt.get(PRIVATE).is_none());
    }
}
