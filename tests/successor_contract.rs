//! Executable H1 contract vectors, not production-runtime or consumer acceptance.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use dolgorae::jcs::{self, LosslessJson};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const VECTORS: &str = include_str!("../docs/protocol/dolgorae-v014-digest-vectors-v1.json");
const ACCEPTANCE: &str = include_str!("../docs/protocol/dolgorae-v014-acceptance-fixtures-v1.json");
const MAX_SAFE_INTEGER: u64 = (1_u64 << 53) - 1;
type Check = Result<(), &'static str>;

// Protobuf JSON accepts either spelling, but the digest object has one closed
// vocabulary. Reject extra fields and duplicate aliases before projecting it.
fn wire_identity_fields(value: &Value, names: &[(&str, &str)]) -> Result<Value, &'static str> {
    let object = value.as_object().ok_or("invalid_identity")?;
    if object.len() != names.len() {
        return Err("invalid_identity");
    }
    let mut result = serde_json::Map::new();
    for (canonical, alias) in names {
        let value = match (object.get(*canonical), object.get(*alias)) {
            (Some(value), None) | (None, Some(value)) => value,
            (Some(value), Some(_)) if canonical == alias => value,
            _ => return Err("invalid_identity"),
        };
        result.insert((*canonical).to_owned(), value.clone());
    }
    Ok(Value::Object(result))
}

fn wire_group_identity(value: &Value) -> Result<Value, &'static str> {
    let mut identity = wire_identity_fields(
        value,
        &[
            ("schema", "schema"),
            ("scope_kind", "scopeKind"),
            ("scope_ref", "scopeRef"),
            ("entries", "entries"),
        ],
    )?;
    identity["entries"] = Value::Array(
        array(&identity["entries"])?
            .iter()
            .map(|entry| {
                wire_identity_fields(
                    entry,
                    &[
                        ("entry_key", "entryKey"),
                        ("role_snapshot_digest", "roleSnapshotDigest"),
                        ("agent_configuration_digest", "agentConfigurationDigest"),
                        ("accepted_task_digest", "acceptedTaskDigest"),
                    ],
                )
            })
            .collect::<Result<Vec<_>, _>>()?,
    );
    validate_identity("dolgorae.group-assignment/v1", &identity)?;
    Ok(identity)
}

#[test]
fn wire_group_fixtures_preserve_closed_identity_and_scope() {
    let fixtures: Value = serde_json::from_str(include_str!(
        "../docs/protocol/dolgorae-gul-consumer-v2.fixtures.json"
    ))
    .unwrap();
    let mut checked = 0;
    for fixture in fixtures["positive"].as_array().unwrap() {
        let request = &fixture["grpc_json"];
        let assignment = request.get("assignment").or_else(|| {
            request
                .get("group")
                .and_then(|group| group.get("assignment"))
        });
        let Some(assignment) = assignment else {
            continue;
        };
        let identity = wire_group_identity(assignment)
            .unwrap_or_else(|error| panic!("fixture {}: {error}", fixture["id"]));
        if let Some(group) = request.get("group") {
            assert_eq!(
                group["group_assignment_digest"],
                checked_digest("dolgorae.group-assignment/v1", &identity.to_string())
                    .unwrap()
                    .0,
                "group digest in {}",
                fixture["id"]
            );
        }
        if let Some(scope) = request.get("scope") {
            if let Some(managed) = scope.get("managed") {
                assert_eq!(identity["scope_kind"], "orchestrated_session");
                assert_eq!(
                    identity["scope_ref"],
                    managed
                        .get("session_id")
                        .or_else(|| managed.get("sessionId"))
                        .unwrap()
                        .clone()
                );
            } else if let Some(external) = scope.get("external") {
                assert_eq!(identity["scope_kind"], "external_engagement");
                assert_eq!(
                    identity["scope_ref"],
                    external
                        .get("engagement_id")
                        .or_else(|| external.get("engagementId"))
                        .unwrap()
                        .clone()
                );
            } else {
                panic!("unknown group scope in {}", fixture["id"]);
            }
        }
        let mut invalid = assignment.clone();
        let field = if invalid.get("scopeKind").is_some() {
            "scopeKind"
        } else {
            "scope_kind"
        };
        invalid[field] = json!("managed");
        assert!(
            wire_group_identity(&invalid).is_err(),
            "invalid scope alias admitted"
        );
        invalid[field] = json!("external");
        assert!(
            wire_group_identity(&invalid).is_err(),
            "invalid scope alias admitted"
        );
        checked += 1;
    }
    assert_eq!(checked, 5, "group wire fixture coverage changed");
}

fn fixture_canonical_bytes(value: &Value) -> Vec<u8> {
    jcs::canonicalize(&jcs::parse(&value.to_string()).unwrap()).unwrap()
}

fn fixture_bytes_ref(bytes: &[u8]) -> Value {
    json!({"byte_length": bytes.len(), "sha256": format!("sha256:{}", jcs::sha256_hex(bytes))})
}

fn fixture_outcome(value: &Value) -> Value {
    json!(
        value
            .as_str()
            .unwrap()
            .strip_prefix("TASK_OUTCOME_")
            .unwrap()
            .to_ascii_lowercase()
    )
}

fn fixture_wire_identity(value: &Value) -> Value {
    let mut identity = value.clone();
    match identity["schema"].as_str().unwrap() {
        "dolgorae.accepted-task/v1" => {
            identity["execution_intent"] =
                json!(match identity["execution_intent"].as_str().unwrap() {
                    "CREATION_MODE_READ_ONLY" => "read_only",
                    "CREATION_MODE_WRITABLE" => "canonical_workspace_write",
                    _ => panic!("unsupported fixture creation mode"),
                });
        }
        "dolgorae.complete-result/v1" => {
            identity["terminal_outcome"] = fixture_outcome(&identity["terminal_outcome"]);
        }
        "dolgorae.merged-findings/v1" => {
            for source in identity["sources"].as_array_mut().unwrap() {
                source["state"] = fixture_outcome(&source["state"]);
            }
        }
        _ => panic!("unsupported fixture identity"),
    }
    identity
}

fn fixture_digest(identity: &Value) -> Value {
    json!(
        checked_digest(identity["schema"].as_str().unwrap(), &identity.to_string())
            .unwrap()
            .0
    )
}

fn fixture_inline_json(artifact: &Value) -> Value {
    let bytes = STANDARD
        .decode(artifact["inline_bytes"].as_str().unwrap())
        .unwrap();
    assert_eq!(artifact["content"], fixture_bytes_ref(&bytes));
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(bytes, fixture_canonical_bytes(&value));
    value
}

fn check_wire_fixture_identities(value: &Value, counts: &mut [usize; 3]) {
    if let Some(object) = value.as_object() {
        let identity = &value["identity"];
        let pair = match identity["schema"].as_str() {
            Some("dolgorae.accepted-task/v1") => Some(("accepted_task_digest", 0)),
            Some("dolgorae.complete-result/v1") => Some(("result_digest", 1)),
            Some("dolgorae.merged-findings/v1") => Some(("merged_findings_digest", 2)),
            _ => None,
        };
        if let Some((field, index)) = pair {
            assert_eq!(
                value[field],
                fixture_digest(&fixture_wire_identity(identity))
            );
            if let Some(body) = value.get("structured") {
                assert_eq!(
                    identity["task_body"],
                    fixture_bytes_ref(&fixture_canonical_bytes(body))
                );
            }
            if let Some(artifact) = value.get("final_response") {
                let bytes = STANDARD
                    .decode(artifact["inline_bytes"].as_str().unwrap())
                    .unwrap();
                assert_eq!(artifact["content"], fixture_bytes_ref(&bytes));
                assert_eq!(identity["final_response"], artifact["content"]);
            }
            counts[index] += 1;
        }
        for child in object.values() {
            check_wire_fixture_identities(child, counts);
        }
    } else if let Some(array) = value.as_array() {
        for child in array {
            check_wire_fixture_identities(child, counts);
        }
    }
}

#[test]
fn wire_task_and_result_fixtures_bind_exact_identities() {
    let fixtures: Value = serde_json::from_str(include_str!(
        "../docs/protocol/dolgorae-gul-consumer-v2.fixtures.json"
    ))
    .unwrap();
    let mut counts = [0; 3];
    for fixture in fixtures["positive"].as_array().unwrap() {
        check_wire_fixture_identities(&fixture["grpc_json"], &mut counts);
    }
    assert_eq!(counts, [12, 6, 1]);
}

