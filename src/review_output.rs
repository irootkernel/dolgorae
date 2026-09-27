//! Checked v3 output structure and bounded diagnostics shared by both review paths.

use crate::jcs::sha256_hex;
use crate::machine::MachineError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::OnceLock;

pub(crate) const MAX_OUTPUT_BYTES: usize = 1_048_576;
const TOP_LEVEL_KEYS: [&str; 5] = [
    "criterion_assessments",
    "evidence_limits",
    "findings",
    "overall_assessment",
    "summary",
];

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewOutputDiagnostic {
    pub schema: String,
    pub category: String,
    pub path: String,
    pub output_sha256: Option<String>,
    pub output_bytes: Option<u64>,
    pub known_top_level_keys: Vec<String>,
    pub unknown_top_level_key_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ReviewExecution>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewExecution {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<uuid::Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<uuid::Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
}

pub(crate) fn output_contract() -> &'static Value {
    static CONTRACT: OnceLock<Value> = OnceLock::new();
    CONTRACT.get_or_init(|| {
        let v3: Value = serde_json::from_str(include_str!(
            "../docs/protocol/dolgorae-specialist-review-tool-v3.schema.json"
        ))
        .expect("checked v3 schema");
        let v1: Value = serde_json::from_str(include_str!(
            "../docs/protocol/dolgorae-specialist-review-tool-v1.schema.json"
        ))
        .expect("checked finding schema");
        expand(&v3["$defs"]["verdict"], &v3, &v1)
    })
}

fn expand(value: &Value, v3: &Value, v1: &Value) -> Value {
    if let Some(reference) = value.get("$ref").and_then(Value::as_str) {
        let (root, pointer) = if let Some(pointer) = reference.strip_prefix('#') {
            (v3, pointer)
        } else {
            (
                v1,
                reference
                    .strip_prefix("https://dolgorae.local/schema/specialist-review-tool/v1#")
                    .expect("output contract references only the checked finding schema"),
            )
        };
        return expand(
            root.pointer(pointer).expect("checked output reference"),
            v3,
            v1,
        );
    }
    match value {
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| (key.clone(), expand(value, v3, v1)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(|v| expand(v, v3, v1)).collect()),
        _ => value.clone(),
    }
}

pub(crate) fn invalid_output(
    text: Option<&str>,
    value: Option<&Value>,
    category: &str,
    path: &str,
) -> MachineError {
    let keys = value.and_then(Value::as_object);
    let known_top_level_keys = TOP_LEVEL_KEYS
        .iter()
        .filter(|key| keys.is_some_and(|keys| keys.contains_key(**key)))
        .map(|key| (*key).to_owned())
        .collect::<Vec<_>>();
    let diagnostic = ReviewOutputDiagnostic {
        schema: "dolgorae-review-output-diagnostic/v1".to_owned(),
        category: category.to_owned(),
        path: path.to_owned(),
        output_sha256: text.map(|text| format!("sha256:{}", sha256_hex(text.as_bytes()))),
        output_bytes: text.map(|text| text.len() as u64),
        unknown_top_level_key_count: keys.map_or(0, |keys| keys.len() - known_top_level_keys.len()),
        known_top_level_keys,
        execution: None,
    };
    MachineError::new(
        "REVIEW_OUTPUT_INVALID",
        "Reviewer output does not match the checked v3 contract",
        false,
        json!({"reason":category,"required_action":"none","diagnostic":diagnostic}),
    )
}

pub(crate) fn oversized_response(byte_length: u64, sha256: &str) -> MachineError {
    let mut error = invalid_output(None, None, "output_too_large", "");
    error.details["diagnostic"]["output_bytes"] = json!(byte_length);
    error.details["diagnostic"]["output_sha256"] = json!(format!("sha256:{sha256}"));
    error
}

pub(crate) fn execution_fields(mut execution: ReviewExecution) -> serde_json::Map<String, Value> {
    fn bounded(value: Option<String>, limit: usize) -> Option<String> {
        value.filter(|value| {
            !value.is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
        })
    }
    execution.model = bounded(execution.model, 128);
    execution.effort = bounded(execution.effort, 64);
    execution.codex_version = bounded(execution.codex_version, 64);
    execution.turn_id = bounded(execution.turn_id, 128);
    json!(execution)
        .as_object()
        .expect("execution serializes")
        .clone()
}

