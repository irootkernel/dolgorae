//! Checked research output. Validation does not resolve claims or verify their truth.

use crate::machine::MachineError;
use serde_json::{Value, json};
use std::sync::OnceLock;

pub(crate) const SCHEMA: &str = "structured_findings_v1";
const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_INTEGER: u64 = 9_007_199_254_740_991;

fn invalid() -> MachineError {
    MachineError::new(
        "FINDINGS_OUTPUT_INVALID",
        "Research output does not match structured_findings_v1",
        false,
        json!({"required_action":"none"}),
    )
}

fn fields(value: &Value, keys: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|map| map.len() == keys.len() && keys.iter().all(|key| map.contains_key(*key)))
}

fn text(value: &Value, maximum: usize) -> bool {
    value
        .as_str()
        .is_some_and(|text| !text.is_empty() && text.len() <= maximum)
}

fn strings(value: &Value, maximum: usize, minimum: usize, unique: bool) -> bool {
    value.as_array().is_some_and(|items| {
        (minimum..=64).contains(&items.len())
            && items.iter().enumerate().all(|(index, item)| {
                text(item, maximum) && (!unique || !items[..index].contains(item))
            })
    })
}

fn choice(value: &Value, choices: &[&str]) -> bool {
    value.as_str().is_some_and(|text| choices.contains(&text))
}