#[test]
fn group_merge_and_synthesis_fixtures_preserve_complete_provenance() {
    let fixtures: Value = serde_json::from_str(include_str!(
        "../docs/protocol/dolgorae-gul-consumer-v2.fixtures.json"
    ))
    .unwrap();
    let cases: BTreeMap<_, _> = fixtures["positive"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| (item["id"].as_str().unwrap(), &item["grpc_json"]))
        .collect();
    let collect = cases["collect_group_response"];
    let merge = cases["merge_group_results_response"];
    let identity = fixture_wire_identity(&merge["identity"]);
    let sources = &identity["sources"];
    assert_eq!(
        collect["group"]["coverage"].as_array().unwrap().len(),
        sources.as_array().unwrap().len()
    );
    assert_eq!(fixture_inline_json(&collect["source_manifest"]), *sources);
    let merged = fixture_inline_json(&merge["merged_output"]);
    for field in ["schema", "algorithm", "group_assignment_digest", "sources"] {
        assert_eq!(merged[field], identity[field]);
    }
    assert_eq!(
        merged["missing_sources"],
        json!(
            sources
                .as_array()
                .unwrap()
                .iter()
                .filter(|source| source["state"] == "unknown" || source["state"] == "missing")
                .collect::<Vec<_>>()
        )
    );
    assert_eq!(identity["merged_output"], merge["merged_output"]["content"]);
    assert_eq!(
        identity["group_assignment_digest"],
        collect["group"]["group_assignment_digest"]
    );
    for (coverage, source) in collect["group"]["coverage"]
        .as_array()
        .unwrap()
        .iter()
        .zip(sources.as_array().unwrap())
    {
        let mut canonical = coverage.clone();
        canonical["state"] = fixture_outcome(&canonical["state"]);
        assert_eq!(canonical, *source);
        let entry = collect["group"]["assignment"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["entry_key"] == source["entry_key"])
            .unwrap();
        for field in [
            "role_snapshot_digest",
            "agent_configuration_digest",
            "accepted_task_digest",
        ] {
            assert_eq!(entry[field], source[field]);
        }
        if source["state"] == "completed" {
            let result = collect["results"]
                .as_array()
                .unwrap()
                .iter()
                .find(|result| result["result_digest"] == source["result_digest"])
                .unwrap();
            for field in [
                "worker_ref",
                "task_ref",
                "role_snapshot_digest",
                "agent_configuration_digest",
                "accepted_task_digest",
            ] {
                assert_eq!(result["identity"]["source"][field], source[field]);
            }
        }
    }
    assert_eq!(
        cases["merge_group_results_request"]["sources"],
        collect["group"]["coverage"]
    );
    let synthesis = cases["assign_synthesis_request"];
    assert_eq!(
        synthesis["merged_findings_digest"],
        merge["merged_findings_digest"]
    );
    assert_eq!(
        synthesis["task"]["identity"]["contexts"],
        synthesis["task"]["structured"]["context"]
    );
    assert_eq!(
        synthesis["task"]["identity"]["contexts"][0]["content"],
        merge["merged_output"]["content"]
    );
    let constructed = &fixtures["constructed_identity_inputs"];
    assert_eq!(
        constructed["accepted_task"],
        fixture_wire_identity(
            &cases["assign_external_specialist_task_request"]["task"]["identity"]
        )
    );
    assert_eq!(
        constructed["complete_result"],
        fixture_wire_identity(
            &cases["lookup_operation_response"]["accepted"]["result"]["identity"]
        )
    );
    assert_eq!(
        constructed["group_assignment"],
        collect["group"]["assignment"]
    );
    assert_eq!(constructed["merged_findings"], identity);
}

#[test]
fn corrected_mutation_receipts_bind_normalized_group_merge_and_synthesis_inputs() {
    let fixtures: Value = serde_json::from_str(include_str!(
        "../docs/protocol/dolgorae-gul-consumer-v2.fixtures.json"
    ))
    .unwrap();
    let cases: BTreeMap<_, _> = fixtures["positive"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| (item["id"].as_str().unwrap(), &item["grpc_json"]))
        .collect();
    for (operation, constructed) in fixtures["constructed_mutation_inputs"].as_object().unwrap() {
        let request = cases[constructed["request_fixture"].as_str().unwrap()];
        let response = cases[constructed["response_fixture"].as_str().unwrap()];
        let body = match operation.as_str() {
            "assign_group" => {
                json!({"manifest":request["assignment"],"inputs":request["inputs"].as_array().unwrap().iter().map(|source| json!({"entry_key":source["entry_key"],"content":source["content"]})).collect::<Vec<_>>()})
            }
            "merge_group_results" => {
                let sources = fixture_wire_identity(
                    &json!({"schema":"dolgorae.merged-findings/v1","sources":request["sources"]}),
                );
                json!({"group_ref":request["group_id"],"group_assignment_digest":cases["collect_group_response"]["group"]["group_assignment_digest"],"sources":sources["sources"],"algorithm":request["algorithm"]})
            }
            "assign_synthesis" => {
                json!({"member_ref":request["member_id"],"merged_findings_digest":request["merged_findings_digest"],"merged_input":request["task"]["identity"]["contexts"][0]["content"],"assignment":{"task_body":request["task"]["structured"],"accepted_task_identity":fixture_wire_identity(&request["task"]["identity"]),"deadline_seconds":request["task"]["deadline_seconds"]}})
            }
            _ => panic!("unexpected corrected mutation"),
        };
        assert_eq!(body, constructed["normalized_body"]);
        let identity = &constructed["identity"];
        let scope = &response["receipt"]["operation"]["caller_scope"];
        assert_eq!(
            identity["request_body"],
            fixture_bytes_ref(&fixture_canonical_bytes(&body))
        );
        assert_eq!(identity["operation_kind"], *operation);
        assert_eq!(identity["workspace_ref"], scope["workspace_id"]);
        assert_eq!(identity["scope_ref"], scope["external_engagement_id"]);
        assert_eq!(identity["controller_principal"], scope["principal_ref"]);
        assert_eq!(
            identity["controller_generation"],
            scope["controller_generation"]
        );
        assert_eq!(
            identity["idempotency_key"],
            request["admission"]["idempotency_key"]
        );
        assert_eq!(
            response["receipt"]["accepted_input_digest"],
            fixture_digest(identity)
        );
    }
    let facade: Value = serde_json::from_str(include_str!(
        "../docs/protocol/examples/successor-facade-v5-methods.valid.json"
    ))
    .unwrap();
    let cases: BTreeMap<_, _> = facade["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| (item["operation"].as_str().unwrap(), item))
        .collect();
    let group_digest = fixture_digest(&cases["assign_group"]["request"]["manifest"]);
    for operation in ["await_group", "collect_group"] {
        let coverage = &cases[operation]["response"]["coverage"];
        assert_eq!(coverage["group_assignment_digest"], group_digest);
        assert_eq!(
            coverage["sources"][0]["accepted_task_digest"],
            cases["assign_group"]["request"]["manifest"]["entries"][0]["accepted_task_digest"]
        );
    }
    let merge = &cases["merge_group_results"]["response"];
    assert_eq!(
        merge["merged_identity"]["group_assignment_digest"],
        group_digest
    );
    assert_eq!(
        merge["merged_digest"],
        fixture_digest(&merge["merged_identity"])
    );
    assert_eq!(
        cases["assign_synthesis"]["request"]["merged_findings_digest"],
        merge["merged_digest"]
    );
    for operation in ["assign_group", "merge_group_results", "assign_synthesis"] {
        let case = cases[operation];
        let receipt = &case["response"]["receipt"];
        let namespace = &receipt["namespace"];
        let identity = json!({"schema":"dolgorae.mutation-envelope/v1","request_schema":case["normalized_body_schema"],"operation_kind":operation,"workspace_ref":namespace["workspace_ref"],"scope_kind":namespace["scope_kind"],"scope_ref":namespace["scope_ref"],"controller_principal":namespace["controller_principal"],"controller_generation":namespace["controller_generation"],"idempotency_key":namespace["idempotency_key"],"trusted_invocation_ref":null,"request_body":fixture_bytes_ref(&fixture_canonical_bytes(&case["normalized_request_body"]))});
        assert_eq!(receipt["accepted_input_digest"], fixture_digest(&identity));
    }
}

fn fields(domain: &str) -> &'static [&'static str] {
    match domain {
        "dolgorae.role-snapshot/v1" => {
            &["schema", "role_body", "dependencies", "selected_guidance"]
        }
        "dolgorae.instruction-identity/v1" => &[
            "schema",
            "core_version",
            "core",
            "common",
            "role_snapshot_digest",
            "dependency_order",
            "guidance_order",
            "composition_version",
            "composition",
        ],
        "dolgorae.admitted-configuration/v1" => &[
            "schema",
            "profile_binding_schema",
            "profile_binding_digest",
            "model",
            "effort",
            "purpose_kind",
            "purpose_label",
            "required_capabilities",
            "execution_lane",
            "requested_access",
            "required_assurance",
            "native_subagent_policy",
            "instruction_identity_digest",
        ],
        "dolgorae.accepted-task/v1" => &[
            "schema",
            "purpose",
            "task_body_schema",
            "task_body",
            "contexts",
            "images",
            "expected_output_schema",
            "execution_intent",
            "admitted_model",
            "admitted_effort",
            "role_snapshot_digest",
            "agent_configuration_digest",
        ],
        "dolgorae.mutation-envelope/v1" => &[
            "schema",
            "request_schema",
            "operation_kind",
            "workspace_ref",
            "scope_kind",
            "scope_ref",
            "controller_principal",
            "controller_generation",
            "idempotency_key",
            "trusted_invocation_ref",
            "request_body",
        ],
        "dolgorae.complete-result/v1" => &[
            "schema",
            "source",
            "terminal_outcome",
            "final_response",
            "structured_output_schema",
            "structured_output",
            "changes",
            "attachments",
            "safe_failure_code",
        ],
        "dolgorae.group-assignment/v1" => &["schema", "scope_kind", "scope_ref", "entries"],
        "dolgorae.merged-findings/v1" => &[
            "schema",
            "algorithm",
            "group_assignment_digest",
            "sources",
            "merged_output",
        ],
        _ => &[],
    }
}

