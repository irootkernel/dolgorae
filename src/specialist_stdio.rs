//! Native hook and newline stdio MCP. No native runtime is launched here.
use crate::host_invocation::{Capability, NativeEvent};
use crate::machine::MachineError;
use crate::paths::DolgoraeHome;
use crate::specialist_facade::{self as facade, FRAME_LIMIT, NativeCarrier, PRIVATE, PUBLIC_LIMIT};
use crate::trusted_host::{HostRoute, OwnerKind, VerifiedOwner, refusal};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::io::{self, BufRead, Read, Write};
use std::path::Path;
use zeroize::Zeroize as _;
const PROTOCOL: &str = "2025-06-18";

/// Return raw member slices only after the whole document passed strict grammar.
fn members(text: &str) -> Result<Vec<(&str, &str)>, MachineError> {
    let bytes = text.as_bytes();
    if bytes.first() != Some(&b'{') || bytes.last() != Some(&b'}') {
        return Err(refusal());
    }
    let mut i = 1;
    let mut found = Vec::new();
    fn ws(bytes: &[u8], i: &mut usize) {
        while bytes.get(*i).is_some_and(u8::is_ascii_whitespace) {
            *i += 1;
        }
    }
    fn string(bytes: &[u8], i: &mut usize) -> Result<(), MachineError> {
        if bytes.get(*i) != Some(&b'"') {
            return Err(refusal());
        }
        *i += 1;
        while let Some(byte) = bytes.get(*i) {
            *i += 1;
            match byte {
                b'\\' => {
                    *i += 1;
                }
                b'"' => return Ok(()),
                _ => {}
            }
        }
        Err(refusal())
    }
    fn value(bytes: &[u8], i: &mut usize) -> Result<(), MachineError> {
        match bytes.get(*i) {
            Some(b'"') => string(bytes, i),
            Some(b'{') | Some(b'[') => {
                let mut depth = 0;
                loop {
                    match bytes.get(*i) {
                        Some(b'"') => {
                            string(bytes, i)?;
                            continue;
                        }
                        Some(b'{') | Some(b'[') => depth += 1,
                        Some(b'}') | Some(b']') => {
                            depth -= 1;
                            if depth == 0 {
                                *i += 1;
                                return Ok(());
                            }
                        }
                        None => return Err(refusal()),
                        _ => {}
                    }
                    *i += 1;
                }
            }
            Some(_) => {
                while bytes
                    .get(*i)
                    .is_some_and(|b| !b.is_ascii_whitespace() && !b",}]".contains(b))
                {
                    *i += 1;
                }
                Ok(())
            }
            None => Err(refusal()),
        }
    }
    loop {
        ws(bytes, &mut i);
        if bytes.get(i) == Some(&b'}') {
            break;
        }
        let key = i;
        string(bytes, &mut i)?;
        let key_end = i;
        ws(bytes, &mut i);
        if bytes.get(i) != Some(&b':') {
            return Err(refusal());
        }
        i += 1;
        ws(bytes, &mut i);
        let start = i;
        value(bytes, &mut i)?;
        found.push((&text[key..key_end], &text[start..i]));
        ws(bytes, &mut i);
        if bytes.get(i) == Some(&b',') {
            i += 1;
        } else if bytes.get(i) == Some(&b'}') {
            break;
        } else {
            return Err(refusal());
        }
    }
    Ok(found)
}
fn raw_member<'a>(text: &'a str, key: &str) -> Result<&'a str, MachineError> {
    for (name, value) in members(text)? {
        if serde_json::from_str::<String>(name).map_err(|_| refusal())? == key {
            return Ok(value);
        }
    }
    Err(refusal())
}
fn strict(bytes: &[u8]) -> Result<(String, Value), MachineError> {
    let text = std::str::from_utf8(bytes).map_err(|_| refusal())?;
    let encoded = crate::jcs::canonicalize(&crate::jcs::parse(text).map_err(|_| refusal())?)
        .map_err(|_| refusal())?;
    let value = serde_json::from_slice(&encoded).map_err(|_| refusal())?;
    Ok((text.trim_matches([' ', '\t', '\r', '\n']).to_owned(), value))
}

fn hex_capability(text: &str) -> Result<Capability, MachineError> {
    if text.len() != 64
        || !text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(refusal());
    }
    let mut bytes = [0_u8; 32];
    for (n, pair) in text.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let part = std::str::from_utf8(pair).map_err(|_| refusal())?;
        bytes[n] = u8::from_str_radix(part, 16).map_err(|_| refusal())?;
    }
    let cap = Capability::from_private_carrier(&bytes);
    bytes.zeroize();
    cap
}
fn public_arguments(
    raw: &str,
    arguments: &mut Value,
    route: HostRoute,
) -> Result<Option<Capability>, MachineError> {
    if route == HostRoute::Codex {
        if arguments.get(PRIVATE).is_some() || raw.len() > PUBLIC_LIMIT {
            return Err(refusal());
        }
        return Ok(None);
    }
    let mut carrier = arguments
        .as_object_mut()
        .ok_or_else(refusal)?
        .remove(PRIVATE)
        .ok_or_else(refusal)?;
    let result = hex_capability(carrier.as_str().ok_or_else(refusal)?);
    if let Value::String(text) = &mut carrier {
        text.zeroize();
    }
    // Keep each original public member's lexical spelling. Private carrier bytes
    // have a separate bound and never enlarge the public request allowance.
    let (name, value) = members(raw)?
        .into_iter()
        .find(|(name, _)| serde_json::from_str::<String>(name).ok().as_deref() == Some(PRIVATE))
        .ok_or_else(refusal)?;
    let base = raw.as_ptr() as usize;
    let key_start = name.as_ptr() as usize - base;
    let value_end = value.as_ptr() as usize - base + value.len();
    let start = raw[..key_start].rfind([',', '{']).ok_or_else(refusal)? + 1;
    let mut end = value_end;
    while raw.as_bytes().get(end).is_some_and(u8::is_ascii_whitespace) {
        end += 1;
    }
    let (start, end) = if raw.as_bytes().get(end) == Some(&b',') {
        (start, end + 1)
    } else if start > 1 {
        (start - 1, end)
    } else {
        (start, end)
    };
    let public = format!("{}{}", &raw[..start], &raw[end..]);
    if public.len() > PUBLIC_LIMIT {
        return Err(refusal());
    }
    result.map(Some)
}