fn git_object(value: &Value) -> bool {
    value.as_str().is_some_and(|text| {
        matches!(text.len(), 40 | 64)
            && text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn bytes_ref(value: &Value) -> bool {
    fields(value, &["byte_length", "sha256"])
        && value["byte_length"]
            .as_u64()
            .is_some_and(|length| length <= MAX_INTEGER)
        && value["sha256"].as_str().is_some_and(|text| {
            text.strip_prefix("sha256:").is_some_and(|hash| {
                hash.len() == 64
                    && hash
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
        })
}

fn uri(value: &Value) -> bool {
    fn unreserved_or_subdelimiter(byte: u8) -> bool {
        byte.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=".contains(&byte)
    }
    fn component(value: &str, extra: &[u8]) -> bool {
        value
            .bytes()
            .all(|byte| unreserved_or_subdelimiter(byte) || byte == b'%' || extra.contains(&byte))
    }
    fn port(value: &str) -> bool {
        value.is_empty()
            || value
                .strip_prefix(':')
                .is_some_and(|port| port.bytes().all(|byte| byte.is_ascii_digit()))
    }
    fn authority(value: &str) -> bool {
        let host_port = if let Some((user, host_port)) = value.rsplit_once('@') {
            if !component(user, b":") {
                return false;
            }
            host_port
        } else {
            value
        };
        if let Some(literal) = host_port.strip_prefix('[') {
            let Some((host, suffix)) = literal.split_once(']') else {
                return false;
            };
            let future = host.strip_prefix(['v', 'V']).is_some_and(|future| {
                future.split_once('.').is_some_and(|(version, address)| {
                    !version.is_empty()
                        && version.bytes().all(|byte| byte.is_ascii_hexdigit())
                        && !address.is_empty()
                        && component(address, b":")
                        && !address.contains('%')
                })
            });
            (host.parse::<std::net::Ipv6Addr>().is_ok() || future) && port(suffix)
        } else if let Some((host, suffix)) = host_port.rsplit_once(':') {
            component(host, b"") && suffix.bytes().all(|byte| byte.is_ascii_digit())
        } else {
            component(host_port, b"")
        }
    }
    let Some(text) = value
        .as_str()
        .filter(|text| !text.is_empty() && text.len() <= 4096 && text.is_ascii())
    else {
        return false;
    };
    let Some((scheme, tail)) = text.split_once(':') else {
        return false;
    };
    if scheme.is_empty()
        || !scheme.as_bytes()[0].is_ascii_alphabetic()
        || !scheme
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"+-.".contains(&byte))
        || !text.as_bytes().iter().enumerate().all(|(index, byte)| {
            *byte != b'%'
                || text
                    .as_bytes()
                    .get(index + 1..index + 3)
                    .is_some_and(|hex| hex.iter().all(u8::is_ascii_hexdigit))
        })
    {
        return false;
    }
    let (before_fragment, fragment) = tail.split_once('#').unwrap_or((tail, ""));
    let (hierarchy, query) = before_fragment
        .split_once('?')
        .unwrap_or((before_fragment, ""));
    if !component(query, b":@/?") || !component(fragment, b":@/?") {
        return false;
    }
    if let Some(rest) = hierarchy.strip_prefix("//") {
        let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
        authority(host) && component(path, b":@/")
    } else {
        component(hierarchy, b":@/")
    }
}

fn evidence(value: &Value) -> bool {
    match value["kind"].as_str() {
        Some("git_blob") => {
            fields(
                value,
                &["kind", "commit", "blob", "path", "line_start", "line_end"],
            ) && git_object(&value["commit"])
                && git_object(&value["blob"])
                && value["path"]
                    .as_str()
                    .is_some_and(crate::delegation_identity::representable_change_path)
                && match (value["line_start"].as_u64(), value["line_end"].as_u64()) {
                    (Some(start), Some(end)) => start > 0 && start <= end && end <= MAX_INTEGER,
                    _ => value["line_start"].is_null() && value["line_end"].is_null(),
                }
        }
        Some("git_object") => fields(value, &["kind", "object"]) && git_object(&value["object"]),
        Some("accepted_context") => {
            fields(value, &["kind", "identity", "content"])
                && text(&value["identity"], 512)
                && value["identity"].as_str().is_some_and(|identity| {
                    !identity.chars().any(|ch| ch <= '\u{1f}' || ch == '\u{7f}')
                })
                && bytes_ref(&value["content"])
        }
        Some("url") => {
            fields(value, &["kind", "url", "content"])
                && uri(&value["url"])
                && bytes_ref(&value["content"])
        }
        _ => false,
    }
}

pub(crate) fn validate(value: &Value) -> Result<(), MachineError> {
    let valid = fields(
        value,
        &["schema", "findings", "covered_questions", "limitations"],
    ) && value["schema"] == SCHEMA
        && strings(&value["covered_questions"], 128, 0, true)
        && strings(&value["limitations"], 4096, 0, false)
        && value["findings"].as_array().is_some_and(|findings| {
            findings.len() <= 64
                && findings.iter().all(|finding| {
                    fields(
                        finding,
                        &[
                            "finding_key",
                            "claim",
                            "kind",
                            "stance",
                            "confidence",
                            "evidence",
                            "covered_questions",
                            "limitations",
                        ],
                    ) && text(&finding["finding_key"], 256)
                        && text(&finding["claim"], 8192)
                        && choice(
                            &finding["kind"],
                            &["defect", "requirement", "risk", "observation"],
                        )
                        && choice(&finding["stance"], &["supports", "opposes", "neutral"])
                        && choice(&finding["confidence"], &["high", "medium", "low"])
                        && strings(&finding["covered_questions"], 128, 1, true)
                        && strings(&finding["limitations"], 4096, 0, false)
                        && finding["evidence"].as_array().is_some_and(|refs| {
                            (1..=64).contains(&refs.len()) && refs.iter().all(evidence)
                        })
                })
        });
    if valid { Ok(()) } else { Err(invalid()) }
}

/// Parse original bytes before serde can discard duplicate object keys.
pub(crate) fn parse(bytes: &[u8]) -> Result<Vec<u8>, MachineError> {
    if bytes.len() > MAX_BYTES {
        return Err(invalid());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
    let lossless = crate::jcs::parse(text).map_err(|_| invalid())?;
    let canonical = crate::jcs::canonicalize(&lossless).map_err(|_| invalid())?;
    let value: Value = serde_json::from_slice(&canonical).map_err(|_| invalid())?;
    validate(&value)?;
    Ok(canonical)
}

pub(crate) fn output_contract() -> &'static Value {
    static CONTRACT: OnceLock<Value> = OnceLock::new();
    CONTRACT.get_or_init(|| {
        let identities: Value = serde_json::from_str(include_str!(
            "../docs/protocol/dolgorae-v014-identities-v1.schema.json"
        ))
        .expect("checked identities");
        let paths: Value = serde_json::from_str(include_str!(
            "../docs/protocol/dolgorae-review-target-v2.schema.json"
        ))
        .expect("checked paths");
        fn expand(value: &Value, identities: &Value, paths: &Value) -> Value {
            if let Some(reference) = value["$ref"].as_str() {
                let (uri, pointer) = reference.split_once('#').expect("checked reference");
                let source = if uri == "https://dolgorae.local/schema/v014-identities/v1" {
                    identities
                } else {
                    paths
                };
                return expand(
                    source.pointer(pointer).expect("checked output reference"),
                    identities,
                    paths,
                );
            }
            match value {
                Value::Object(fields) => Value::Object(
                    fields
                        .iter()
                        .map(|(key, value)| (key.clone(), expand(value, identities, paths)))
                        .collect(),
                ),
                Value::Array(values) => Value::Array(
                    values
                        .iter()
                        .map(|value| expand(value, identities, paths))
                        .collect(),
                ),
                _ => value.clone(),
            }
        }
        expand(
            &identities["$defs"]["structured_findings"],
            &identities,
            &paths,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Value {
        serde_json::from_str(include_str!(
            "../docs/protocol/examples/v014-identity-findings.valid.json"
        ))
        .unwrap()
    }
    #[test]
    fn findings_preserve_source_bytes_and_reject_invalid_shapes() {
        let good = fixture();
        assert!(parse(&serde_json::to_vec(&good).unwrap()).is_ok());
        for mutate in [0, 1, 2, 3, 4, 5] {
            let mut bad = good.clone();
            match mutate {
                0 => bad["extra"] = json!(true),
                1 => bad["findings"][0]["evidence"] = json!([]),
                2 => bad["findings"][0]["confidence"] = json!("certain"),
                3 => bad["covered_questions"] = json!(["C1", "C1"]),
                4 => bad["findings"][0]["claim"] = json!("é".repeat(4097)),
                _ => bad["findings"] = json!(vec![good["findings"][0].clone(); 65]),
            }
            assert_eq!(validate(&bad).unwrap_err().code, "FINDINGS_OUTPUT_INVALID");
        }
        assert!(parse(br#"{"schema":"wrong","schema":"structured_findings_v1","findings":[],"covered_questions":[],"limitations":[]}"#).is_err());
        assert!(parse(b"not JSON").is_err());
    }
    #[test]
    fn evidence_requires_pinned_closed_references() {
        let mut good = fixture();
        for reference in [
            json!({"kind":"git_blob","commit":"a".repeat(40),"blob":"b".repeat(64),"path":"src/lib.rs","line_start":1,"line_end":2}),
            json!({"kind":"git_object","object":"a".repeat(40)}),
            json!({"kind":"url","url":"https://example.com/a%20b","content":{"byte_length":0,"sha256":format!("sha256:{}", "a".repeat(64))}}),
        ] {
            good["findings"][0]["evidence"] = json!([reference]);
            validate(&good).unwrap();
        }
        for reference in [
            json!({"kind":"git_blob","commit":"a".repeat(40),"blob":"b".repeat(40),"path":"../private","line_start":1,"line_end":2}),
            json!({"kind":"git_blob","commit":"a".repeat(40),"blob":"b".repeat(40),"path":"src/lib.rs","line_start":2,"line_end":1}),
            json!({"kind":"git_object","object":"moving-branch"}),
            json!({"kind":"url","url":"relative/path","content":{"byte_length":0,"sha256":format!("sha256:{}", "a".repeat(64))}}),
        ] {
            good["findings"][0]["evidence"] = json!([reference]);
            assert!(validate(&good).is_err());
        }
        assert!(
            output_contract()["properties"]["findings"]["items"]["properties"]["evidence"]["items"]
                ["oneOf"]
                .is_array()
        );
    }

    #[test]
    fn evidence_uri_matches_absolute_uri_grammar() {
        for value in [
            "https://example.com/a%20b?q=a/b?c#part",
            "urn:example:item",
            "file:///tmp/source",
            "https://[::1]:443/path",
            "https://[v1.example]/path",
        ] {
            assert!(uri(&json!(value)), "{value}");
        }
        for value in [
            "relative/path",
            "https://[broken]/",
            "https://host:abc/",
            "https://host/a[b]",
            "https://host/?q=[bad]",
            "https://host/#one#two",
            "https://host/%zz",
            "https://one@two@host/",
        ] {
            assert!(!uri(&json!(value)), "{value}");
        }
    }
}