fn closed(value: &Value, names: &[&str]) -> Check {
    let object = value.as_object().ok_or("invalid_identity")?;
    if object.len() != names.len() || names.iter().any(|name| !object.contains_key(*name)) {
        return Err("invalid_identity");
    }
    Ok(())
}

fn safe_integers(value: &LosslessJson) -> Check {
    match value {
        LosslessJson::Number(number) => {
            if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err("invalid_integer");
            }
            let integer: u64 = number.parse().map_err(|_| "invalid_integer")?;
            if integer > MAX_SAFE_INTEGER {
                return Err("invalid_integer");
            }
        }
        LosslessJson::Array(items) => {
            for item in items {
                safe_integers(item)?;
            }
        }
        LosslessJson::Object(items) => {
            for (_, item) in items {
                safe_integers(item)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn string(value: &Value) -> Result<&str, &'static str> {
    value.as_str().ok_or("invalid_identity")
}

fn digest_ref(value: &Value) -> Check {
    let value = string(value)?;
    if value.len() != 71
        || !value.starts_with("sha256:")
        || !value.as_bytes()[7..]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        return Err("invalid_identity");
    }
    Ok(())
}

fn bytes_ref(value: &Value, nonempty: bool) -> Check {
    closed(value, &["byte_length", "sha256"])?;
    let length = value["byte_length"].as_u64().ok_or("invalid_identity")?;
    if length > MAX_SAFE_INTEGER || (nonempty && length == 0) {
        return Err("invalid_identity");
    }
    digest_ref(&value["sha256"])
}

fn array(value: &Value) -> Result<&Vec<Value>, &'static str> {
    value.as_array().ok_or("invalid_identity")
}

fn sorted_unique<'a>(values: impl Iterator<Item = &'a str>) -> bool {
    let mut prior = None;
    for value in values {
        if prior.is_some_and(|last: &str| last.as_bytes() >= value.as_bytes()) {
            return false;
        }
        prior = Some(value);
    }
    true
}

fn alias(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !value.chars().any(char::is_control)
        && !value.contains(['/', '\\'])
}

fn named_sources(value: &Value, sorted: bool) -> Check {
    let sources = array(value)?;
    if sources.len() > 64 {
        return Err("invalid_identity");
    }
    let mut names = Vec::new();
    for source in sources {
        closed(source, &["name", "content"])?;
        let name = string(&source["name"])?;
        if !alias(name) || names.contains(&name) {
            return Err("invalid_identity");
        }
        names.push(name);
        bytes_ref(&source["content"], true)?;
    }
    if sorted && !sorted_unique(names.into_iter()) {
        return Err("invalid_identity");
    }
    Ok(())
}

fn context(value: &Value) -> Check {
    closed(value, &["kind", "identity", "content", "media_type"])?;
    if !["accepted_context", "artifact", "review_target"].contains(&string(&value["kind"])?) {
        return Err("invalid_identity");
    }
    if string(&value["identity"])?.is_empty() || string(&value["media_type"])?.is_empty() {
        return Err("invalid_identity");
    }
    bytes_ref(&value["content"], false)
}

fn one_of(value: &Value, choices: &[&str]) -> Check {
    if choices.contains(&string(value)?) {
        Ok(())
    } else {
        Err("invalid_identity")
    }
}

