//! Pinned-host disposition for the optional external Specialist Review MCP tool.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const EXTERNAL_REQUEST_REF_KEY: &str = "xyz.rootkernel.dolgorae/externalRequestRef";

pub const REQUIRED_PROBE_SCENARIOS: [&str; 9] = [
    "initial_call",
    "same_reference_retry",
    "client_reconnect",
    "server_restart",
    "concurrent_calls",
    "response_loss_before_result_commit",
    "response_loss_after_result_commit",
    "connection_identity_ignored",
    "process_identity_ignored",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpReviewDisposition {
    ReplaySafeMeta,
    McpUnavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CarrierObservation<'a> {
    pub scenario: &'a str,
    pub external_request_ref: Option<&'a str>,
}

/// Select replay-safe mode only when every required path preserves the exact
/// same trusted reference. Missing, malformed, duplicate, or drifting evidence
/// selects the mandatory Machine CLI fallback without best-effort inference.
#[must_use]
pub fn select_disposition(observations: &[CarrierObservation<'_>]) -> McpReviewDisposition {
    let names = observations
        .iter()
        .map(|observation| observation.scenario)
        .collect::<BTreeSet<_>>();
    if names != REQUIRED_PROBE_SCENARIOS.into_iter().collect()
        || observations.len() != REQUIRED_PROBE_SCENARIOS.len()
    {
        return McpReviewDisposition::McpUnavailable;
    }
    let Some(reference) = observations
        .first()
        .and_then(|observation| observation.external_request_ref)
    else {
        return McpReviewDisposition::McpUnavailable;
    };
    if !uuid7(reference)
        || observations
            .iter()
            .any(|observation| observation.external_request_ref != Some(reference))
    {
        return McpReviewDisposition::McpUnavailable;
    }
    McpReviewDisposition::ReplaySafeMeta
}

#[must_use]
pub const fn advertise_tool(disposition: McpReviewDisposition) -> bool {
    matches!(disposition, McpReviewDisposition::ReplaySafeMeta)
}

fn uuid7(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && [8, 13, 18, 23]
            .into_iter()
            .all(|index| bytes[index] == b'-')
        && bytes[14] == b'7'
        && matches!(bytes[19], b'8' | b'9' | b'a' | b'b')
        && bytes.iter().enumerate().all(|(index, byte)| {
            [8, 13, 18, 23].contains(&index)
                || byte.is_ascii_digit()
                || (b'a'..=b'f').contains(byte)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const REFERENCE: &str = "018f0c6a-7b01-7abc-8def-0123456789ab";

    fn complete<'a>(reference: Option<&'a str>) -> Vec<CarrierObservation<'a>> {
        REQUIRED_PROBE_SCENARIOS
            .iter()
            .map(|scenario| CarrierObservation {
                scenario,
                external_request_ref: reference,
            })
            .collect()
    }

    #[test]
    fn replay_safe_requires_one_exact_uuid7_on_every_path() {
        assert_eq!(
            select_disposition(&complete(Some(REFERENCE))),
            McpReviewDisposition::ReplaySafeMeta
        );
        assert!(advertise_tool(McpReviewDisposition::ReplaySafeMeta));

        for index in 0..REQUIRED_PROBE_SCENARIOS.len() {
            let mut observations = complete(Some(REFERENCE));
            observations[index].external_request_ref = None;
            assert_eq!(
                select_disposition(&observations),
                McpReviewDisposition::McpUnavailable,
                "missing carrier in {}",
                REQUIRED_PROBE_SCENARIOS[index]
            );
        }
        let mut drift = complete(Some(REFERENCE));
        drift[3].external_request_ref = Some("018f0c6a-7b01-7abc-8def-0123456789ac");
        assert_eq!(
            select_disposition(&drift),
            McpReviewDisposition::McpUnavailable
        );
    }

    #[test]
    fn pinned_artifact_selects_machine_cli_fallback() {
        let artifact: Value = serde_json::from_str(include_str!(
            "../docs/protocol/dolgorae-specialist-review-mcp-disposition-v1.json"
        ))
        .unwrap();
        assert_eq!(artifact["disposition"], "mcp_unavailable");
        assert_eq!(artifact["tool_advertised"], false);
        assert_eq!(artifact["supported_carrier"], "machine_cli");
        assert_eq!(
            artifact.pointer("/observed_call/required_key_observed"),
            Some(&Value::Bool(false))
        );
        let scenarios = artifact["scenario_matrix"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["scenario"].as_str().unwrap())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            scenarios,
            REQUIRED_PROBE_SCENARIOS.into_iter().collect(),
            "the checked artifact must cover every required scenario exactly once"
        );
        assert_eq!(artifact["scenario_matrix"].as_array().unwrap().len(), 9);
        assert!(!advertise_tool(McpReviewDisposition::McpUnavailable));
    }

    #[test]
    fn uuid7_contract_is_lowercase_and_variant_checked() {
        assert!(uuid7(REFERENCE));
        assert!(!uuid7("018F0C6A-7B01-7ABC-8DEF-0123456789AB"));
        assert!(!uuid7("018f0c6a-7b01-7abc-cdef-0123456789ab"));
    }
}