pub(crate) fn attach_execution(error: &mut MachineError, execution: ReviewExecution) {
    let fields = execution_fields(execution);
    if fields.is_empty() {
        return;
    }
    if let Some(diagnostic) = error
        .details
        .get_mut("diagnostic")
        .and_then(Value::as_object_mut)
    {
        let current = diagnostic.entry("execution").or_insert_with(|| json!({}));
        current
            .as_object_mut()
            .expect("checked execution object")
            .extend(fields);
    }
}

pub(crate) fn diagnostic(error: &MachineError) -> Option<ReviewOutputDiagnostic> {
    if error.code != "REVIEW_OUTPUT_INVALID" {
        return None;
    }
    error
        .details
        .get("diagnostic")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

pub(crate) fn check_structure(value: &Value) -> Result<(), (&'static str, String)> {
    check(value, output_contract(), "")
}

fn matches_type(value: &Value, kind: &str) -> bool {
    match kind {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "null" => value.is_null(),
        "integer" => value.is_u64() || value.is_i64(),
        _ => unreachable!("checked output schema type"),
    }
}

fn check(value: &Value, schema: &Value, path: &str) -> Result<(), (&'static str, String)> {
    let fail = |category| Err((category, path.to_owned()));
    if let Some(choices) = schema.get("oneOf").and_then(Value::as_array) {
        let selected = choices.iter().find(|s| {
            s.get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| matches_type(value, kind))
        });
        return selected.map_or_else(|| fail("wrong_type"), |s| check(value, s, path));
    }
    if let Some(kind) = schema.get("type") {
        let matches = match kind {
            Value::String(kind) => matches_type(value, kind),
            Value::Array(kinds) => kinds
                .iter()
                .any(|k| matches_type(value, k.as_str().expect("type"))),
            _ => unreachable!("checked output schema type declaration"),
        };
        if !matches {
            return fail("wrong_type");
        }
    }
    if let Some(options) = schema.get("enum").and_then(Value::as_array) {
        if !value.is_string() {
            return fail("wrong_type");
        }
        if !options.contains(value) {
            return fail("invalid_enum");
        }
    }
    if let Some(fields) = value.as_object() {
        let properties = schema["properties"]
            .as_object()
            .expect("checked object properties");
        for field in schema["required"]
            .as_array()
            .expect("checked required fields")
        {
            let field = field.as_str().expect("checked field");
            if !fields.contains_key(field) {
                return Err(("missing_field", format!("{path}/{field}")));
            }
        }
        if fields.keys().any(|key| !properties.contains_key(key)) {
            return fail("unknown_field");
        }
        for (key, property) in properties {
            if let Some(value) = fields.get(key) {
                check(value, property, &format!("{path}/{key}"))?;
            }
        }
    }
    if let Some(items) = value.as_array() {
        if schema
            .get("maxItems")
            .and_then(Value::as_u64)
            .is_some_and(|max| items.len() as u64 > max)
            || schema
                .get("minItems")
                .and_then(Value::as_u64)
                .is_some_and(|min| (items.len() as u64) < min)
        {
            return fail("semantic_constraint");
        }
        for (index, value) in items.iter().enumerate() {
            check(value, &schema["items"], &format!("{path}/{index}"))?;
        }
    }
    if let Some(text) = value.as_str()
        && (text.contains('\0')
            || schema
                .get("minLength")
                .and_then(Value::as_u64)
                .is_some_and(|min| (text.chars().count() as u64) < min)
            || schema
                .get("x-maxUtf8Bytes")
                .and_then(Value::as_u64)
                .is_some_and(|max| text.len() as u64 > max))
    {
        return fail("semantic_constraint");
    }
    if value.is_number()
        && schema
            .get("minimum")
            .and_then(Value::as_u64)
            .is_some_and(|min| value.as_u64().is_none_or(|number| number < min))
    {
        return fail("semantic_constraint");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::specialist::{parse_reviewer_output_v3, validate_reviewer_output_v3};
    use crate::task_request::{
        STRUCTURED_REVIEW_OUTPUT, SpecialistTaskRequest, TaskContext, TaskCriterion,
    };

    fn task() -> SpecialistTaskRequest {
        SpecialistTaskRequest {
            purpose: "completion".to_owned(),
            brief: "Check the accepted behavior.".to_owned(),
            contexts: vec![TaskContext {
                id: "specification".to_owned(),
                content: "The command prints Hello, world!.".to_owned(),
                provenance: "accepted specification".to_owned(),
            }],
            criteria: vec![TaskCriterion {
                id: "C-1".to_owned(),
                statement: "Print Hello, world!.".to_owned(),
                source_context_ids: vec!["specification".to_owned()],
            }],
            expected_output: STRUCTURED_REVIEW_OUTPUT.to_owned(),
        }
    }

    fn report() -> Value {
        json!({
            "summary": "The candidate does not satisfy the criterion.",
            "findings": [{
                "severity": "P2", "title": "Add the missing comma",
                "description": "The printed greeting lacks a comma.",
                "path": null, "line_start": null, "line_end": null,
                "recommendation": "Print the accepted greeting.", "confidence": "high"
            }],
            "criterion_assessments": [{
                "criterion_id": "C-1", "status": "unmet",
                "explanation": "The output differs from the accepted greeting.",
                "evidence": [{
                    "basis": "candidate", "description": "The greeting lacks a comma.",
                    "path": null, "line_start": null, "line_end": null, "context_id": null
                }],
                "remaining_gap": null
            }],
            "evidence_limits": [], "overall_assessment": "requirements_not_met"
        })
    }

    fn rejected(value: Value, category: &str, path: &str) -> ReviewOutputDiagnostic {
        let error = validate_reviewer_output_v3(value, &task()).unwrap_err();
        assert_eq!(error.code, "REVIEW_OUTPUT_INVALID");
        assert!(!error.retryable);
        assert_eq!(error.details["required_action"], "none");
        let diagnostic = diagnostic(&error).unwrap();
        assert_eq!(diagnostic.category, category);
        assert_eq!(diagnostic.path, path);
        diagnostic
    }

    #[test]
    fn required_nullable_members_must_be_present_at_every_output_level() {
        validate_reviewer_output_v3(report(), &task()).unwrap();
        for (parent, fields) in [
            (
                "",
                vec![
                    "summary",
                    "findings",
                    "criterion_assessments",
                    "evidence_limits",
                    "overall_assessment",
                ],
            ),
            (
                "/findings/0",
                vec![
                    "severity",
                    "title",
                    "description",
                    "path",
                    "line_start",
                    "line_end",
                    "recommendation",
                    "confidence",
                ],
            ),
            (
                "/criterion_assessments/0",
                vec![
                    "criterion_id",
                    "status",
                    "explanation",
                    "evidence",
                    "remaining_gap",
                ],
            ),
            (
                "/criterion_assessments/0/evidence/0",
                vec![
                    "basis",
                    "description",
                    "path",
                    "line_start",
                    "line_end",
                    "context_id",
                ],
            ),
        ] {
            for field in fields {
                let mut value = report();
                value
                    .pointer_mut(parent)
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .remove(field);
                rejected(value, "missing_field", &format!("{parent}/{field}"));
            }
        }
    }

    #[test]
    fn structural_failures_identify_only_contract_owned_paths() {
        for (path, replacement, category) in [
            (
                "/overall_assessment",
                json!("provider-secret-enum"),
                "invalid_enum",
            ),
            ("/findings/0/severity", json!("urgent"), "invalid_enum"),
            ("/findings/0/confidence", json!(false), "wrong_type"),
            ("/findings/0/path", json!([]), "wrong_type"),
            (
                "/criterion_assessments/0/status",
                json!("complete"),
                "invalid_enum",
            ),
            (
                "/criterion_assessments/0/evidence/0/basis",
                json!("host"),
                "invalid_enum",
            ),
            (
                "/criterion_assessments/0/evidence/0/line_start",
                json!(1.5),
                "wrong_type",
            ),
            (
                "/criterion_assessments/0/evidence/0/line_start",
                json!(0),
                "semantic_constraint",
            ),
            (
                "/criterion_assessments/0/remaining_gap",
                json!(42),
                "wrong_type",
            ),
            ("/summary", json!(""), "semantic_constraint"),
            ("/summary", json!("가".repeat(2731)), "semantic_constraint"),
        ] {
            let mut value = report();
            *value.pointer_mut(path).unwrap() = replacement;
            rejected(value, category, path);
        }
    }

    #[test]
    fn unknown_fields_and_provider_content_never_enter_diagnostics() {
        let key = "secret-key~/credential";
        let secret = "provider-private-content-canary";
        for parent in ["", "/findings/0", "/criterion_assessments/0/evidence/0"] {
            let mut value = report();
            value["summary"] = secret.into();
            value
                .pointer_mut(parent)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert(key.to_owned(), json!(secret));
            let error = validate_reviewer_output_v3(value, &task()).unwrap_err();
            let wire = serde_json::to_string(&error).unwrap();
            assert!(!wire.contains(key));
            assert!(!wire.contains(secret));
            let diagnostic = diagnostic(&error).unwrap();
            assert_eq!(diagnostic.category, "unknown_field");
            assert_eq!(diagnostic.path, parent);
            assert_eq!(diagnostic.known_top_level_keys, TOP_LEVEL_KEYS);
            assert_eq!(
                diagnostic.unknown_top_level_key_count,
                usize::from(parent.is_empty())
            );
        }
    }

    #[test]
    fn diagnostics_hash_original_utf8_bytes_without_reserializing() {
        let text = "  {\n  \"summary\" : \"private-가\"\n}\r\n";
        let error = parse_reviewer_output_v3(Some(text), &task()).unwrap_err();
        let diagnostic = diagnostic(&error).unwrap();
        assert_eq!(diagnostic.category, "missing_field");
        assert_eq!(diagnostic.path, "/findings");
        assert_eq!(diagnostic.output_bytes, Some(text.len() as u64));
        assert_eq!(
            diagnostic.output_sha256,
            Some(format!("sha256:{}", sha256_hex(text.as_bytes())))
        );
        assert_eq!(diagnostic.known_top_level_keys, ["summary"]);
        assert!(
            !serde_json::to_string(&error)
                .unwrap()
                .contains("private-가")
        );
        let normalized =
            serde_json::to_string(&serde_json::from_str::<Value>(text).unwrap()).unwrap();
        assert_ne!(
            sha256_hex(text.as_bytes()),
            sha256_hex(normalized.as_bytes())
        );
    }

    #[test]
    fn malformed_duplicate_fenced_and_oversized_outputs_have_bounded_errors() {
        let oversized = "private-provider-text".repeat(MAX_OUTPUT_BYTES / 21 + 1);
        assert!(oversized.len() > MAX_OUTPUT_BYTES);
        for (text, category) in [
            ("private-provider-text", "invalid_json"),
            ("{\"summary\":", "invalid_json"),
            (
                "{\"summary\":\"first\",\"summary\":\"private-provider-text\"}",
                "invalid_json",
            ),
            ("```json\n{}\n```", "invalid_json"),
            (oversized.as_str(), "output_too_large"),
        ] {
            let error = parse_reviewer_output_v3(Some(text), &task()).unwrap_err();
            let diagnostic = diagnostic(&error).unwrap();
            assert_eq!(diagnostic.category, category);
            assert_eq!(diagnostic.path, "");
            assert_eq!(diagnostic.output_bytes, Some(text.len() as u64));
            assert_eq!(
                diagnostic.output_sha256,
                Some(format!("sha256:{}", sha256_hex(text.as_bytes())))
            );
            assert!(diagnostic.known_top_level_keys.is_empty());
            let wire = serde_json::to_string(&error).unwrap();
            assert!(!wire.contains("private-provider-text"));
            assert!(wire.len() < 1024);
        }
        let error = parse_reviewer_output_v3(None, &task()).unwrap_err();
        let diagnostic = diagnostic(&error).unwrap();
        assert_eq!(diagnostic.category, "output_unavailable");
        assert_eq!(diagnostic.output_sha256, None);
        assert_eq!(diagnostic.output_bytes, None);
    }

    #[test]
    fn semantic_failures_keep_criterion_and_evidence_values_private() {
        for (path, replacement, diagnostic_path) in [
            (
                "/criterion_assessments/0/criterion_id",
                json!("private-criterion"),
                "/criterion_assessments/0/criterion_id",
            ),
            (
                "/criterion_assessments/0/evidence/0/path",
                json!("../private-file"),
                "/criterion_assessments/0/evidence/0",
            ),
            (
                "/criterion_assessments/0/evidence/0/context_id",
                json!("private-context"),
                "/criterion_assessments/0/evidence/0",
            ),
            (
                "/overall_assessment",
                json!("requirements_met"),
                "/overall_assessment",
            ),
        ] {
            let mut value = report();
            *value.pointer_mut(path).unwrap() = replacement;
            let diagnostic = rejected(value, "semantic_constraint", diagnostic_path);
            assert!(
                !serde_json::to_string(&diagnostic)
                    .unwrap()
                    .contains("private-")
            );
        }
    }

    #[test]
    fn prompt_embeds_the_complete_checked_contract_without_external_references() {
        fn assert_supported_schema(schema: &Value) {
            for (keyword, value) in schema.as_object().unwrap() {
                match keyword.as_str() {
                    "properties" => {
                        for child in value.as_object().unwrap().values() {
                            assert_supported_schema(child);
                        }
                    }
                    "items" => assert_supported_schema(value),
                    "oneOf" => {
                        let mut types = std::collections::BTreeSet::new();
                        for branch in value.as_array().unwrap() {
                            assert!(types.insert(branch["type"].as_str().unwrap()));
                            assert_supported_schema(branch);
                        }
                    }
                    "additionalProperties" => assert_eq!(value, false),
                    "type" | "enum" | "required" | "minItems" | "maxItems" | "minLength"
                    | "x-maxUtf8Bytes" | "minimum" | "description" => {}
                    other => panic!("output validator does not implement schema keyword {other}"),
                }
            }
        }
        let v1: Value = serde_json::from_str(include_str!(
            "../docs/protocol/dolgorae-specialist-review-tool-v1.schema.json"
        ))
        .unwrap();
        let v3: Value = serde_json::from_str(include_str!(
            "../docs/protocol/dolgorae-specialist-review-tool-v3.schema.json"
        ))
        .unwrap();
        let contract = output_contract();
        assert_supported_schema(contract);
        assert_eq!(
            contract["properties"]["findings"]["items"],
            v1["$defs"]["finding"]
        );
        assert_eq!(
            contract["properties"]["criterion_assessments"]["items"]["properties"]["evidence"]["items"],
            v3["$defs"]["assessment_evidence"]
        );
        assert_eq!(contract["required"], v3["$defs"]["verdict"]["required"]);
        let serialized = serde_json::to_string(contract).unwrap();
        assert!(!serialized.contains("\"$ref\""));
        assert!(task().prompt().unwrap().contains(&serialized));
    }

    #[test]
    fn diagnostic_key_allowlist_matches_the_checked_output_contract() {
        let schema: Value = serde_json::from_str(include_str!(
            "../docs/protocol/dolgorae-review-output-diagnostic-v1.schema.json"
        ))
        .unwrap();
        let keys = &schema["properties"]["known_top_level_keys"];
        let contract = output_contract();
        let properties = contract["properties"].as_object().unwrap();
        let mut property_names = properties.keys().map(String::as_str).collect::<Vec<_>>();
        property_names.sort_unstable();
        assert_eq!(property_names, TOP_LEVEL_KEYS);
        for values in [&contract["required"], &keys["items"]["enum"]] {
            let mut names = values
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap())
                .collect::<Vec<_>>();
            names.sort_unstable();
            assert_eq!(names, TOP_LEVEL_KEYS);
        }
        assert_eq!(
            keys["maxItems"].as_u64().unwrap(),
            TOP_LEVEL_KEYS.len() as u64
        );
    }

    #[test]
    fn execution_metadata_accumulates_without_creating_empty_metadata() {
        let mut error = invalid_output(None, None, "output_unavailable", "");
        attach_execution(&mut error, ReviewExecution::default());
        assert!(diagnostic(&error).unwrap().execution.is_none());
        let run_id = uuid::Uuid::now_v7();
        attach_execution(
            &mut error,
            ReviewExecution {
                model: Some("gpt-6-sol".to_owned()),
                effort: Some("high".to_owned()),
                ..ReviewExecution::default()
            },
        );
        attach_execution(
            &mut error,
            ReviewExecution {
                run_id: Some(run_id),
                ..ReviewExecution::default()
            },
        );
        let execution = diagnostic(&error).unwrap().execution.unwrap();
        assert_eq!(execution.model.as_deref(), Some("gpt-6-sol"));
        assert_eq!(execution.effort.as_deref(), Some("high"));
        assert_eq!(execution.run_id, Some(run_id));
    }
    #[test]
    fn execution_metadata_omits_unbounded_or_control_bearing_identities() {
        let mut error = invalid_output(None, None, "output_unavailable", "");
        attach_execution(
            &mut error,
            ReviewExecution {
                model: Some("m".repeat(129)),
                effort: Some("secret\neffort".to_owned()),
                codex_version: Some(String::new()),
                turn_id: Some("turn\0secret".to_owned()),
                ..Default::default()
            },
        );
        assert!(diagnostic(&error).unwrap().execution.is_none());
        let task_id = uuid::Uuid::now_v7();
        attach_execution(
            &mut error,
            ReviewExecution {
                model: Some("gpt-6-sol".to_owned()),
                task_id: Some(task_id),
                ..Default::default()
            },
        );
        let execution = diagnostic(&error).unwrap().execution.unwrap();
        assert_eq!(execution.model.as_deref(), Some("gpt-6-sol"));
        assert_eq!(execution.task_id, Some(task_id));
    }
}