pub(crate) fn hook(home: &DolgoraeHome, path: &Path, bytes: &[u8]) -> Result<Value, MachineError> {
    if bytes.len() > FRAME_LIMIT {
        return Err(refusal());
    }
    let (raw, event) = strict(bytes)?;
    if event["hook_event_name"] != "PreToolUse" {
        return Err(refusal());
    }
    let owner = VerifiedOwner::load(home, path)?;
    if owner.configuration.owner != OwnerKind::External {
        return Err(refusal());
    }
    let tool = event["tool_name"]
        .as_str()
        .and_then(|s| s.strip_prefix("mcp__dolgorae__"))
        .ok_or_else(refusal)?;
    if event["tool_input"].get(PRIVATE).is_some() {
        return Err(refusal());
    }
    let input = raw_member(&raw, "tool_input")?;
    let request = if crate::native_consumer::is_control(tool) {
        crate::native_consumer::Control::parse(input.as_bytes())?;
        crate::native_consumer::decode(input.as_bytes(), crate::native_consumer::MAX_INPUT)?
    } else {
        crate::host_invocation::parse_public_request(input)?
    };
    let native = NativeEvent {
        session: event["session_id"].as_str().ok_or_else(refusal)?.to_owned(),
        turn: match event.get("turn_id") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.clone()),
            _ => return Err(refusal()),
        },
        tool_use: event["tool_use_id"]
            .as_str()
            .ok_or_else(refusal)?
            .to_owned(),
        server: "dolgorae".into(),
        server_source: match owner.configuration.route {
            HostRoute::Codex => None,
            HostRoute::Claude => {
                if event["mcp_server"]["name"] != "dolgorae"
                    || event["mcp_server"]["source"] != "dynamic"
                {
                    return Err(refusal());
                }
                Some("dynamic".into())
            }
        },
        tool: tool.to_owned(),
    };
    let route = owner.configuration.route;
    let store = owner.ready()?;
    let (_, capability) = store.bind_native(native, request.clone())?;
    let mut output =
        json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow"}});
    if route == HostRoute::Claude {
        let binding = capability
            .private_carrier_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let mut updated = request;
        updated[PRIVATE] = json!(binding);
        output["hookSpecificOutput"]["updatedInput"] = updated;
    }
    Ok(output)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Fresh,
    Initializing,
    Ready,
}
fn rpc_error(id: Value, code: i64) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":match code{-32700=>"parse error",-32601=>"method not supported",-32602=>"invalid parameters",_=>"invalid request"}}})
}
fn rpc_result(id: Value, result: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":result})
}
fn string_or_integer(value: &Value) -> bool {
    value.is_string() || value.as_f64().is_some_and(|number| number.fract() == 0.0)
}

fn optional_object_params(params: Option<&Value>) -> bool {
    params.is_none_or(|value| {
        value
            .as_object()
            .is_some_and(|object| object.get("_meta").is_none_or(Value::is_object))
    })
}

fn request_params(params: Option<&Value>) -> bool {
    optional_object_params(params)
        && params
            .and_then(|params| params.get("_meta"))
            .and_then(|meta| meta.get("progressToken"))
            .is_none_or(string_or_integer)
}

fn initialize_params(params: &Value) -> bool {
    let info = &params["clientInfo"];
    let capabilities = &params["capabilities"];
    params["protocolVersion"].is_string()
        && info.is_object()
        && info["name"].is_string()
        && info["version"].is_string()
        && info.get("title").is_none_or(Value::is_string)
        && capabilities.is_object()
        && ["sampling", "elicitation"]
            .iter()
            .all(|name| capabilities.get(*name).is_none_or(Value::is_object))
        && capabilities.get("roots").is_none_or(|roots| {
            roots.is_object() && roots.get("listChanged").is_none_or(Value::is_boolean)
        })
        && capabilities.get("experimental").is_none_or(|features| {
            features
                .as_object()
                .is_some_and(|features| features.values().all(Value::is_object))
        })
}