fn validate_identity(domain: &str, value: &Value) -> Check {
    if fields(domain).is_empty() || value["schema"] != domain {
        return Err("invalid_identity");
    }
    closed(value, fields(domain))?;
    match domain {
        "dolgorae.role-snapshot/v1" => {
            bytes_ref(&value["role_body"], true)?;
            named_sources(&value["dependencies"], true)?;
            named_sources(&value["selected_guidance"], false)?;
        }
        "dolgorae.instruction-identity/v1" => {
            if value["core_version"].as_u64().is_none() {
                return Err("invalid_identity");
            }
            for key in ["core", "common", "composition"] {
                bytes_ref(&value[key], true)?;
            }
            digest_ref(&value["role_snapshot_digest"])?;
            if value["composition_version"] != "core-common-role-dependencies-guidance-lf2/v1" {
                return Err("invalid_identity");
            }
            for key in ["dependency_order", "guidance_order"] {
                let names = array(&value[key])?;
                if names.len() > 64 {
                    return Err("invalid_identity");
                }
                let mut unique = BTreeSet::new();
                for name in names {
                    let name = string(name)?;
                    if !alias(name) || !unique.insert(name) {
                        return Err("invalid_identity");
                    }
                }
            }
            if !sorted_unique(
                array(&value["dependency_order"])?
                    .iter()
                    .map(|name| name.as_str().unwrap()),
            ) {
                return Err("invalid_identity");
            }
        }
        "dolgorae.admitted-configuration/v1" => {
            digest_ref(&value["profile_binding_digest"])?;
            digest_ref(&value["instruction_identity_digest"])?;
            for key in ["profile_binding_schema", "model", "effort"] {
                if string(&value[key])?.is_empty() {
                    return Err("invalid_identity");
                }
            }
            if !value["purpose_label"].is_null() {
                string(&value["purpose_label"])?;
            }
            one_of(
                &value["purpose_kind"],
                &[
                    "interactive",
                    "planning",
                    "implementation",
                    "review",
                    "research",
                    "discussion",
                    "workflow_stage",
                    "other",
                ],
            )?;
            let caps = array(&value["required_capabilities"])?;
            for cap in caps {
                if string(cap)?.is_empty() {
                    return Err("invalid_identity");
                }
            }
            if !sorted_unique(caps.iter().map(|cap| cap.as_str().unwrap())) {
                return Err("invalid_identity");
            }
            one_of(&value["execution_lane"], &["dedicated", "shared_readonly"])?;
            one_of(
                &value["requested_access"],
                &["read_only", "canonical_workspace_write"],
            )?;
            one_of(
                &value["required_assurance"],
                &[
                    "best_effort_personal_alpha",
                    "verified_thread_scoped_control",
                    "strong_process_containment",
                ],
            )?;
            one_of(&value["native_subagent_policy"], &["enabled", "disabled"])?;
            if value["execution_lane"] == "shared_readonly"
                && (value["requested_access"] != "read_only"
                    || value["native_subagent_policy"] != "disabled")
            {
                return Err("invalid_identity");
            }
        }
        "dolgorae.accepted-task/v1" => {
            for key in [
                "purpose",
                "task_body_schema",
                "expected_output_schema",
                "admitted_model",
                "admitted_effort",
            ] {
                if string(&value[key])?.is_empty() {
                    return Err("invalid_identity");
                }
            }
            bytes_ref(&value["task_body"], true)?;
            for context_ref in array(&value["contexts"])? {
                context(context_ref)?;
            }
            for image in array(&value["images"])? {
                closed(image, &["content", "media_type", "detail"])?;
                bytes_ref(&image["content"], true)?;
                one_of(
                    &image["media_type"],
                    &["image/png", "image/jpeg", "image/webp", "image/gif"],
                )?;
                one_of(&image["detail"], &["auto", "low", "high"])?;
            }
            one_of(
                &value["execution_intent"],
                &["read_only", "canonical_workspace_write"],
            )?;
            digest_ref(&value["role_snapshot_digest"])?;
            digest_ref(&value["agent_configuration_digest"])?;
        }
        "dolgorae.mutation-envelope/v1" => {
            for key in [
                "request_schema",
                "operation_kind",
                "workspace_ref",
                "controller_principal",
                "idempotency_key",
            ] {
                if string(&value[key])?.is_empty() {
                    return Err("invalid_identity");
                }
            }
            if value["controller_generation"].as_u64().is_none() {
                return Err("invalid_identity");
            }
            if value["scope_kind"].is_null() != value["scope_ref"].is_null() {
                return Err("invalid_identity");
            }
            if !value["scope_kind"].is_null() {
                one_of(
                    &value["scope_kind"],
                    &[
                        "workspace",
                        "run",
                        "profile",
                        "external_engagement",
                        "orchestrated_session",
                    ],
                )?;
                string(&value["scope_ref"])?;
            }
            if !value["trusted_invocation_ref"].is_null() {
                string(&value["trusted_invocation_ref"])?;
            }
            bytes_ref(&value["request_body"], true)?;
        }
        "dolgorae.complete-result/v1" => {
            let source = &value["source"];
            closed(
                source,
                &[
                    "scope_kind",
                    "scope_ref",
                    "worker_ref",
                    "task_ref",
                    "role_snapshot_digest",
                    "agent_configuration_digest",
                    "accepted_task_digest",
                ],
            )?;
            for key in ["scope_kind", "scope_ref", "worker_ref", "task_ref"] {
                if string(&source[key])?.is_empty() {
                    return Err("invalid_identity");
                }
            }
            for key in [
                "role_snapshot_digest",
                "agent_configuration_digest",
                "accepted_task_digest",
            ] {
                digest_ref(&source[key])?;
            }
            one_of(
                &value["terminal_outcome"],
                &["completed", "failed", "cancelled", "expired"],
            )?;
            for key in ["final_response", "structured_output"] {
                if !value[key].is_null() {
                    bytes_ref(&value[key], false)?;
                }
            }
            if value["structured_output_schema"].is_null() != value["structured_output"].is_null() {
                return Err("invalid_identity");
            }
            if value["terminal_outcome"] == "completed" && !value["safe_failure_code"].is_null() {
                return Err("invalid_identity");
            }
            let mut changes = Vec::new();
            for change in array(&value["changes"])? {
                closed(
                    change,
                    &[
                        "kind",
                        "path",
                        "old_path",
                        "before",
                        "after",
                        "before_mode",
                        "after_mode",
                    ],
                )?;
                one_of(&change["kind"], &["add", "modify", "delete", "rename"])?;
                let path = string(&change["path"])?;
                if path.starts_with('/')
                    || path.split('/').any(|part| part == ".." || part.is_empty())
                {
                    return Err("invalid_identity");
                }
                if (change["kind"] == "rename") == change["old_path"].is_null() {
                    return Err("invalid_identity");
                }
                for key in ["before", "after"] {
                    if !change[key].is_null() {
                        bytes_ref(&change[key], false)?;
                    }
                }
                if change["kind"] == "add" && !change["before"].is_null() {
                    return Err("invalid_identity");
                }
                if change["kind"] == "delete" && !change["after"].is_null() {
                    return Err("invalid_identity");
                }
                changes.push((
                    path,
                    change["old_path"].as_str().unwrap_or(""),
                    string(&change["kind"])?,
                ));
            }
            if changes.windows(2).any(|pair| pair[0] >= pair[1]) {
                return Err("invalid_identity");
            }
            for attachment in array(&value["attachments"])? {
                context(attachment)?;
            }
        }
        "dolgorae.group-assignment/v1" => {
            one_of(
                &value["scope_kind"],
                &["external_engagement", "orchestrated_session"],
            )?;
            if string(&value["scope_ref"])?.is_empty() {
                return Err("invalid_identity");
            }
            let entries = array(&value["entries"])?;
            if entries.is_empty() || entries.len() > 64 {
                return Err("invalid_identity");
            }
            let mut keys = Vec::new();
            for entry in entries {
                closed(
                    entry,
                    &[
                        "entry_key",
                        "role_snapshot_digest",
                        "agent_configuration_digest",
                        "accepted_task_digest",
                    ],
                )?;
                keys.push(string(&entry["entry_key"])?);
                for key in [
                    "role_snapshot_digest",
                    "agent_configuration_digest",
                    "accepted_task_digest",
                ] {
                    digest_ref(&entry[key])?;
                }
            }
            if !sorted_unique(keys.into_iter()) {
                return Err("invalid_identity");
            }
        }
        "dolgorae.merged-findings/v1" => {
            if value["algorithm"] != "merge-exact-findings-v1" {
                return Err("invalid_identity");
            }
            digest_ref(&value["group_assignment_digest"])?;
            bytes_ref(&value["merged_output"], true)?;
            let mut keys = Vec::new();
            for source in array(&value["sources"])? {
                closed(
                    source,
                    &[
                        "entry_key",
                        "worker_ref",
                        "task_ref",
                        "role_snapshot_digest",
                        "agent_configuration_digest",
                        "accepted_task_digest",
                        "state",
                        "result_digest",
                        "safe_failure_code",
                    ],
                )?;
                keys.push(string(&source["entry_key"])?);
                one_of(
                    &source["state"],
                    &[
                        "completed",
                        "failed",
                        "cancelled",
                        "expired",
                        "unknown",
                        "missing",
                    ],
                )?;
                let unresolved = ["unknown", "missing"].contains(&string(&source["state"])?);
                if unresolved != source["result_digest"].is_null() {
                    return Err("invalid_identity");
                }
                for key in [
                    "role_snapshot_digest",
                    "agent_configuration_digest",
                    "accepted_task_digest",
                    "result_digest",
                ] {
                    if !source[key].is_null() {
                        digest_ref(&source[key])?;
                    }
                }
            }
            if !sorted_unique(keys.into_iter()) {
                return Err("invalid_identity");
            }
        }
        _ => return Err("invalid_identity"),
    }
    Ok(())
}

fn checked_digest(domain: &str, input: &str) -> Result<(String, String), &'static str> {
    let parsed = jcs::parse(input).map_err(|_| "invalid_json")?;
    safe_integers(&parsed)?;
    let canonical = jcs::canonicalize(&parsed).map_err(|_| "invalid_json")?;
    let value: Value = serde_json::from_slice(&canonical).map_err(|_| "invalid_json")?;
    validate_identity(domain, &value)?;
    let mut bytes = domain.as_bytes().to_vec();
    bytes.push(b'\n');
    bytes.extend_from_slice(&canonical);
    Ok((
        format!("sha256:{}", jcs::sha256_hex(&bytes)),
        String::from_utf8(canonical).unwrap(),
    ))
}

#[test]
fn eight_domains_known_answers_and_closed_negative_inputs() {
    let fixture: Value = serde_json::from_str(VECTORS).unwrap();
    assert_eq!(fixture["contract_only"], true);
    let mut domains = BTreeSet::new();
    let mut answers = BTreeMap::new();
    for vector in fixture["vectors"].as_array().unwrap() {
        let domain = vector["domain"].as_str().unwrap();
        let input = serde_json::to_string(&vector["input"]).unwrap();
        let (digest, canonical) = checked_digest(domain, &input)
            .unwrap_or_else(|error| panic!("{}: {error}", vector["id"]));
        assert_eq!(digest, vector["expected_digest"], "{}", vector["id"]);
        assert_eq!(canonical, vector["canonical_utf8"], "{}", vector["id"]);
        domains.insert(domain);
        answers.insert(vector["id"].as_str().unwrap(), digest);
    }
    assert_eq!(domains.len(), 8);
    for invalid in fixture["invalid"].as_array().unwrap() {
        assert_eq!(
            checked_digest(
                invalid["domain"].as_str().unwrap(),
                invalid["raw_json"].as_str().unwrap()
            )
            .unwrap_err(),
            invalid["expected_error"].as_str().unwrap(),
            "{}",
            invalid["id"]
        );
    }
    for relation in fixture["relations"].as_array().unwrap() {
        assert_ne!(
            answers[relation["left"].as_str().unwrap()],
            answers[relation["right"].as_str().unwrap()]
        );
    }
}

