//! Pure-Rust JSON Schema subset validator for drivers with `native_output_schema == false`.
//!
//! Supports `type`, `required`, `properties`, `items`, `enum`, `const`, and
//! `additionalProperties`. Mismatches record `{ validated: false, errors }`
//! without failing the underlying turn.

use serde_json::Value;

use crate::types::SchemaValidationResult;

/// Validates `instance` against a JSON Schema subset `schema`.
pub fn validate_json_schema(schema: &Value, instance: &Value) -> SchemaValidationResult {
    let mut errors = Vec::new();
    validate_node(schema, instance, "$", &mut errors);
    SchemaValidationResult {
        validated: errors.is_empty(),
        errors,
    }
}

fn validate_node(schema: &Value, instance: &Value, path: &str, errors: &mut Vec<String>) {
    match schema {
        Value::Bool(true) => return,
        Value::Bool(false) => {
            errors.push(format!("{path}: schema forbids any value"));
            return;
        }
        Value::Object(_) => {}
        _ => return,
    }
    let Some(obj) = schema.as_object() else {
        return;
    };

    if let Some(expected_const) = obj.get("const")
        && instance != expected_const
    {
        errors.push(format!("{path}: expected constant value {expected_const}"));
    }

    if let Some(enum_vals) = obj.get("enum").and_then(Value::as_array)
        && !enum_vals.iter().any(|v| v == instance)
    {
        errors.push(format!("{path}: value is not in allowed enum set"));
    }

    if let Some(type_val) = obj.get("type")
        && !matches_type_spec(type_val, instance)
    {
        errors.push(format!(
            "{path}: expected type {type_val}, got {}",
            json_type_name(instance)
        ));
        return;
    }

    if let Some(inst_obj) = instance.as_object() {
        if let Some(req) = obj.get("required").and_then(Value::as_array) {
            for field in req.iter().filter_map(Value::as_str) {
                if !inst_obj.contains_key(field) {
                    errors.push(format!("{path}: missing required property '{field}'"));
                }
            }
        }

        let props = obj.get("properties").and_then(Value::as_object);
        if let Some(props_map) = props {
            for (k, sub_schema) in props_map {
                if let Some(val) = inst_obj.get(k) {
                    validate_node(sub_schema, val, &format!("{path}.{k}"), errors);
                }
            }
        }

        if let Some(additional) = obj.get("additionalProperties") {
            for (k, val) in inst_obj {
                if props.is_some_and(|p| p.contains_key(k)) {
                    continue;
                }
                match additional {
                    Value::Bool(true) => {}
                    Value::Bool(false) => {
                        errors.push(format!("{path}.{k}: additional property not allowed"));
                    }
                    sub_schema @ Value::Object(_) => {
                        validate_node(sub_schema, val, &format!("{path}.{k}"), errors);
                    }
                    _ => {}
                }
            }
        }
    }

    if let Some(inst_arr) = instance.as_array()
        && let Some(items_schema) = obj.get("items")
    {
        for (idx, elem) in inst_arr.iter().enumerate() {
            validate_node(items_schema, elem, &format!("{path}[{idx}]"), errors);
        }
    }
}

fn matches_type_spec(type_spec: &Value, instance: &Value) -> bool {
    match type_spec {
        Value::String(t) => matches_single_type(t, instance),
        Value::Array(list) => list
            .iter()
            .filter_map(Value::as_str)
            .any(|t| matches_single_type(t, instance)),
        _ => true,
    }
}

fn matches_single_type(expected: &str, instance: &Value) -> bool {
    match expected {
        "null" => instance.is_null(),
        "boolean" => instance.is_boolean(),
        "integer" => instance.as_i64().is_some() || instance.as_u64().is_some(),
        "number" => instance.is_number(),
        "string" => instance.is_string(),
        "array" => instance.is_array(),
        "object" => instance.is_object(),
        _ => true,
    }
}

fn json_type_name(val: &Value) -> &'static str {
    match val {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