struct Protocol {
    phase: Phase,
    tools: Vec<String>,
    route: HostRoute,
}
impl Protocol {
    fn frame(
        &mut self,
        bytes: &[u8],
        mut deliver: impl FnMut(&str, &Value, Option<&Capability>) -> Value,
    ) -> Option<Value> {
        let (raw, request) = match strict(bytes) {
            Ok(v) => v,
            Err(_) => return Some(rpc_error(Value::Null, -32700)),
        };
        if !request.is_object() {
            return Some(rpc_error(Value::Null, -32600));
        }
        let id = request.get("id").cloned();
        let notification = id.is_none();
        if request["jsonrpc"] != "2.0"
            || !request["method"].is_string()
            || id.as_ref().is_some_and(|id| !string_or_integer(id))
        {
            return Some(rpc_error(Value::Null, -32600));
        }
        let method = request["method"].as_str().expect("validated method");
        if notification {
            if method == "notifications/initialized"
                && self.phase == Phase::Initializing
                && optional_object_params(request.get("params"))
            {
                self.phase = Phase::Ready;
            }
            return None;
        }
        let id = id.expect("request ID");
        if !request_params(request.get("params")) {
            return Some(rpc_error(id, -32602));
        }
        let params = &request["params"];
        match method {
            "initialize" if self.phase == Phase::Fresh => {
                if !initialize_params(params) {
                    return Some(rpc_error(id, -32602));
                }
                self.phase = Phase::Initializing;
                Some(rpc_result(
                    id,
                    json!({"protocolVersion":PROTOCOL,"capabilities":{"tools":{}},"serverInfo":{"name":"dolgorae-specialists","version":env!("CARGO_PKG_VERSION")}}),
                ))
            }
            "ping" => Some(rpc_result(id, json!({}))),
            "tools/list" if self.phase == Phase::Ready => {
                if params.get("cursor").is_some() {
                    return Some(rpc_error(id, -32602));
                }
                Some(rpc_result(
                    id,
                    json!({"tools":self.tools.iter().map(|name|if crate::native_consumer::is_control(name){crate::native_consumer::control_spec(name)}else{facade::tool_spec(name,facade::EXTERNAL)}).collect::<Vec<_>>()}),
                ))
            }
            "tools/call" if self.phase == Phase::Ready => {
                let name = match params["name"].as_str() {
                    Some(name) if self.tools.iter().any(|t| t == name) => name,
                    _ => return Some(rpc_error(id, -32602)),
                };
                if crate::native_consumer::is_native(name)
                    && crate::native_consumer::encode(&id).map_or(true, |v| v.len() > 1024)
                {
                    return Some(rpc_error(id, -32602));
                }
                let raw_arguments =
                    match raw_member(&raw, "params").and_then(|p| raw_member(p, "arguments")) {
                        Ok(v) => v,
                        Err(_) => return Some(rpc_error(id, -32602)),
                    };
                let mut params = params.clone();
                let cap =
                    match public_arguments(raw_arguments, &mut params["arguments"], self.route) {
                        Ok(v) => v,
                        Err(_) => return Some(rpc_error(id, -32602)),
                    };
                if crate::host_invocation::normalize_tool_request(name, &params["arguments"])
                    .is_err()
                {
                    return Some(rpc_error(id, -32602));
                }
                let result = deliver(name, &params, cap.as_ref());
                if crate::native_consumer::is_native(name) {
                    let presented = match crate::consumer_bridge::native_content(&result, false) {
                        Ok(content) => {
                            json!({"content":content,"isError":crate::consumer_bridge::native_failed(&result)})
                        }
                        Err(_) => {
                            let failure = crate::consumer_bridge::presentation_failure(&result);
                            json!({"content":[{"type":"text","text":failure.to_string()}],"isError":true})
                        }
                    };
                    let wire = rpc_result(id, presented);
                    return Some(wire);
                }
                let failed = result["schema"] == "dolgorae.external-specialist-failure/v5";
                Some(rpc_result(
                    id,
                    json!({"content":[{"type":"text","text":result.to_string()}],"structuredContent":result,"isError":failed}),
                ))
            }
            "initialize" | "tools/list" | "tools/call" => Some(rpc_error(id, -32600)),
            _ => Some(rpc_error(id, -32601)),
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
enum Record {
    Complete,
    Oversized,
    Partial,
    Eof,
}
fn read_frame<R: BufRead>(reader: &mut R, frame: &mut Vec<u8>) -> io::Result<Record> {
    frame.clear();
    let mut oversized = false;
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return Ok(if oversized {
                Record::Oversized
            } else if frame.is_empty() {
                Record::Eof
            } else {
                Record::Partial
            });
        }
        let newline = buffer.iter().position(|b| *b == b'\n');
        let count = newline.map_or(buffer.len(), |n| n + 1);
        if frame.len().saturating_add(count) > FRAME_LIMIT {
            oversized = true;
            frame.clear();
        }
        if !oversized {
            frame.extend_from_slice(&buffer[..count]);
        }
        reader.consume(count);
        if newline.is_some() {
            return Ok(if oversized {
                Record::Oversized
            } else {
                Record::Complete
            });
        }
    }
}
fn serve<R: BufRead, W: Write>(
    mut reader: R,
    mut writer: W,
    mut protocol: Protocol,
    mut deliver: impl FnMut(&str, &Value, Option<&Capability>) -> Value,
) -> io::Result<()> {
    let mut frame = Vec::new();
    loop {
        let state = read_frame(&mut reader, &mut frame)?;
        if state == Record::Eof {
            return Ok(());
        }
        let response = match state {
            Record::Complete => protocol.frame(&frame, &mut deliver),
            _ => Some(rpc_error(Value::Null, -32700)),
        };
        frame.zeroize();
        if let Some(response) = response {
            writer.write_all(response.to_string().as_bytes())?;
            writer.write_all(b"\n")?;
            writer.flush()?;
        }
        if state == Record::Partial {
            return Ok(());
        }
    }
}

/// Private CLI wiring; no setup, host launch or lifecycle cancellation occurs here.
pub fn run(args: &[OsString]) -> Result<(), MachineError> {
    crate::darwin::private_host_umask();
    let operation = args.first().and_then(|s| s.to_str()).ok_or_else(refusal)?;
    let home = DolgoraeHome::system().map_err(|_| refusal())?;
    let mut values = std::collections::BTreeMap::new();
    let (pairs, remainder) = args[1..].as_chunks::<2>();
    for pair in pairs {
        let key = pair[0].to_str().ok_or_else(refusal)?;
        if !matches!(key, "--owner-config" | "--session" | "--call")
            || values.insert(key, &pair[1]).is_some()
        {
            return Err(refusal());
        }
    }
    if !remainder.is_empty() {
        return Err(refusal());
    }
    let path = Path::new(values.get("--owner-config").ok_or_else(refusal)?);
    match operation {
        "host-hook" if values.len() == 1 => {
            let mut bytes = Vec::new();
            io::stdin()
                .take((FRAME_LIMIT + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|_| refusal())?;
            let output=hook(&home,path,&bytes).unwrap_or_else(|_|json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"trusted invocation unavailable or conflicting"}}));
            bytes.zeroize();
            writeln!(io::stdout(), "{output}").map_err(|_| refusal())
        }
        "host-lookup" if values.len() == 3 => {
            let session = values
                .get("--session")
                .and_then(|s| s.to_str())
                .ok_or_else(refusal)?;
            let call = values
                .get("--call")
                .and_then(|s| s.to_str())
                .ok_or_else(refusal)?;
            let result = facade::lookup_native(&home, path, session, call)?;
            writeln!(io::stdout(), "{result}").map_err(|_| refusal())
        }
        "host-mcp" if values.len() == 1 => {
            let owner = VerifiedOwner::load(&home, path)?;
            if owner.configuration.owner != OwnerKind::External {
                return Err(refusal());
            }
            let tools = owner.configuration.tools.clone();
            let route = owner.configuration.route;
            drop(owner.ready()?);
            serve(
                io::BufReader::new(io::stdin().lock()),
                io::stdout().lock(),
                Protocol {
                    phase: Phase::Fresh,
                    tools,
                    route,
                },
                |tool, params, cap| {
                    let request = &params["arguments"];
                    match route {
                        HostRoute::Codex => match (
                            params["_meta"]["threadId"].as_str(),
                            params["_meta"]["callId"].as_str(),
                        ) {
                            (Some(session), Some(call)) => facade::external(
                                &home,
                                path,
                                tool,
                                NativeCarrier::Codex { session, call },
                                request,
                            ),
                            _ => facade::failure(
                                request["operation"].as_str().unwrap_or("lookup_operation"),
                                &refusal(),
                                false,
                            ),
                        },
                        HostRoute::Claude => match cap {
                            Some(cap) => facade::external(
                                &home,
                                path,
                                tool,
                                NativeCarrier::Claude(cap),
                                request,
                            ),
                            None => facade::failure(
                                request["operation"].as_str().unwrap_or("lookup_operation"),
                                &refusal(),
                                false,
                            ),
                        },
                    }
                },
            )
            .map_err(|_| refusal())
        }
        _ => Err(refusal()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trusted_host::tests::Fixture;
    use uuid::Uuid;
    fn protocol(route: HostRoute) -> Protocol {
        Protocol {
            phase: Phase::Fresh,
            tools: vec!["specialist".into()],
            route,
        }
    }
    fn init(protocol: &mut Protocol) {
        let reply=protocol.frame(br#"{"jsonrpc":"2.0","id":"init","method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}}"#,|_,_,_|panic!("initialize dispatched"));
        assert_eq!(reply.unwrap()["result"]["protocolVersion"], PROTOCOL);
        assert!(
            protocol
                .frame(
                    br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                    |_, _, _| panic!("notification dispatched")
                )
                .is_none()
        );
    }
    #[test]
    fn native_tool_listing_declares_object_input_schemas_for_both_hosts() {
        for route in [HostRoute::Codex, HostRoute::Claude] {
            let mut p = protocol(route);
            p.tools = vec![
                crate::native_consumer::EXTERNAL_NATIVE.into(),
                crate::native_consumer::EXTERNAL_CONTROL.into(),
            ];
            init(&mut p);
            let reply = p
                .frame(
                    br#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
                    |_, _, _| panic!("listing dispatched"),
                )
                .unwrap();
            let tools = reply["result"]["tools"].as_array().unwrap();
            assert_eq!(tools.len(), 2);
            for tool in tools {
                assert_eq!(tool["inputSchema"]["type"], "object");
                for key in ["oneOf", "anyOf", "allOf"] {
                    assert!(tool["inputSchema"].get(key).is_none());
                }
                let discriminator = if tool["name"] == crate::native_consumer::EXTERNAL_CONTROL {
                    "action"
                } else {
                    "operation"
                };
                assert!(
                    !tool["inputSchema"]["dependentSchemas"][discriminator]["oneOf"]
                        .as_array()
                        .unwrap()
                        .is_empty()
                );
            }
        }
    }
    pub(crate) fn open_request() -> Value {
        json!({"schema":"dolgorae.external-specialist-request/v5","operation":"open_external_engagement","workspace_ref":"placeholder","idempotency_key":"open","expected_revision":null})
    }
    fn event(request: Value, route: HostRoute, call: &str) -> Value {
        let mut value = json!({"hook_event_name":"PreToolUse","tool_name":"mcp__dolgorae__specialist","session_id":"thread-native","tool_use_id":call,"tool_input":request});
        if route == HostRoute::Claude {
            value["mcp_server"] = json!({"name":"dolgorae","source":"dynamic"});
        }
        value
    }
    #[test]
    fn transport_lifecycle_raw_grammar_ids_notifications_and_no_dispatch() {
        for bytes in [
            b"{".as_slice(),
            br#"{"a":0,"a":1}"#,
            br#"{"x":NaN}"#,
            br#"{"x":"\ud800"}"#,
            b"\xff",
        ] {
            assert_eq!(
                protocol(HostRoute::Codex)
                    .frame(bytes, |_, _, _| panic!("invalid input dispatched"))
                    .unwrap()["error"]["code"],
                -32700
            );
        }
        for bytes in [
            b"{}".as_slice(),
            b"[]",
            br#"{"jsonrpc":"2.0","id":9007199254740993,"method":"ping"}"#,
            br#"{"jsonrpc":"1.0","method":"ping"}"#,
            br#"{"jsonrpc":"2.0","id":{},"method":"ping"}"#,
        ] {
            assert_eq!(
                protocol(HostRoute::Codex)
                    .frame(bytes, |_, _, _| panic!("invalid envelope dispatched"))
                    .unwrap()["error"]["code"],
                -32600
            );
        }
        let extension = protocol(HostRoute::Codex)
            .frame(
                br#"{"jsonrpc":"2.0","id":1,"method":"ping","extra":null}"#,
                |_, _, _| panic!("metadata dispatched"),
            )
            .unwrap();
        assert_eq!(extension["result"], json!({}));
        assert_eq!(extension["id"], 1);
        let mut p = protocol(HostRoute::Codex);
        assert_eq!(
            p.frame(
                br#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
                |_, _, _| panic!()
            )
            .unwrap()["error"]["code"],
            -32600
        );
        init(&mut p);
        for id in [
            json!(0),
            json!(-9_007_199_254_740_991_i64),
            json!("rpc-correlation"),
        ] {
            let value = json!({"jsonrpc":"2.0","id":id,"method":"ping"});
            assert_eq!(
                p.frame(value.to_string().as_bytes(), |_, _, _| panic!())
                    .unwrap()["id"],
                id
            );
        }
        for method in [
            "tools/call",
            "notifications/cancelled",
            "future/notification",
        ] {
            let value =
                json!({"jsonrpc":"2.0","method":method,"params":{"arguments":open_request()}});
            assert!(
                p.frame(value.to_string().as_bytes(), |_, _, _| panic!(
                    "notification dispatched"
                ))
                .is_none()
            );
        }
        let tools = p
            .frame(
                br#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
                |_, _, _| panic!(),
            )
            .unwrap();
        assert_eq!(
            tools["result"]["tools"][0]["inputSchema"]["dependentSchemas"]["operation"]["oneOf"]
                .as_array()
                .unwrap()
                .len(),
            facade::EXTERNAL.len()
        );
        let public = tools.to_string();
        for secret in [
            PRIVATE,
            "controller_file",
            "owner_path",
            "group_specialists",
            "merge_results",
            "spawn_specialist_request",
        ] {
            assert!(!public.contains(secret), "advertised {secret}");
        }
        let mut request = open_request();
        request["unexpected"] = json!(true);
        let frame = json!({"jsonrpc":"2.0","id":"bad","method":"tools/call","params":{"name":"specialist","arguments":request}});
        assert_eq!(
            p.frame(frame.to_string().as_bytes(), |_, _, _| panic!(
                "closed request dispatched"
            ))
            .unwrap()["error"]["code"],
            -32602
        );
    }
    #[test]
    fn lifecycle_method_state_table_and_id_domains_preserve_correlation() {
        for phase in [Phase::Fresh, Phase::Initializing, Phase::Ready] {
            for method in [
                "initialize",
                "ping",
                "tools/list",
                "tools/call",
                "unknown/method",
            ] {
                let mut p = protocol(HostRoute::Codex);
                p.phase = phase;
                let params = match method {
                    "initialize" => {
                        json!({"protocolVersion":PROTOCOL,"capabilities":{},"clientInfo":{"name":"table","version":"1"}})
                    }
                    "tools/call" => json!({"name":"specialist","arguments":open_request()}),
                    _ => json!({}),
                };
                let mut calls = 0;
                let result = p
                    .frame(
                        json!({"jsonrpc":"2.0","id":"same-id","method":method,"params":params})
                            .to_string()
                            .as_bytes(),
                        |_, _, _| {
                            calls += 1;
                            facade::failure("open_external_engagement", &refusal(), false)
                        },
                    )
                    .unwrap();
                assert_eq!(result["id"], "same-id");
                let expected = match method {
                    "initialize" if phase != Phase::Fresh => Some(-32600),
                    "tools/list" | "tools/call" if phase != Phase::Ready => Some(-32600),
                    "unknown/method" => Some(-32601),
                    _ => None,
                };
                assert_eq!(
                    result["error"]["code"].as_i64(),
                    expected,
                    "{phase:?}/{method}: {result}"
                );
                assert_eq!(
                    calls,
                    usize::from(method == "tools/call" && phase == Phase::Ready)
                );
            }
            let mut p = protocol(HostRoute::Codex);
            p.phase = phase;
            assert!(
                p.frame(
                    br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                    |_, _, _| panic!()
                )
                .is_none()
            );
            assert_eq!(
                p.phase,
                if phase == Phase::Initializing {
                    Phase::Ready
                } else {
                    phase
                }
            );
        }
        for id in [
            json!(""),
            json!("é".repeat(512)),
            json!("x".repeat(513)),
            json!("x".repeat(65_536)),
            json!(9_007_199_254_740_991_u64),
            json!(9_007_199_254_740_992_u64),
        ] {
            let result = protocol(HostRoute::Codex)
                .frame(
                    json!({"jsonrpc":"2.0","id":id,"method":"ping"})
                        .to_string()
                        .as_bytes(),
                    |_, _, _| panic!(),
                )
                .unwrap();
            assert_eq!(result["id"], id);
            assert!(result.get("result").is_some());
        }
        for id in [json!(true), json!([]), json!({}), json!(0.5)] {
            assert_eq!(
                protocol(HostRoute::Codex)
                    .frame(
                        json!({"jsonrpc":"2.0","id":id,"method":"ping"})
                            .to_string()
                            .as_bytes(),
                        |_, _, _| panic!()
                    )
                    .unwrap()["error"]["code"],
                -32600
            );
        }
        let mut p = protocol(HostRoute::Codex);
        init(&mut p);
        for params in [
            json!({"name":"foreign","arguments":open_request()}),
            json!({"name":"specialist","arguments":null}),
        ] {
            assert_eq!(
                p.frame(
                    json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":params})
                        .to_string()
                        .as_bytes(),
                    |_, _, _| panic!("invalid params dispatched")
                )
                .unwrap()["error"]["code"],
                -32602
            );
        }
    }

    #[test]
    fn protocol_dictionary_extension_preserves_the_closed_public_arguments() {
        let mut p = protocol(HostRoute::Codex);
        init(&mut p);
        let request = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"specialist","arguments":open_request(),"extra":true}});
        let mut deliveries = 0;
        let result = p
            .frame(request.to_string().as_bytes(), |_, params, cap| {
                deliveries += 1;
                assert_eq!(params["arguments"], open_request());
                assert!(cap.is_none());
                json!({"response":true})
            })
            .unwrap();
        assert!(result.get("result").is_some());
        assert_eq!(deliveries, 1);
    }

    #[test]
    fn null_request_ids_never_advance_lifecycle_or_dispatch_and_unknown_error_ids_remain_null() {
        for phase in [Phase::Fresh, Phase::Initializing, Phase::Ready] {
            for method in ["initialize", "ping", "tools/list", "tools/call"] {
                let mut p = protocol(HostRoute::Codex);
                p.phase = phase;
                let params = match method {
                    "initialize" => {
                        json!({"protocolVersion":PROTOCOL,"capabilities":{},"clientInfo":{"name":"null-id","version":"1"}})
                    }
                    "tools/call" => json!({"name":"specialist","arguments":open_request()}),
                    _ => json!({}),
                };
                let response = p
                    .frame(
                        json!({"jsonrpc":"2.0","id":null,"method":method,"params":params})
                            .to_string()
                            .as_bytes(),
                        |_, _, _| panic!("null request dispatched"),
                    )
                    .unwrap();
                assert_eq!(response["error"]["code"], -32600, "{phase:?}/{method}");
                assert!(response["id"].is_null());
                assert_eq!(p.phase, phase);
            }
        }
        assert!(rpc_error(Value::Null, -32700)["id"].is_null());
        assert!(rpc_error(Value::Null, -32600)["id"].is_null());
    }

    #[test]
    fn supported_numeric_ids_and_tokens_keep_correlation_across_routes_and_methods() {
        let valid = [
            ("0", "0"),
            ("-0", "0"),
            ("1.0", "1"),
            ("1e0", "1"),
            ("1000e-3", "1"),
            ("9007199254740991", "9007199254740991"),
            ("9007199254740992", "9007199254740992"),
            ("-9007199254740992", "-9007199254740992"),
            ("9007199254740994", "9007199254740994"),
            ("-9007199254740994", "-9007199254740994"),
            ("9.007199254740992e15", "9007199254740992"),
            ("-9.007199254740992e15", "-9007199254740992"),
            ("9007199254740992.0", "9007199254740992"),
            ("1e21", "1e21"),
            ("-1e21", "-1e21"),
            ("1e30", "1e30"),
            ("-1e30", "-1e30"),
            ("1e300", "1e300"),
            ("-1e300", "-1e300"),
            (r#""""#, r#""""#),
            (r#""opaque""#, r#""opaque""#),
        ];
        let invalid = [
            "null",
            "true",
            "false",
            "[]",
            "{}",
            "0.5",
            "-0.5",
            "1e-3",
            "9007199254740993",
            "-9007199254740993",
            "9007199254740993.0",
            "9.007199254740993e15",
            "18446744073709551616",
            "1e309",
            "1e-400",
        ];
        for route in [HostRoute::Codex, HostRoute::Claude] {
            for method in ["initialize", "ping", "tools/list", "tools/call"] {
                let mut params = match method {
                    "initialize" => {
                        json!({"protocolVersion":PROTOCOL,"capabilities":{},"clientInfo":{"name":"fixture","version":"1"}})
                    }
                    "tools/call" => json!({"name":"specialist","arguments":open_request()}),
                    _ => json!({}),
                };
                if method == "tools/call" && route == HostRoute::Claude {
                    params["arguments"][PRIVATE] = json!("11".repeat(32));
                }
                let starting = if method == "initialize" {
                    Phase::Fresh
                } else {
                    Phase::Ready
                };
                for token in [false, true] {
                    for (raw_id, expected) in valid {
                        let mut p = protocol(route);
                        p.phase = starting;
                        let raw = if token {
                            let mut params = params.clone();
                            params["_meta"] = json!({"progressToken":"RAW_TOKEN"});
                            format!(
                                r#"{{"jsonrpc":"2.0","id":"correlation","method":"{method}","params":{}}}"#,
                                params.to_string().replace("\"RAW_TOKEN\"", raw_id)
                            )
                        } else {
                            format!(
                                r#"{{"jsonrpc":"2.0","id":{raw_id},"method":"{method}","params":{params}}}"#
                            )
                        };
                        let mut deliveries = 0;
                        let response = p
                            .frame(raw.as_bytes(), |name, args, cap| {
                                deliveries += 1;
                                assert_eq!(name, "specialist");
                                assert_eq!(args["arguments"], open_request());
                                assert_eq!(cap.is_some(), route == HostRoute::Claude);
                                json!({"original_public_body":true})
                            })
                            .unwrap();
                        assert!(
                            response.get("result").is_some(),
                            "{route:?}/{method}/token={token}/{raw_id}: {response}"
                        );
                        let expected: Value = if token {
                            json!("correlation")
                        } else {
                            serde_json::from_str(expected).unwrap()
                        };
                        assert_eq!(response["id"], expected, "{raw_id}");
                        assert_eq!(deliveries, usize::from(method == "tools/call"));
                        assert_eq!(
                            p.phase,
                            if method == "initialize" {
                                Phase::Initializing
                            } else {
                                starting
                            }
                        );
                    }
                    for raw_id in invalid {
                        let mut p = protocol(route);
                        p.phase = starting;
                        let raw = if token {
                            let mut params = params.clone();
                            params["_meta"] = json!({"progressToken":"RAW_TOKEN"});
                            format!(
                                r#"{{"jsonrpc":"2.0","id":"correlation","method":"{method}","params":{}}}"#,
                                params.to_string().replace("\"RAW_TOKEN\"", raw_id)
                            )
                        } else {
                            format!(
                                r#"{{"jsonrpc":"2.0","id":{raw_id},"method":"{method}","params":{params}}}"#
                            )
                        };
                        let response = p
                            .frame(raw.as_bytes(), |_, _, _| {
                                panic!("invalid numeric union dispatched")
                            })
                            .unwrap();
                        assert_eq!(
                            response["error"]["code"],
                            if token { -32602 } else { -32600 },
                            "{route:?}/{method}/token={token}/{raw_id}: {response}"
                        );
                        assert_eq!(
                            response["id"],
                            if token {
                                json!("correlation")
                            } else {
                                Value::Null
                            }
                        );
                        assert_eq!(p.phase, starting);
                    }
                }
            }
        }
    }

    #[test]
    fn initialize_negotiates_supported_version_without_dispatch() {
        for route in [HostRoute::Codex, HostRoute::Claude] {
            for offered in [
                PROTOCOL,
                "2025-03-26",
                "2026-11-25",
                "opaque-future-version",
            ] {
                let mut p = protocol(route);
                let message = json!({"jsonrpc":"2.0","id":"version-offer","method":"initialize","params":{"protocolVersion":offered,"capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}});
                let response = p
                    .frame(message.to_string().as_bytes(), |_, _, _| {
                        panic!("initialize dispatched product work")
                    })
                    .unwrap();
                assert_eq!(
                    response["result"]["protocolVersion"], PROTOCOL,
                    "{offered}: {response}"
                );
                assert_eq!(response["id"], "version-offer");
                assert_eq!(p.phase, Phase::Initializing);
                let repeated = p
                    .frame(message.to_string().as_bytes(), |_, _, _| panic!())
                    .unwrap();
                assert_eq!(repeated["error"]["code"], -32600);
                assert_eq!(p.phase, Phase::Initializing);
            }
            for offered in [
                None,
                Some(Value::Null),
                Some(json!(false)),
                Some(json!(12)),
                Some(json!([])),
                Some(json!({})),
            ] {
                let mut p = protocol(route);
                let mut params =
                    json!({"capabilities":{},"clientInfo":{"name":"fixture","version":"1"}});
                if let Some(offered) = offered {
                    params["protocolVersion"] = offered;
                }
                let response = p
                    .frame(
                        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":params})
                            .to_string()
                            .as_bytes(),
                        |_, _, _| panic!(),
                    )
                    .unwrap();
                assert_eq!(response["error"]["code"], -32602);
                assert_eq!(p.phase, Phase::Fresh);
            }
        }
    }

    #[test]
    fn common_request_and_notification_metadata_follow_selected_definitions() {
        fn message(method: &str) -> Value {
            let params = match method {
                "initialize" => {
                    json!({"protocolVersion":PROTOCOL,"capabilities":{},"clientInfo":{"name":"fixture","title":"Fixture","version":"1"}})
                }
                "tools/call" => json!({"name":"specialist","arguments":open_request()}),
                _ => json!({}),
            };
            json!({"jsonrpc":"2.0","id":"metadata","method":method,"params":params})
        }
        for method in ["initialize", "ping", "tools/list", "tools/call"] {
            for metadata in [
                json!({}),
                json!({"progressToken":"opaque"}),
                json!({"progressToken":-1}),
                json!({"progressToken":1.0}),
                json!({"progressToken":1e30}),
                json!({"progressToken":"","controller_principal":"ignored","extension":{"scope_ref":"not-authority"}}),
            ] {
                let mut p = protocol(HostRoute::Codex);
                p.phase = if method == "initialize" {
                    Phase::Fresh
                } else {
                    Phase::Ready
                };
                let mut request = message(method);
                request["params"]["_meta"] = metadata;
                request["params"]["extension"] = json!({"controller":"ignored"});
                request["extension"] = json!(null);
                let mut deliveries = 0;
                let response = p
                    .frame(request.to_string().as_bytes(), |name, params, cap| {
                        deliveries += 1;
                        assert_eq!(name, "specialist");
                        assert_eq!(params["arguments"], open_request());
                        assert!(cap.is_none());
                        json!({"valid_public_call":true})
                    })
                    .unwrap();
                assert!(response.get("result").is_some(), "{method}: {response}");
                assert_eq!(deliveries, usize::from(method == "tools/call"));
                assert_eq!(
                    p.phase,
                    if method == "initialize" {
                        Phase::Initializing
                    } else {
                        Phase::Ready
                    }
                );
            }
            for metadata in [
                Value::Null,
                json!([]),
                json!(false),
                json!({"progressToken":null}),
                json!({"progressToken":true}),
                json!({"progressToken":0.5}),
                json!({"progressToken":[]}),
                json!({"progressToken":{}}),
            ] {
                let mut p = protocol(HostRoute::Codex);
                let phase = if method == "initialize" {
                    Phase::Fresh
                } else {
                    Phase::Ready
                };
                p.phase = phase;
                let mut request = message(method);
                request["params"]["_meta"] = metadata.clone();
                let response = p
                    .frame(request.to_string().as_bytes(), |_, _, _| {
                        panic!("invalid request metadata dispatched")
                    })
                    .unwrap();
                assert_eq!(
                    response["error"]["code"], -32602,
                    "{method}/{metadata}: {response}"
                );
                assert_eq!(p.phase, phase);
            }
            for params in [Value::Null, json!([]), json!("params"), json!(false)] {
                let mut p = protocol(HostRoute::Codex);
                p.phase = if method == "initialize" {
                    Phase::Fresh
                } else {
                    Phase::Ready
                };
                let mut request = message(method);
                request["params"] = params;
                assert_eq!(
                    p.frame(request.to_string().as_bytes(), |_, _, _| panic!())
                        .unwrap()["error"]["code"],
                    -32602
                );
            }
        }
        for metadata in [
            json!({}),
            json!({"progressToken":null}),
            json!({"progressToken":{"notification_dictionary":true}}),
        ] {
            let mut p = protocol(HostRoute::Codex);
            p.phase = Phase::Initializing;
            assert!(p.frame(json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{"_meta":metadata,"extension":true},"extension":false}).to_string().as_bytes(),|_,_,_|panic!()).is_none());
            assert_eq!(
                p.phase,
                Phase::Ready,
                "notification metadata is an untyped dictionary"
            );
        }
        for (pointer, value) in [
            ("/params/clientInfo/title", Value::Null),
            ("/params/capabilities/roots/listChanged", json!("yes")),
            ("/params/capabilities/sampling", json!(false)),
            ("/params/capabilities/experimental/feature", json!(false)),
        ] {
            let mut request = message("initialize");
            match pointer {
                "/params/clientInfo/title" => request["params"]["clientInfo"]["title"] = value,
                "/params/capabilities/roots/listChanged" => {
                    request["params"]["capabilities"]["roots"] = json!({"listChanged":value})
                }
                "/params/capabilities/sampling" => {
                    request["params"]["capabilities"]["sampling"] = value
                }
                _ => request["params"]["capabilities"]["experimental"] = json!({"feature":value}),
            }
            let mut p = protocol(HostRoute::Codex);
            assert_eq!(
                p.frame(request.to_string().as_bytes(), |_, _, _| panic!())
                    .unwrap()["error"]["code"],
                -32602,
                "{pointer}"
            );
            assert_eq!(p.phase, Phase::Fresh);
        }
        for (field, invalid) in [
            ("clientInfo", Value::Null),
            ("clientInfo", json!({"version":"1"})),
            ("clientInfo", json!({"name":"fixture"})),
            ("clientInfo", json!({"name":1,"version":"1"})),
            ("clientInfo", json!({"name":"fixture","version":1})),
            ("capabilities", Value::Null),
            ("capabilities", json!([])),
            ("capabilities", json!({"roots":false})),
            ("capabilities", json!({"elicitation":false})),
            ("capabilities", json!({"experimental":false})),
        ] {
            let mut request = message("initialize");
            request["params"][field] = invalid;
            let mut p = protocol(HostRoute::Codex);
            assert_eq!(
                p.frame(request.to_string().as_bytes(), |_, _, _| panic!())
                    .unwrap()["error"]["code"],
                -32602,
                "{request}"
            );
            assert_eq!(p.phase, Phase::Fresh);
        }
        let mut request = message("initialize");
        request["params"]["capabilities"] = json!({"roots":{"listChanged":true,"extension":null},"sampling":{},"elicitation":{},"experimental":{"fixture":{}},"extension":false});
        request["params"]["clientInfo"]["extension"] = json!(null);
        let mut p = protocol(HostRoute::Codex);
        assert!(
            p.frame(request.to_string().as_bytes(), |_, _, _| panic!())
                .unwrap()
                .get("result")
                .is_some()
        );
        assert_eq!(p.phase, Phase::Initializing);
    }

    #[test]
    fn base_optional_params_preserve_metadata_readiness_presence_and_no_authority() {
        let metadata = json!({"_meta":{"trace":"ignored","controller_principal":"untrusted"},"extension":{"scope_ref":"not-authority"}});
        for params in [
            None,
            Some(json!({})),
            Some(metadata.clone()),
            Some(json!({"extension":null})),
        ] {
            let mut p = protocol(HostRoute::Codex);
            let initialize = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":PROTOCOL,"capabilities":{},"clientInfo":{"name":"metadata","version":"1"}}});
            assert!(
                p.frame(initialize.to_string().as_bytes(), |_, _, _| panic!())
                    .unwrap()
                    .get("result")
                    .is_some()
            );
            let mut initialized = json!({"jsonrpc":"2.0","method":"notifications/initialized"});
            if let Some(params) = &params {
                initialized["params"] = params.clone();
            }
            assert!(
                p.frame(initialized.to_string().as_bytes(), |_, _, _| panic!(
                    "metadata dispatched"
                ))
                .is_none()
            );
            assert_eq!(p.phase, Phase::Ready);
            for method in ["ping", "tools/list"] {
                let mut request = json!({"jsonrpc":"2.0","id":"metadata","method":method});
                if let Some(params) = &params {
                    request["params"] = params.clone();
                }
                assert!(
                    p.frame(request.to_string().as_bytes(), |_, _, _| panic!(
                        "metadata dispatched"
                    ))
                    .unwrap()
                    .get("result")
                    .is_some(),
                    "{method}/{params:?}"
                );
            }
        }
        for phase in [Phase::Fresh, Phase::Initializing, Phase::Ready] {
            let mut p = protocol(HostRoute::Codex);
            p.phase = phase;
            assert!(
                p.frame(
                    json!({"jsonrpc":"2.0","id":"ping","method":"ping","params":metadata})
                        .to_string()
                        .as_bytes(),
                    |_, _, _| panic!("ping metadata dispatched")
                )
                .unwrap()
                .get("result")
                .is_some()
            );
            assert_eq!(p.phase, phase);
        }
        let mut ready = protocol(HostRoute::Codex);
        ready.phase = Phase::Ready;
        let mut deliveries = 0;
        let response = ready.frame(json!({"jsonrpc":"2.0","id":"tool-meta","method":"tools/call","params":{"name":"specialist","arguments":open_request(),"_meta":metadata["_meta"]}}).to_string().as_bytes(), |name,params,cap| {
            deliveries += 1;
            assert_eq!(name, "specialist");
            assert_eq!(params["arguments"], open_request());
            assert!(cap.is_none());
            json!({"observed":"metadata supplies no capability"})
        }).unwrap();
        assert!(response.get("result").is_some());
        assert_eq!(deliveries, 1);
        for params in [
            Value::Null,
            json!([]),
            json!(false),
            json!(1),
            json!("params"),
            json!({"_meta":null}),
            json!({"_meta":[]}),
            json!({"_meta":"metadata"}),
        ] {
            let mut p = protocol(HostRoute::Codex);
            p.phase = Phase::Initializing;
            assert!(
                p.frame(
                    json!({"jsonrpc":"2.0","method":"notifications/initialized","params":params})
                        .to_string()
                        .as_bytes(),
                    |_, _, _| panic!()
                )
                .is_none()
            );
            assert_eq!(p.phase, Phase::Initializing, "{params}");
            for method in ["ping", "tools/list"] {
                p.phase = Phase::Ready;
                assert_eq!(
                    p.frame(
                        json!({"jsonrpc":"2.0","id":1,"method":method,"params":params})
                            .to_string()
                            .as_bytes(),
                        |_, _, _| panic!()
                    )
                    .unwrap()["error"]["code"],
                    -32602,
                    "{method}/{params}"
                );
            }
        }
        let mut p = protocol(HostRoute::Codex);
        p.phase = Phase::Ready;
        for meta in [Value::Null, json!([]), json!(false)] {
            assert_eq!(p.frame(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"specialist","arguments":open_request(),"_meta":meta}}).to_string().as_bytes(),|_,_,_|panic!("malformed metadata dispatched")).unwrap()["error"]["code"],-32602);
        }
        assert_eq!(p.frame(br#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"cursor":"not-issued"}}"#,|_,_,_|panic!()).unwrap()["error"]["code"],-32602);
        for params in [
            Value::Null,
            json!({}),
            json!({"protocolVersion":PROTOCOL,"capabilities":{},"clientInfo":{"name":"missing-version"}}),
        ] {
            let mut p = protocol(HostRoute::Codex);
            assert_eq!(
                p.frame(
                    json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":params})
                        .to_string()
                        .as_bytes(),
                    |_, _, _| panic!()
                )
                .unwrap()["error"]["code"],
                -32602
            );
            assert_eq!(p.phase, Phase::Fresh);
        }
    }

    #[test]
    fn frame_chunking_drain_partial_eof_and_correlation() {
        for capacity in 1..=32 {
            let input=b"{\"jsonrpc\":\"2.0\",\"id\":\"one\",\"method\":\"ping\"}\r\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n";
            let mut output = Vec::new();
            serve(
                io::BufReader::with_capacity(capacity, &input[..]),
                &mut output,
                protocol(HostRoute::Codex),
                |_, _, _| panic!(),
            )
            .unwrap();
            let values = String::from_utf8(output)
                .unwrap()
                .lines()
                .map(|s| serde_json::from_str::<Value>(s).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(values.len(), 2);
            assert_eq!(values[0]["id"], "one");
            assert_eq!(values[1]["id"], 2);
        }
        let mut output = Vec::new();
        serve(
            io::Cursor::new(b"{}"),
            &mut output,
            protocol(HostRoute::Codex),
            |_, _, _| panic!(),
        )
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&output).unwrap()["error"]["code"],
            -32700
        );
        let mut bytes = vec![b' '; FRAME_LIMIT + 1];
        bytes.extend_from_slice(b"\n{}\n");
        let mut reader = io::BufReader::with_capacity(8192, &bytes[..]);
        let mut frame = Vec::new();
        assert_eq!(
            read_frame(&mut reader, &mut frame).unwrap(),
            Record::Oversized
        );
        assert!(frame.is_empty());
        assert_eq!(
            read_frame(&mut reader, &mut frame).unwrap(),
            Record::Complete
        );
        assert_eq!(frame, b"{}\n");
        assert_eq!(read_frame(&mut reader, &mut frame).unwrap(), Record::Eof);
    }
    #[test]
    fn private_carrier_raw_allowance_and_native_origin_corpus() {
        let cap = "01".repeat(32);
        for position in 0..3 {
            let raw = match position {
                0 => format!("{{\"{PRIVATE}\":\"{cap}\", \"a\" : 1,\"b\":2}}"),
                1 => format!("{{\"a\" : 1, \"{PRIVATE}\":\"{cap}\",\"b\":2}}"),
                _ => format!("{{\"a\" : 1,\"b\":2, \"{PRIVATE}\":\"{cap}\"}}"),
            };
            let mut value = serde_json::from_str(&raw).unwrap();
            assert!(
                public_arguments(&raw, &mut value, HostRoute::Claude)
                    .unwrap()
                    .is_some()
            );
            assert_eq!(value, json!({"a":1,"b":2}));
        }
        let raw = format!(
            "{{\"a\":{}1,\"{PRIVATE}\":\"{cap}\"}}",
            " ".repeat(PUBLIC_LIMIT)
        );
        let mut value = serde_json::from_str(&raw).unwrap();
        assert!(
            public_arguments(&raw, &mut value, HostRoute::Claude).is_err(),
            "public raw whitespace exceeded the bound"
        );
        for invalid in ["", &"A".repeat(64), &"0".repeat(63), &"x".repeat(64)] {
            assert!(hex_capability(invalid).is_err());
        }
        assert!(hex_capability(&cap).is_ok());
        for route in [HostRoute::Codex, HostRoute::Claude] {
            let f = Fixture::new(route);
            f.complete();
            let mut request = open_request();
            request["workspace_ref"] = json!(f.owner().view.workspace_id);
            let original = event(request.clone(), route, "native-1");
            let allowed =
                hook(&f.home, &f.path(), &serde_json::to_vec(&original).unwrap()).unwrap();
            assert_eq!(allowed["hookSpecificOutput"]["permissionDecision"], "allow");
            let repeated =
                hook(&f.home, &f.path(), &serde_json::to_vec(&original).unwrap()).unwrap();
            assert_eq!(repeated, allowed);
            if route == HostRoute::Claude {
                let updated = &allowed["hookSpecificOutput"]["updatedInput"];
                let raw = updated.to_string();
                let mut public = updated.clone();
                let cap = public_arguments(&raw, &mut public, route).unwrap().unwrap();
                let store = f.ready();
                let selected = store.claude_carrier_delivery(&cap, &public).unwrap();
                assert_eq!(selected.request(), &request);
            } else {
                assert!(allowed["hookSpecificOutput"].get("updatedInput").is_none());
            }
            for field in ["session_id", "tool_use_id", "tool_name", "hook_event_name"] {
                let mut bad = original.clone();
                bad.as_object_mut().unwrap().remove(field);
                assert!(
                    hook(&f.home, &f.path(), &serde_json::to_vec(&bad).unwrap()).is_err(),
                    "missing {field}"
                );
            }
            let mut bad = original.clone();
            bad["tool_input"][PRIVATE] = json!("model-value");
            assert!(hook(&f.home, &f.path(), &serde_json::to_vec(&bad).unwrap()).is_err());
            let mut changed = original.clone();
            changed["tool_input"]["idempotency_key"] = json!("changed");
            assert!(hook(&f.home, &f.path(), &serde_json::to_vec(&changed).unwrap()).is_err());
            if route == HostRoute::Claude {
                let mut bad = original;
                bad["mcp_server"]["source"] = json!("user");
                assert!(hook(&f.home, &f.path(), &serde_json::to_vec(&bad).unwrap()).is_err());
            }
        }
    }
    #[test]
    fn native_keyed_acceptance_replay_fresh_get_and_empty_close_use_product_owners() {
        for route in [HostRoute::Codex, HostRoute::Claude] {
            let f = Fixture::new(route);
            f.complete();
            let mut request = open_request();
            request["workspace_ref"] = json!(f.owner().view.workspace_id);
            let bound = {
                let store = f.ready();
                let native = NativeEvent {
                    session: "thread-native".into(),
                    turn: Some("turn".into()),
                    tool_use: "open-call".into(),
                    server: "dolgorae".into(),
                    server_source: if route == HostRoute::Claude {
                        Some("dynamic".into())
                    } else {
                        None
                    },
                    tool: "specialist".into(),
                };
                store.bind_native(native, request.clone()).unwrap()
            };
            let port = crate::delegation_service::tests::FakePort::new();
            let deliver = || {
                facade::external_with_port(
                    &f.home,
                    &f.path(),
                    "specialist",
                    if route == HostRoute::Codex {
                        NativeCarrier::Codex {
                            session: "thread-native",
                            call: "open-call",
                        }
                    } else {
                        NativeCarrier::Claude(&bound.1)
                    },
                    &request,
                    &port,
                )
            };
            let first = deliver();
            assert_eq!(
                first["schema"], "dolgorae.external-specialist-response/v5",
                "{first}"
            );
            assert!(facade::validates(
                &first,
                "open_external_engagement_response"
            ));
            let repeated = deliver();
            assert_eq!(repeated["receipt"], first["receipt"]);
            let different = {
                let store = f.ready();
                store
                    .bind_native(
                        NativeEvent {
                            session: "thread-native".into(),
                            turn: Some("turn".into()),
                            tool_use: "different-call".into(),
                            server: "dolgorae".into(),
                            server_source: if route == HostRoute::Claude {
                                Some("dynamic".into())
                            } else {
                                None
                            },
                            tool: "specialist".into(),
                        },
                        request.clone(),
                    )
                    .unwrap()
            };
            assert_ne!(
                different.0.context().invocation_ref,
                bound.0.context().invocation_ref
            );
            let conflict = facade::external_with_port(
                &f.home,
                &f.path(),
                "specialist",
                if route == HostRoute::Codex {
                    NativeCarrier::Codex {
                        session: "thread-native",
                        call: "different-call",
                    }
                } else {
                    NativeCarrier::Claude(&different.1)
                },
                &request,
                &port,
            );
            assert_eq!(
                conflict["error"]["code"], "IDEMPOTENCY_CONFLICT",
                "{conflict}"
            );
            assert_eq!(conflict["error"]["effect"], "none");
            assert_eq!(deliver()["receipt"], first["receipt"]);
            assert_eq!(port.calls(), 0);
            let lookup =
                facade::lookup_native(&f.home, &f.path(), "thread-native", "open-call").unwrap();
            assert_eq!(lookup["lookup"]["receipt"], first["receipt"]);
            let owner = f.owner();
            let scope = crate::delegation_service::DelegationScope {
                view: &owner.view,
                state_root: &owner.state_root,
                controller: &owner.controller,
                trusted_invocation: None,
            };
            let id =
                Uuid::parse_str(first["engagement"]["engagement_ref"].as_str().unwrap()).unwrap();
            let get = crate::delegation_service::get_external_engagement(&scope, id).unwrap();
            assert_eq!(get.state, "open");
            assert!(
                crate::delegation_service::close_external_engagement(
                    &scope,
                    &port,
                    id,
                    get.revision + 1,
                    false
                )
                .is_err()
            );
            assert_eq!(
                crate::delegation_service::get_external_engagement(&scope, id).unwrap(),
                get
            );
            let close = crate::delegation_service::close_external_engagement(
                &scope,
                &port,
                id,
                get.revision,
                false,
            )
            .unwrap();
            assert_eq!(close["engagement"]["state"], "closed");
            assert!(
                facade::validates(&close, "close_external_engagement_response"),
                "{close}"
            );
            let repeat = crate::delegation_service::observe_state_control(
                &scope,
                crate::delegation_service::SettlementOwner::External { engagement_id: id },
                None,
                "close_external_engagement",
                None,
                None,
                Some(&json!({"allow_interrupt":false})),
            )
            .unwrap();
            assert_eq!(repeat, close);
            let product =
                crate::engagement::EngagementStore::workspace_database_path(&owner.state_root);
            let db = rusqlite::Connection::open(&product).unwrap();
            db.execute(
                "UPDATE engagements SET state='active' WHERE engagement_id=?1",
                [id.to_string()],
            )
            .unwrap();
            let before = std::fs::read(&product).unwrap();
            assert!(
                crate::delegation_service::observe_state_control(
                    &scope,
                    crate::delegation_service::SettlementOwner::External { engagement_id: id },
                    None,
                    "close_external_engagement",
                    None,
                    None,
                    Some(&json!({"allow_interrupt":false}))
                )
                .is_err(),
                "terminal close owner accepted a reopened aggregate"
            );
            assert_eq!(std::fs::read(&product).unwrap(), before);
            db.execute(
                "UPDATE engagements SET state='completed' WHERE engagement_id=?1",
                [id.to_string()],
            )
            .unwrap();

            let bytes = std::fs::read(f.private.join("invocations.sqlite")).unwrap();
            assert!(!bytes.is_empty());
        }
    }
    #[test]
    fn cold_stdio_child() {
        let Some(owner) = std::env::var_os("DOLGORAE_SPECIALIST_STDIO_OWNER") else {
            return;
        };
        let operation =
            std::env::var_os("DOLGORAE_SPECIALIST_CHILD_MODE").unwrap_or_else(|| "host-mcp".into());
        run(&[operation, "--owner-config".into(), owner]).unwrap();
    }
}
