//! Safe public Resolve identity at the existing approval-decision boundary.
use crate::operation::{Envelope, Namespace, Subjects};
use crate::turn::TurnError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

pub(crate) const INTENT_SCHEMA: &str = "dolgorae.public-resolution-intent/v2";
const RECEIPT_SCHEMA: &str = "dolgorae.public-resolution-receipt/v2";
const BODY_SCHEMA: &str =
    "https://dolgorae.local/schema/machine-output/v5#/$defs/resolve_interaction_normalized_body";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Answer {
    pub question_id: String,
    pub selected_labels: Vec<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub text: Option<String>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Body {
    pub run_ref: String,
    pub interaction_ref: String,
    pub turn_ref: String,
    pub thread_generation: u64,
    pub decision: String,
    pub granted_permissions: Vec<String>,
    pub protected_input_present: bool,
    pub answers: Vec<Answer>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Intent {
    pub schema: String,
    pub body: Body,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub schema: String,
    pub mutation_envelope: Envelope,
    pub accepted_input_digest: String,
    pub operation_ref: Uuid,
    pub accepted_subjects: Subjects,
    pub contains_protected_input: bool,
}
impl Intent {
    pub(crate) fn validate(&self) -> Result<(), TurnError> {
        let b = &self.body;
        if self.schema != INTENT_SCHEMA
            || !["accept_once", "decline", "cancel"].contains(&b.decision.as_str())
            || !crate::operation::valid_opaque_ref(&b.run_ref)
            || Uuid::parse_str(&b.interaction_ref).is_err()
            || !crate::operation::valid_opaque_ref(&b.turn_ref)
            || b.thread_generation == 0
            || b.thread_generation > crate::domain::MAX_JCS_SAFE_INTEGER
            || b.answers.len() > 64
            || b.granted_permissions.len() > 64
            || (b.protected_input_present
                && (!b.answers.is_empty() || !b.granted_permissions.is_empty()))
            || b.answers.iter().any(|a| {
                a.question_id.is_empty()
                    || a.question_id.len() > 128
                    || a.selected_labels.len() > 64
                    || a.selected_labels
                        .iter()
                        .any(|s| s.is_empty() || s.len() > 256)
                    || a.text.as_ref().is_some_and(|s| s.len() > 65536)
            })
            || b.granted_permissions
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != b.granted_permissions.len()
            || b.granted_permissions
                .iter()
                .any(|s| s.is_empty() || s.len() > 256)
        {
            return Err(TurnError::InvalidInput(
                "public resolution intent is invalid",
            ));
        }
        Ok(())
    }
    /// Called before private admission hashing, using journal-owned protection.
    pub(crate) fn redact(&mut self, protected: bool) {
        if protected {
            for a in &mut self.body.answers {
                use zeroize::Zeroize;
                a.question_id.zeroize();
                a.selected_labels.zeroize();
                a.text.zeroize();
            }
            self.body.answers.clear();
            use zeroize::Zeroize;
            self.body.granted_permissions.zeroize();
            self.body.granted_permissions.clear();
        }
    }
    pub(crate) fn receipt(
        &self,
        namespace: Namespace,
        operation_ref: Uuid,
        protected: bool,
    ) -> Result<Receipt, TurnError> {
        self.validate()?;
        let bytes = crate::operation::canonical(
            &serde_json::to_value(&self.body)
                .map_err(|_| TurnError::InvalidInput("resolution body"))?,
        )?;
        let envelope = Envelope {
            schema: crate::operation::ENVELOPE_SCHEMA.into(),
            request_schema: BODY_SCHEMA.into(),
            namespace,
            trusted_invocation_ref: None,
            request_body: crate::operation::BytesRef {
                byte_length: bytes.len() as u64,
                sha256: format!("sha256:{}", crate::jcs::sha256_hex(&bytes)),
            },
        };
        Ok(Receipt {
            schema: RECEIPT_SCHEMA.into(),
            accepted_input_digest: crate::operation::envelope_digest(&envelope)?,
            mutation_envelope: envelope,
            operation_ref,
            contains_protected_input: protected,
            accepted_subjects: Subjects {
                run_ref: Some(self.body.run_ref.clone()),
                turn_ref: Some(self.body.turn_ref.clone()),
                interaction_ref: Some(self.body.interaction_ref.clone()),
                ..Default::default()
            },
        })
    }
}
impl Receipt {
    pub(crate) fn validate(&self) -> Result<(), TurnError> {
        let e = &self.mutation_envelope;
        let s = &self.accepted_subjects;
        if self.schema != RECEIPT_SCHEMA
            || self.operation_ref.get_version_num() != 7
            || e.schema != crate::operation::ENVELOPE_SCHEMA
            || e.request_schema != BODY_SCHEMA
            || e.trusted_invocation_ref.is_some()
            || e.namespace.operation_kind != "resolve_interaction"
            || e.namespace.scope_kind.as_deref() != Some("run")
            || e.namespace.scope_ref != s.run_ref
            || e.namespace.controller_generation == 0
            || e.namespace.controller_generation > crate::domain::MAX_JCS_SAFE_INTEGER
            || e.namespace.controller_principal.len() != 71
            || !e.namespace.controller_principal.starts_with("sha256:")
            || !e.namespace.controller_principal[7..]
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            || e.namespace.workspace_ref.len() != 64
            || !e
                .namespace
                .workspace_ref
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            || e.namespace.idempotency_key.is_empty()
            || e.namespace.idempotency_key.len() > 256
            || e.request_body.byte_length == 0
            || e.request_body.byte_length > crate::domain::MAX_JCS_SAFE_INTEGER
            || e.request_body.sha256.len() != 71
            || !e.request_body.sha256.starts_with("sha256:")
            || !e.request_body.sha256[7..]
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            || *s
                != (Subjects {
                    run_ref: s.run_ref.clone(),
                    turn_ref: s.turn_ref.clone(),
                    interaction_ref: s.interaction_ref.clone(),
                    ..Default::default()
                })
            || s.run_ref.is_none()
            || s.turn_ref
                .as_deref()
                .is_none_or(|s| !crate::operation::valid_opaque_ref(s))
            || s.interaction_ref
                .as_deref()
                .and_then(|s| Uuid::parse_str(s).ok())
                .is_none_or(|id| id.get_version_num() != 7)
            || self.accepted_input_digest != crate::operation::envelope_digest(e)?
        {
            return Err(TurnError::Journal(
                "public resolution receipt is invalid".into(),
            ));
        }
        Ok(())
    }
}

pub(crate) fn lookup(
    records: &[crate::audit::AuditRecord],
    namespace: &Namespace,
) -> Result<Value, TurnError> {
    let mut accepted: Option<(Receipt, u64, u64)> = None;
    for record in records {
        if record.kind() != crate::audit::AuditKind::ApprovalDecided {
            continue;
        }
        let bytes = crate::jcs::canonicalize(record.payload())
            .map_err(|_| TurnError::Journal("resolution journal".into()))?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| TurnError::Journal("resolution journal".into()))?;
        if let Some(raw) = value.get("public_resolution") {
            let receipt: Receipt = serde_json::from_value(raw.clone())
                .map_err(|_| TurnError::Journal("resolution receipt".into()))?;
            receipt.validate()?;
            if receipt.accepted_subjects.run_ref.as_deref()
                != Some(record.run_id().to_string().as_str())
                || value["idempotency_key"] != receipt.mutation_envelope.namespace.idempotency_key
                || receipt.contains_protected_input
                    != (value["resolution"]["contained_secret"] == true)
            {
                return Err(TurnError::Journal(
                    "public resolution decision linkage".into(),
                ));
            }
            if &receipt.mutation_envelope.namespace == namespace {
                if accepted.is_some() {
                    return Err(TurnError::Journal("duplicate public resolution key".into()));
                }
                if value["resolution_receipt_id"] != receipt.operation_ref.to_string() {
                    return Err(TurnError::Journal("resolution receipt linkage".into()));
                }
                accepted = Some((receipt, record.sequence(), record.run_generation()));
            }
        }
    }
    let Some((receipt, sequence, generation)) = accepted else {
        return Ok(
            json!({"state":"unresolved","known_subjects":Subjects {run_ref:namespace.scope_ref.clone(),..Default::default()}}),
        );
    };
    let completed = records
        .iter()
        .filter(|r| {
            r.sequence() > sequence
                && r.run_generation() == generation
                && r.kind() == crate::audit::AuditKind::TurnTerminal
        })
        .any(|r| {
            crate::jcs::canonicalize(r.payload())
                .ok()
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                .is_some_and(|v| {
                    v["turn_id"].as_str() == receipt.accepted_subjects.turn_ref.as_deref()
                })
        });
    Ok(
        json!({"state":"accepted","receipt":{"operation_ref":receipt.operation_ref,"accepted_input_digest":receipt.accepted_input_digest},"accepted_subjects":receipt.accepted_subjects,"current_outcome":if completed {"completed"} else {"unknown"}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_resolution_ordinary_bounds_match_frozen_normalized_body() {
        let mut intent = Intent {
            schema: INTENT_SCHEMA.into(),
            body: Body {
                run_ref: Uuid::now_v7().to_string(),
                interaction_ref: Uuid::now_v7().to_string(),
                turn_ref: "turn".into(),
                thread_generation: 1,
                decision: "accept_once".into(),
                granted_permissions: vec![],
                protected_input_present: false,
                answers: vec![Answer {
                    question_id: "q".repeat(128),
                    selected_labels: vec!["é".repeat(128)],
                    text: Some("é".repeat(32768)),
                }],
            },
        };
        intent.validate().unwrap();
        intent.body.answers[0].question_id.push('q');
        assert!(intent.validate().is_err());
        intent.body.answers[0].question_id.pop();
        intent.body.answers[0].selected_labels[0].push('x');
        assert!(intent.validate().is_err());
        intent.body.answers[0].selected_labels[0].pop();
        intent.body.answers[0].text.as_mut().unwrap().push('x');
        assert!(intent.validate().is_err());
        intent.body.answers[0].text.as_mut().unwrap().pop();
        intent.body.granted_permissions = vec!["one".into(), "one".into()];
        assert!(intent.validate().is_err());
    }
}