#[test]
fn source_bytes_and_composition_are_exact_and_not_unicode_normalized() {
    let fixture: Value = serde_json::from_str(VECTORS).unwrap();
    let mut sources = BTreeMap::new();
    for source in fixture["sources"].as_array().unwrap() {
        let text = source["utf8"].as_str().unwrap();
        assert_eq!(source["content"]["byte_length"], text.len());
        assert_eq!(
            source["content"]["sha256"],
            format!("sha256:{}", jcs::sha256_hex(text.as_bytes()))
        );
        sources.insert(source["id"].as_str().unwrap(), text);
    }
    let composed = [
        "core",
        "common",
        "role-body",
        "dependency",
        "guidance-two",
        "guidance-one",
    ]
    .map(|name| sources[name])
    .join("\n\n");
    assert_eq!(composed, sources["composition"]);
    assert_ne!(
        jcs::sha256_hex("café".as_bytes()),
        jcs::sha256_hex("cafe\u{301}".as_bytes())
    );
    assert_ne!(jcs::sha256_hex(b"line\n"), jcs::sha256_hex(b"line\r\n"));
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
struct RetainedOperation {
    digest: String,
    state: String,
    subject: String,
    content_available: bool,
    tombstone: bool,
}

#[derive(Default, serde::Deserialize, serde::Serialize)]
struct LookupNamespace {
    operations: BTreeMap<String, RetainedOperation>,
    corrupt: bool,
    unavailable: bool,
    allocations: usize,
}

impl LookupNamespace {
    fn apply(&mut self, request: &Value) -> &'static str {
        let action = request["action"].as_str().unwrap();
        match action {
            "restart" => {
                *self = serde_json::from_slice(&serde_json::to_vec(self).unwrap()).unwrap();
                return "restarted";
            }
            "corrupt" => {
                self.corrupt = true;
                return "corrupt";
            }
            "unavailable" => {
                self.unavailable = true;
                return "unavailable";
            }
            "retire_content" => {
                for op in self.operations.values_mut() {
                    op.content_available = false;
                }
                return "content_retired";
            }
            "delete_subject" => {
                for op in self.operations.values_mut() {
                    op.tombstone = true;
                }
                return "deleted";
            }
            "deadline" => return "unchanged",
            _ => {}
        }
        // The authorized namespace is fixed independently of caller-supplied lookup arguments.
        if request["scope"] != "workspace:a/engagement:a"
            || request["principal"] != "controller:a"
            || request["generation"] != 1
        {
            return "unauthorized";
        }
        if self.corrupt || self.unavailable {
            return "recovery_needed";
        }
        let key = format!(
            "{}:{}",
            request["kind"].as_str().unwrap(),
            request["key"].as_str().unwrap()
        );
        if action == "lookup" {
            return match self
                .operations
                .get(&key)
                .map(|operation| operation.state.as_str())
            {
                Some("accepted") => "accepted",
                Some("not_accepted") => "not_accepted",
                Some("unresolved") => "unresolved",
                None => "absent",
                other => panic!("invalid retained state: {other:?}"),
            };
        }
        let digest = request["digest"].as_str().unwrap();
        if let Some(retained) = self.operations.get(&key) {
            if retained.digest != digest {
                return "idempotency_conflict";
            }
            match retained.state.as_str() {
                "accepted" => return "replayed",
                "unresolved" => return "recovery_needed",
                "not_accepted" => {}
                other => panic!("invalid state {other}"),
            }
        }
        // A revision fence only governs new acceptance, after original receipt replay.
        if request
            .get("expected_revision")
            .is_some_and(|revision| revision != 1)
        {
            return "stale_revision";
        }
        let state = match action {
            "admit" => "accepted",
            "refuse" => "not_accepted",
            "mark_unresolved" => "unresolved",
            other => panic!("invalid operation action {other}"),
        };
        if state == "accepted" {
            self.allocations += 1;
        }
        self.operations.insert(
            key,
            RetainedOperation {
                digest: digest.to_owned(),
                state: state.to_owned(),
                subject: format!("task:{}", self.allocations),
                content_available: true,
                tombstone: false,
            },
        );
        match state {
            "accepted" => "accepted",
            "not_accepted" => "not_accepted",
            _ => "unresolved",
        }
    }
}

fn classify(input: &Value) -> &'static str {
    let detail = &input["detail"];
    let policy: Value = serde_json::from_str(include_str!(
        "../docs/protocol/dolgorae-grpc-error-mapping-v2.json"
    ))
    .unwrap();
    if detail["detail_version"].as_u64() != Some(2)
        || !detail["error_code"]
            .as_str()
            .is_some_and(|code| policy["code_status"].get(code).is_some())
    {
        return "Unknown";
    }
    let class = detail["classification"].as_str().unwrap_or("");
    let effect = detail["effect"].as_str().unwrap_or("");
    let action = detail["required_action"].as_str().unwrap_or("");
    match (class, effect) {
        ("Rejected", "none") => "Rejected",
        ("RecoveryNeeded", "unresolved" | "accepted")
            if [
                "reconcile_operation",
                "recover_run",
                "restore_state",
                "resume_maintenance",
            ]
            .contains(&action) =>
        {
            "RecoveryNeeded"
        }
        ("RetrySameKey", "none")
            if action == "replay_same_key"
                && [Some("not_accepted"), Some("absent")].contains(&input["lookup"].as_str()) =>
        {
            "RetrySameKey"
        }
        _ => "Unknown",
    }
}

fn limits(name: &str) -> (u64, u64) {
    match name {
        "source_bytes" => (1, 65_536),
        "source_alias_bytes" => (1, 256),
        "dependencies" | "guidance" | "findings_per_source" => (0, 64),
        "pool_k" => (1, 16),
        "generation_cap" | "podway_rounds" => (1, 32),
        "group_entries" => (1, 64),
        "group_manifest_bytes" | "artifact_chunk_bytes" => (1, 1_048_576),
        "merged_findings" => (0, 4096),
        "inline_result_bytes" => (0, 1_048_576),
        "merged_artifact_bytes" | "result_artifact_bytes" => (0, 33_554_432),
        "run_artifact_bytes" => (0, 268_435_456),
        "timeline_page_bytes" | "stream_queue_bytes" => (0, 4_194_304),
        "stream_envelopes" => (0, 32),
        "stall_millis" => (0, 5000),
        "ledger_bytes" => (0, 536_870_912),
        "ledger_records" => (0, 1_000_000),
        "gul_streams" => (0, 6),
        name => panic!("unknown bound {name}"),
    }
}

fn execute_case(case: &Value) -> Value {
    let input = &case["input"];
    match case["scenario"].as_str().unwrap() {
        "operation_history" => {
            let mut namespace = LookupNamespace::default();
            let results: Vec<_> = input["actions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|action| namespace.apply(action))
                .collect();
            assert!(
                namespace.allocations <= 1,
                "{} allocated duplicate work",
                case["id"]
            );
            json!(results)
        }
        "classify" => json!(classify(input)),
        "secret_resolution" => {
            let mut receipts = BTreeMap::new();
            let key = input["key"].as_str().unwrap();
            let first = receipts
                .entry(key)
                .or_insert(
                    json!({"key": key, "interaction": "interaction:fixture", "accepted": true}),
                )
                .clone();
            let replay = receipts
                .entry(key)
                .or_insert(json!({"impossible": true}))
                .clone();
            let retained = serde_json::to_string(&receipts).unwrap();
            json!({"replayed": true, "stored_secret": retained.contains(input["first_answer"].as_str().unwrap()) || retained.contains(input["replay_answer"].as_str().unwrap()), "receipt_equal": first == replay})
        }
        "rounds" => {
            let count = input["actions"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|action| {
                    matches!(action.as_str(), Some("accepted" | "unknown" | "synthesis"))
                })
                .count();
            json!({"rounds_used": count, "next_admission": (count as u64) < input["max_rounds"].as_u64().unwrap()})
        }
        "disposition" => json!(
            [Some("idle"), Some("released")].contains(&input["state"].as_str())
                && input["fresh_after_result"] == true
                && input["outstanding"] == 0
        ),
        "merge" => {
            let mut keys: BTreeMap<String, BTreeSet<Vec<u8>>> = BTreeMap::new();
            let mut coverage = BTreeSet::new();
            for source in input["sources"].as_array().unwrap() {
                coverage.insert(source["entry_key"].as_str().unwrap());
                for finding in source["findings"].as_array().unwrap() {
                    let encoded = jcs::canonicalize(
                        &jcs::parse(&serde_json::to_string(finding).unwrap()).unwrap(),
                    )
                    .unwrap();
                    keys.entry(finding["finding_key"].as_str().unwrap().to_owned())
                        .or_default()
                        .insert(encoded);
                }
            }
            let conflicts: Vec<_> = keys
                .iter()
                .filter(|(_, variants)| variants.len() > 1)
                .map(|(key, _)| key)
                .collect();
            json!({"unique_findings": keys.values().map(BTreeSet::len).sum::<usize>(), "conflict_keys": conflicts, "coverage": coverage, "model_calls": 0})
        }
        "await" => {
            let states = input["states"].as_array().unwrap();
            let terminal = states
                .iter()
                .filter(|state| {
                    matches!(
                        state.as_str(),
                        Some("completed" | "failed" | "cancelled" | "expired")
                    )
                })
                .count();
            let satisfied = match input["predicate"].as_str().unwrap() {
                "any" => terminal > 0,
                "all" => terminal == states.len(),
                "k" => input["k"]
                    .as_u64()
                    .is_some_and(|k| k > 0 && k <= states.len() as u64 && terminal as u64 >= k),
                _ => false,
            };
            json!({"satisfied": satisfied, "states": states})
        }
        "lane_admission" => json!(
            input["lane"] == "dedicated"
                || (input["lane"] == "shared_readonly"
                    && input["access"] == "read_only"
                    && input["native_subagents"] == "disabled")
        ),
        "maintenance" => {
            let corrupt = input["corrupt"] == true;
            let busy =
                input["active"] != 0 || input["unknown"] != 0 || input["live_writer"] == true;
            json!({"apply": !corrupt && !busy, "finalization_allowed": !corrupt, "failure": if corrupt {"integrity"} else if busy {"busy"} else if input["capacity_full"] == true {"capacity"} else {"none"}})
        }
        "terminal_proof" => {
            json!(input["terminal_receipt"] == true && input["descendants_absent"] == true)
        }
        "creation_mode" => {
            json!({"mode": input["requested"], "observed_access": input["observed_access"]})
        }
        "heartbeat" => {
            json!({"reconnect": input["gap"] == true || input["stream_ended"] == true, "committed_cursor": input["committed_cursor"]})
        }
        "history" => {
            json!({"snapshot_committed": input["snapshot_ok"], "actions_available": input["snapshot_ok"] == true && input["interactions_ok"] == true && input["aggregate_ok"] == true, "history_degraded": input["timeline_ok"] != true})
        }
        "page_commit" => {
            let mut total = 0;
            let commits: Vec<_> = input["pages"]
                .as_array()
                .unwrap()
                .iter()
                .map(|page| {
                    total += page.as_u64().unwrap();
                    total
                })
                .collect();
            json!({"commits": commits, "refresh": input["refresh_mask"]})
        }
        "stream_interest" => {
            let active = input["progress"] == true || input["pending_interaction"] == true;
            json!({"retain_active_stream": active, "slow_observe": !active && input["lifecycle"] == "settling"})
        }
        "safe_projection" => {
            json!({"command": input["safe_command"], "status": input["status"], "elapsed_millis": input["elapsed_millis"]})
        }
        "complete_content" => {
            let bytes = input["chunks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|chunk| chunk.as_str().unwrap())
                .collect::<String>();
            json!(
                input["accepted_link"] == true
                    && input["content"]["byte_length"] == bytes.len()
                    && input["content"]["sha256"]
                        == format!("sha256:{}", jcs::sha256_hex(bytes.as_bytes()))
            )
        }
        "capability" => json!(input["advertised"] == true),
        "qualification" => json!(
            input["contract_valid"] == true
                && input["actual_consumer"] == true
                && input["final_artifact_bound"] == true
                && input["independent_acceptance"] == true
        ),
        "limit" => {
            let (minimum, maximum) = limits(input["name"].as_str().unwrap());
            let value = input["value"].as_u64().unwrap();
            json!(minimum <= value && value <= maximum)
        }
        other => panic!("unhandled contract case {other}"),
    }
}

#[test]
fn checked_acceptance_histories_recovery_coverage_and_exact_bounds() {
    let fixture: Value = serde_json::from_str(ACCEPTANCE).unwrap();
    assert_eq!(fixture["contract_only"], true);
    assert_eq!(fixture["runtime_acceptance"], false);
    let mut ids = BTreeSet::new();
    let mut families = BTreeSet::new();
    for case in fixture["cases"].as_array().unwrap() {
        assert!(
            ids.insert(case["id"].as_str().unwrap()),
            "duplicate case {}",
            case["id"]
        );
        families.insert(case["family"].as_str().unwrap());
        assert_eq!(execute_case(case), case["expected"], "{}", case["id"]);
        if case["scenario"] == "merge" {
            let mut reordered = case.clone();
            reordered["input"]["sources"]
                .as_array_mut()
                .unwrap()
                .reverse();
            assert_eq!(execute_case(case), execute_case(&reordered));
        }
        if case["scenario"] == "safe_projection" {
            assert!(!execute_case(case).to_string().contains("SECRET-CANARY"));
        }
    }
    assert_eq!(
        families,
        BTreeSet::from([
            "ID", "OP", "LIFE", "OBS", "INT", "DELEG", "FIND", "SCHED", "STORE", "REL"
        ])
    );
    assert_eq!(fixture["feedback_coverage"].as_array().unwrap().len(), 45);
    assert_eq!(
        fixture["gul_foundation_coverage"].as_array().unwrap().len(),
        7
    );
    let mut feedback_ids = BTreeSet::new();
    for row in fixture["feedback_coverage"]
        .as_array()
        .unwrap()
        .iter()
        .chain(fixture["gul_foundation_coverage"].as_array().unwrap())
    {
        assert!(feedback_ids.insert(row["feedback"].as_str().unwrap()));
        for case in row["contract_cases"].as_array().unwrap() {
            assert!(
                ids.contains(case.as_str().unwrap()),
                "unresolved contract case {case}"
            );
        }
    }
}

fn captured_source(bytes: &[u8]) -> Check {
    let text = std::str::from_utf8(bytes).map_err(|_| "invalid_utf8")?;
    if text.is_empty() || text.contains('\0') {
        return Err("invalid_source");
    }
    Ok(())
}

fn source_budget(core: &[u8], sources: &[&[u8]]) -> Check {
    captured_source(core)?;
    let mut total = 0_usize;
    for source in sources {
        captured_source(source)?;
        total = total.checked_add(source.len()).ok_or("capacity")?;
    }
    if total > 65_536 {
        return Err("capacity");
    }
    Ok(())
}

#[test]
fn utf8_source_limits_count_exact_bytes_exclude_core_and_separators() {
    let source = "é".repeat(32_768);
    assert_eq!(source.len(), 65_536);
    assert!(source_budget(&vec![b'c'; 100_000], &[source.as_bytes()]).is_ok());
    let overflow = format!("{source}a");
    assert_eq!(
        source_budget(b"core", &[overflow.as_bytes()]),
        Err("capacity")
    );
    assert!(source_budget(b"core", &[b"a", &vec![b'b'; 65_535]]).is_ok());
    assert_eq!(
        source_budget(b"core", &[b"a", &vec![b'b'; 65_536]]),
        Err("capacity")
    );
    assert_eq!(captured_source(b""), Err("invalid_source"));
    assert_eq!(captured_source(b"a\0b"), Err("invalid_source"));
    assert_eq!(captured_source(&[0xff]), Err("invalid_utf8"));
    assert!(alias(&"é".repeat(128)));
    assert!(!alias(&format!("{}a", "é".repeat(128))));
    assert!(!alias("unsafe\nname"));
}

#[test]
fn largest_bounded_group_manifests_and_referenced_coverage_fit_encoded_pages() {
    let fixtures: Value = serde_json::from_str(ACCEPTANCE).unwrap();
    let proof = &fixtures["size_proofs"];
    let schema: Value = serde_json::from_str(include_str!(
        "../docs/protocol/dolgorae-v014-identities-v1.schema.json"
    ))
    .unwrap();
    let entry_max = schema["$defs"]["group_entry"]["properties"]["entry_key"]["x-maxUtf8Bytes"]
        .as_u64()
        .unwrap() as usize;
    let scope_max = schema["$defs"]["opaque_ref"]["x-maxUtf8Bytes"]
        .as_u64()
        .unwrap() as usize;
    let count_max = schema["$defs"]["group_assignment"]["properties"]["entries"]["maxItems"]
        .as_u64()
        .unwrap() as usize;
    assert_eq!((entry_max, scope_max, count_max), (256, 512, 64));
    let digest = format!("sha256:{}", "f".repeat(64));
    let entries: Vec<_> = (0..count_max).map(|index| json!({"entry_key": format!("{index:02}{}", "\"".repeat(entry_max - 2)), "role_snapshot_digest": digest, "agent_configuration_digest": digest, "accepted_task_digest": digest})).collect();
    let group = json!({"schema": "dolgorae.group-assignment/v1", "scope_kind": "orchestrated_session", "scope_ref": "\"".repeat(scope_max), "entries": entries});
    validate_identity("dolgorae.group-assignment/v1", &group).unwrap();
    let encoded = jcs::canonicalize(&jcs::parse(&group.to_string()).unwrap()).unwrap();
    // JSON escaping never uses more than six encoded bytes per valid UTF-8 input byte.
    // Digest alphabet is fixed ASCII; each other bounded string receives the full 6x bound.
    let entry_skeleton = json!({"entry_key":"", "role_snapshot_digest":digest, "agent_configuration_digest":digest, "accepted_task_digest":digest}).to_string().len();
    let group_skeleton = json!({"schema":"dolgorae.group-assignment/v1", "scope_kind":"orchestrated_session", "scope_ref":"", "entries":[]}).to_string().len();
    let worst_case =
        group_skeleton + scope_max * 6 + count_max * (entry_skeleton + entry_max * 6) + count_max
            - 1;
    assert_eq!(proof["full_group_encoded_upper_bound"], worst_case);
    assert!(encoded.len() <= worst_case);
    assert!(
        worst_case < 1_048_576,
        "full 64-entry upper bound {worst_case}"
    );
    let sources: Vec<_> = (0..64).map(|index| json!({"entry_key":format!("{index:02}{}", "\"".repeat(254)), "worker_ref":"\"".repeat(512), "task_ref":"\"".repeat(512), "role_snapshot_digest":digest, "agent_configuration_digest":digest, "accepted_task_digest":digest, "state":"completed", "result_digest":digest, "safe_failure_code":null})).collect();
    let coverage = json!({"schema":"dolgorae.merged-findings/v1", "algorithm":"merge-exact-findings-v1", "group_assignment_digest":digest, "sources":sources, "merged_output":{"byte_length":33_554_432,"sha256":digest}});
    validate_identity("dolgorae.merged-findings/v1", &coverage).unwrap();
    let coverage_encoded = jcs::canonicalize(&jcs::parse(&coverage.to_string()).unwrap()).unwrap();
    assert!(coverage_encoded.len() < 4_194_304);
    let source_skeleton = json!({"entry_key":"", "worker_ref":"", "task_ref":"", "role_snapshot_digest":digest, "agent_configuration_digest":digest, "accepted_task_digest":digest, "state":"completed", "result_digest":digest, "safe_failure_code":null}).to_string().len();
    let coverage_upper_bound = 1024 + 64 * (source_skeleton + 6 * (256 + 512 + 512 + 128));
    assert_eq!(
        proof["full_coverage_encoded_upper_bound"],
        coverage_upper_bound
    );
    assert!(coverage_upper_bound < 4_194_304);
    assert!(coverage_encoded.len() <= coverage_upper_bound);
    assert_eq!(64 * 64, 4096);
    // All 4096 legal 8192-byte claims alone meet 32 MiB; encoding/evidence pushes over.
    // Refusal must preserve references, never truncate claims into a complete result.
    assert_eq!(4096 * 8192, 33_554_432);
    assert!(4096_u64 * (8192 + 1) > limits("merged_artifact_bytes").1);
    let mut fifty = group.clone();
    fifty["entries"].as_array_mut().unwrap().truncate(50);
    validate_identity("dolgorae.group-assignment/v1", &fifty).unwrap();
}

#[test]
fn checked_schema_and_digest_field_sets_agree_without_reinterpreting_generations() {
    let schema: Value = serde_json::from_str(include_str!(
        "../docs/protocol/dolgorae-v014-identities-v1.schema.json"
    ))
    .unwrap();
    for (domain, definition) in [
        ("dolgorae.role-snapshot/v1", "role_snapshot"),
        ("dolgorae.instruction-identity/v1", "instruction_identity"),
        (
            "dolgorae.admitted-configuration/v1",
            "admitted_configuration",
        ),
        ("dolgorae.accepted-task/v1", "accepted_task"),
        ("dolgorae.mutation-envelope/v1", "mutation_envelope"),
        ("dolgorae.complete-result/v1", "complete_result"),
        ("dolgorae.group-assignment/v1", "group_assignment"),
        ("dolgorae.merged-findings/v1", "merged_findings"),
    ] {
        let definition = &schema["$defs"][definition];
        let expected: BTreeSet<_> = fields(domain).iter().copied().collect();
        let required: BTreeSet<_> = definition["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|name| name.as_str().unwrap())
            .collect();
        let properties: BTreeSet<_> = definition["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(definition["additionalProperties"], false);
        assert_eq!(definition["properties"]["schema"]["const"], domain);
        assert_eq!(expected, required);
        assert_eq!(expected, properties);
    }
}

#[test]
fn external_and_managed_podway_known_answers_preserve_domains_and_distinct_scopes() {
    let fixture: Value = serde_json::from_str(ACCEPTANCE).unwrap();
    let vectors: Value = serde_json::from_str(VECTORS).unwrap();
    let digests: BTreeMap<_, _> = vectors["vectors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|vector| {
            (
                vector["id"].as_str().unwrap(),
                vector["expected_digest"].as_str().unwrap(),
            )
        })
        .collect();
    let exports = fixture["export_vectors"].as_array().unwrap();
    assert_eq!(exports.len(), 2);
    for export in exports {
        closed(
            export,
            &[
                "mapping",
                "adapter_id",
                "adapter_version",
                "binding_ref",
                "primary_host",
                "scope_kind",
                "scope_ref",
                "supervisor_ref",
                "worker_ref",
                "task_ref",
                "role_ref",
                "role_snapshot_digest",
                "agent_configuration_digest",
                "accepted_task_digest",
                "result_digest",
                "rounds_used",
                "worker_disposition",
            ],
        )
        .unwrap();
        for (field, vector) in [
            ("role_snapshot_digest", "role-base"),
            ("agent_configuration_digest", "configuration-base"),
            ("accepted_task_digest", "task-base"),
            ("result_digest", "result-base"),
        ] {
            assert_eq!(export[field], digests[vector]);
        }
        if export["primary_host"] == "external" {
            assert_eq!(export["mapping"], "dolgorae.podway-external/v1");
            assert_eq!(export["scope_kind"], "external_engagement");
        } else {
            assert_eq!(export["primary_host"], "managed");
            assert_eq!(export["mapping"], "dolgorae.podway-managed/v1");
            assert_eq!(export["scope_kind"], "orchestrated_session");
        }
    }
    assert_ne!(exports[0]["scope_ref"], exports[1]["scope_ref"]);
    assert_ne!(digests["role-base"], digests["configuration-base"]);
}

fn verify_artifact<'a>(chunks: impl Iterator<Item = &'a [u8]>, reference: &Value) -> bool {
    let mut length = 0_u64;
    let mut digest = Sha256::new();
    for chunk in chunks {
        if chunk.is_empty() || chunk.len() > 1_048_576 {
            return false;
        }
        length += chunk.len() as u64;
        if length > 33_554_432 {
            return false;
        }
        digest.update(chunk);
    }
    reference["byte_length"] == length
        && reference["sha256"] == format!("sha256:{:x}", digest.finalize())
}

#[test]
fn inline_encoded_pages_and_complete_chunked_artifacts_enforce_separate_byte_limits() {
    let fixtures: Value = serde_json::from_str(ACCEPTANCE).unwrap();
    let reference = &fixtures["size_proofs"]["complete_32mib_artifact"];
    let chunk = vec![b'x'; 1_048_576];
    assert!(verify_artifact(
        std::iter::repeat_n(chunk.as_slice(), 32),
        reference
    ));
    assert!(!verify_artifact(
        std::iter::repeat_n(chunk.as_slice(), 31),
        reference
    ));
    assert!(!verify_artifact(
        std::iter::repeat_n(chunk.as_slice(), 33),
        reference
    ));
    let oversized_chunk = vec![b'x'; 1_048_577];
    assert!(!verify_artifact(
        std::iter::once(oversized_chunk.as_slice()),
        reference
    ));
    let mut changed = chunk.clone();
    changed[0] = b'y';
    assert!(!verify_artifact(
        std::iter::once(changed.as_slice()).chain(std::iter::repeat_n(chunk.as_slice(), 31)),
        reference
    ));
    let inline = "é".repeat(524_288);
    assert_eq!(inline.len(), limits("inline_result_bytes").1 as usize);
    assert!(inline.len() as u64 <= limits("inline_result_bytes").1);
    assert!(format!("{inline}a").len() as u64 > limits("inline_result_bytes").1);
    // One MiB of permitted control bytes expands to over the encoded page budget.
    // Even an inline-size result therefore needs an artifact reference in that page.
    let escaping = json!({"final_response": "\0".repeat(1_048_576)});
    assert!(escaping.to_string().len() > 4_194_304);
    let referenced = json!({"final_response_artifact": reference});
    assert!(referenced.to_string().len() < 4_194_304);
    let overhead = json!({"final_response":""}).to_string().len();
    let page = json!({"final_response":"x".repeat(4_194_304 - overhead)});
    assert_eq!(page.to_string().len(), 4_194_304);
    let page_plus_one = json!({"final_response":"x".repeat(4_194_305 - overhead)});
    assert_eq!(page_plus_one.to_string().len(), 4_194_305);
}

// Contract admission model; production group admission remains TASK-084.
fn capture_group_inputs(
    manifest: &Value,
    sources: &Value,
    files: &BTreeMap<String, String>,
    installed: &Value,
) -> Result<Vec<Value>, &'static str> {
    validate_identity("dolgorae.group-assignment/v1", manifest)?;
    let entries = array(&manifest["entries"])?;
    let sources = array(sources)?;
    if entries.len() != sources.len() || serde_json::to_vec(sources).unwrap().len() > 2_097_152 {
        return Err("group_input_invalid");
    }
    let mut captures = Vec::new();
    for (entry, source) in entries.iter().zip(sources) {
        closed(source, &["entry_key", "absolute_file_path", "content"])?;
        if source["entry_key"] != entry["entry_key"] {
            return Err("group_input_invalid");
        }
        let path = string(&source["absolute_file_path"])?;
        if !path.starts_with("/fixture/workspace/inputs/")
            || path.len() > 4096
            || path.split('/').any(|part| part == "..")
        {
            return Err("group_input_invalid");
        }
        let bytes = files.get(path).ok_or("group_input_missing")?.as_bytes();
        if bytes.len() > 134_217_728
            || source["content"]["byte_length"] != bytes.len()
            || source["content"]["sha256"] != format!("sha256:{}", jcs::sha256_hex(bytes))
        {
            return Err("group_input_invalid");
        }
        let parsed = jcs::parse(std::str::from_utf8(bytes).map_err(|_| "group_input_invalid")?)
            .map_err(|_| "group_input_invalid")?;
        let canonical = jcs::canonicalize(&parsed).map_err(|_| "group_input_invalid")?;
        if canonical != bytes {
            return Err("group_input_invalid");
        }
        let input: Value = serde_json::from_slice(bytes).map_err(|_| "group_input_invalid")?;
        closed(&input, &["schema", "provisioning_ref", "assignment"])?;
        if input["schema"] != "dolgorae.group-task-input/v1"
            || input["provisioning_ref"] != installed["provisioning_ref"]
        {
            return Err("group_input_invalid");
        }
        let assignment = &input["assignment"];
        closed(
            assignment,
            &["task_body", "accepted_task_identity", "deadline_seconds"],
        )?;
        if !(1..=3600).contains(
            &assignment["deadline_seconds"]
                .as_u64()
                .ok_or("group_input_invalid")?,
        ) {
            return Err("group_input_invalid");
        }
        let identity = &assignment["accepted_task_identity"];
        validate_identity("dolgorae.accepted-task/v1", identity)?;
        let body = jcs::canonicalize(
            &jcs::parse(&assignment["task_body"].to_string()).map_err(|_| "group_input_invalid")?,
        )
        .map_err(|_| "group_input_invalid")?;
        if identity["task_body_schema"] != "dolgorae.structured-task/v1"
            || identity["task_body"]["byte_length"] != body.len()
            || identity["task_body"]["sha256"] != format!("sha256:{}", jcs::sha256_hex(&body))
        {
            return Err("group_input_invalid");
        }
        for field in ["role_snapshot_digest", "agent_configuration_digest"] {
            if identity[field] != installed[field] || identity[field] != entry[field] {
                return Err("group_input_invalid");
            }
        }
        if checked_digest("dolgorae.accepted-task/v1", &identity.to_string())?.0
            != entry["accepted_task_digest"]
        {
            return Err("group_input_invalid");
        }
        captures.push(json!({"entry_key":entry["entry_key"],"content":source["content"],"provisioning_ref":input["provisioning_ref"],"bytes":input}));
    }
    Ok(captures)
}

#[test]
fn cold_group_captures_complete_inputs_before_receipt_and_rejects_missing_or_mismatch() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../docs/protocol/dolgorae-gul-consumer-v2.fixtures.json"
    ))
    .unwrap();
    let wire = &fixture["positive"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == "assign_group_request")
        .unwrap()["grpc_json"];
    let supplied = array(&fixture["group_input_fixtures"]["sources"]).unwrap();
    let files: BTreeMap<String, String> = supplied
        .iter()
        .map(|source| {
            (
                source["source"]["absolute_file_path"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
                source["utf8"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let installed = &supplied[0]["installed_provisioning"];
    let captures =
        capture_group_inputs(&wire["assignment"], &wire["inputs"], &files, installed).unwrap();
    assert_eq!(captures.len(), 2);
    // A cold group needs no prior member or individual Task acceptance.
    for count in [1, 50, 64] {
        let mut manifest = wire["assignment"].clone();
        let mut entries = Vec::new();
        let mut sources = Vec::new();
        let mut cold_files = BTreeMap::new();
        for index in 0..count {
            let key = format!("cold-{index:03}");
            let mut entry = manifest["entries"][0].clone();
            entry["entry_key"] = json!(key);
            let mut source = wire["inputs"][0].clone();
            source["entry_key"] = json!(key);
            let path = format!("/fixture/workspace/inputs/{key}.json");
            source["absolute_file_path"] = json!(path);
            let mut input: Value =
                serde_json::from_str(supplied[0]["utf8"].as_str().unwrap()).unwrap();
            input["assignment"]["task_body"]["brief"] = json!(format!("Complete new task {key}."));
            let body = jcs::canonicalize(
                &jcs::parse(&input["assignment"]["task_body"].to_string()).unwrap(),
            )
            .unwrap();
            input["assignment"]["accepted_task_identity"]["task_body"] = json!({"byte_length":body.len(),"sha256":format!("sha256:{}",jcs::sha256_hex(&body))});
            entry["accepted_task_digest"] = json!(
                checked_digest(
                    "dolgorae.accepted-task/v1",
                    &input["assignment"]["accepted_task_identity"].to_string()
                )
                .unwrap()
                .0
            );
            let bytes = jcs::canonicalize(&jcs::parse(&input.to_string()).unwrap()).unwrap();
            source["content"] = json!({"byte_length":bytes.len(),"sha256":format!("sha256:{}",jcs::sha256_hex(&bytes))});
            cold_files.insert(path, String::from_utf8(bytes).unwrap());
            entries.push(entry);
            sources.push(source);
        }
        manifest["entries"] = json!(entries);
        let captures =
            capture_group_inputs(&manifest, &json!(sources), &cold_files, installed).unwrap();
        assert_eq!(captures.len(), count);
        // Committed captures remain materializable after source removal and restart.
        cold_files.clear();
        assert!(capture_group_inputs(&manifest, &json!(sources), &cold_files, installed).is_err());
        for capture in captures {
            assert_eq!(
                capture["bytes"]["assignment"]["accepted_task_identity"]["role_snapshot_digest"],
                installed["role_snapshot_digest"]
            );
        }
    }
    let mut missing = files.clone();
    missing.remove(wire["inputs"][0]["absolute_file_path"].as_str().unwrap());
    assert_eq!(
        capture_group_inputs(&wire["assignment"], &wire["inputs"], &missing, installed)
            .unwrap_err(),
        "group_input_missing"
    );
    for bad in 0..6 {
        let mut sources = wire["inputs"].clone();
        let mut manifest = wire["assignment"].clone();
        let mut selected = files.clone();
        match bad {
            0 => {
                sources.as_array_mut().unwrap().pop();
            }
            1 => sources[1] = sources[0].clone(),
            2 => {
                manifest["entries"][0]["accepted_task_digest"] =
                    json!(format!("sha256:{}", "f".repeat(64)))
            }
            3 => sources[0]["content"]["byte_length"] = json!(1),
            4 => sources[0]["absolute_file_path"] = json!("/fixture/workspace/inputs/../secret"),
            _ => {
                let path = sources[0]["absolute_file_path"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                selected.insert(path, "{\"schema\":1,\"schema\":2}".to_owned());
            }
        }
        assert!(
            capture_group_inputs(&manifest, &sources, &selected, installed).is_err(),
            "bad case{bad}"
        );
    }
    // Changed bytes cannot hide behind a recomputed outer hash: task identity is checked.
    let mut corrupted: Value = serde_json::from_str(supplied[0]["utf8"].as_str().unwrap()).unwrap();
    corrupted["assignment"]["task_body"]["brief"] = json!("changed");
    let changed = jcs::canonicalize(&jcs::parse(&corrupted.to_string()).unwrap()).unwrap();
    let mut sources = wire["inputs"].clone();
    sources[0]["content"] = json!({"byte_length":changed.len(),"sha256":format!("sha256:{}",jcs::sha256_hex(&changed))});
    let mut changed_files = files.clone();
    changed_files.insert(
        sources[0]["absolute_file_path"]
            .as_str()
            .unwrap()
            .to_owned(),
        String::from_utf8(changed).unwrap(),
    );
    assert!(
        capture_group_inputs(&wire["assignment"], &sources, &changed_files, installed).is_err()
    );
    let mut foreign = installed.clone();
    foreign["agent_configuration_digest"] = json!(format!("sha256:{}", "e".repeat(64)));
    assert!(capture_group_inputs(&wire["assignment"], &wire["inputs"], &files, &foreign).is_err());
    // Six escaped bytes per UTF-8 byte covers the original rich task limits.
    let task_upper_bound =
        65_536 + 64 * (128 + 262_144 + 4096) + 64 * (128 + 8192 + 64 * 128) + 128;
    let schema: Value = serde_json::from_str(include_str!(
        "../docs/protocol/dolgorae-external-specialist-facade-v5.schema.json"
    ))
    .unwrap();
    let definitions = &schema["$defs"];
    let capture_limit = definitions["group_task_input"]["x-maxEncodedBytes"]
        .as_u64()
        .unwrap();
    let inputs = &definitions["assign_group_request"]["properties"]["inputs"];
    let path_limit =
        definitions["group_task_source"]["properties"]["absolute_file_path"]["x-maxUtf8Bytes"]
            .as_u64()
            .unwrap();
    assert!(6 * task_upper_bound + 1_048_576 < capture_limit);
    assert!(
        inputs["maxItems"].as_u64().unwrap() * (6 * (256 + path_limit) + 1024)
            < inputs["x-maxEncodedBytes"].as_u64().unwrap()
    );
}
