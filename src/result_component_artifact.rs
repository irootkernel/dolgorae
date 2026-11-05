//! Immutable publication bindings for complete-result component observations.
use crate::artifact::{ArtifactKind, ArtifactReference, ArtifactVisibility};
use crate::controller::CredentialCarrier;
use crate::delegation_identity::{BytesRef, CompleteResult};
use crate::engagement::EngagementStore;
use crate::machine::MachineError;
use crate::workspace::WorkspaceView;
use base64::Engine as _;
use rusqlite::{Connection, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;
use uuid::Uuid;

const MAX_ARTIFACT: u64 = 32 * 1024 * 1024;
const INLINE: u64 = 1024 * 1024;
const PREFIX: &str = "result-component-bundle:v1:";
const IDENTITY_PREFIX: &str = "result-component-identity:v1:";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityPublication {
    schema: String,
    result_id: Uuid,
    workspace_id: String,
    workspace_path: String,
    run_id: Uuid,
    result_digest: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Component {
    selector: String,
    reference: BytesRef,
    media_type: String,
    artifact_id: Uuid,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Publication {
    schema: String,
    result_id: Uuid,
    workspace_id: String,
    workspace_path: String,
    run_id: Uuid,
    result: CompleteResult,
    components: Vec<Component>,
    manifest_id: Uuid,
    identity_ids: Vec<Uuid>,
    page_ids: Vec<Uuid>,
    #[serde(skip)]
    prepared_logical_bytes: Option<u64>,
}
fn integrity() -> MachineError {
    MachineError::new(
        "ARTIFACT_INTEGRITY_FAILURE",
        "result component source does not match its publication",
        false,
        json!({}),
    )
}
fn unavailable() -> MachineError {
    MachineError::new(
        "COMPLETE_RESULT_UNAVAILABLE",
        "the complete result source is unavailable",
        false,
        json!({}),
    )
}
fn key(task: &str) -> String {
    format!("{PREFIX}{task}")
}
fn encode(value: &impl Serialize) -> Result<Vec<u8>, MachineError> {
    crate::operation::canonical(&serde_json::to_value(value).map_err(|_| integrity())?)
        .map_err(|_| integrity())
}
fn component_ref(publication: &Publication, component: &Component) -> Value {
    json!({"content": component.reference, "media_type":component.media_type,
        "artifact":{"artifact_id":component.artifact_id,"kind":"ARTIFACT_KIND_FINAL_RESPONSE","visibility":"ARTIFACT_VISIBILITY_CONTROLLER_ONLY"},
        "artifact_owner":{"workspace":{"absolute_path":publication.workspace_path,"expected_workspace_id":publication.workspace_id},"run_id":publication.run_id}})
}
const COMPONENTS_PER_PAGE: usize = 256;
fn generated_ref(p: &Publication, id: Uuid, bytes: &[u8]) -> Value {
    generated_ref_media(p, id, bytes, "application/json")
}
fn generated_ref_media(p: &Publication, id: Uuid, bytes: &[u8], media_type: &str) -> Value {
    component_ref(
        p,
        &Component {
            selector: String::new(),
            reference: BytesRef::from_bytes(bytes),
            media_type: media_type.into(),
            artifact_id: id,
        },
    )
}
fn manifest_page(p: &Publication, index: usize) -> Result<Vec<u8>, MachineError> {
    let components = p
        .components
        .chunks(COMPONENTS_PER_PAGE)
        .nth(index)
        .ok_or_else(integrity)?;
    encode(
        &json!({"schema":"dolgorae.result-component-manifest-page/v1","result_id":p.result_id,"page_index":index,
        "components":components.iter().map(|c| json!({"selector":c.selector,"content_ref":component_ref(p,c)})).collect::<Vec<_>>() }),
    )
}
fn manifest(p: &Publication) -> Result<Vec<u8>, MachineError> {
    manifest_with_logical_bytes(p).map(|(bytes, _)| bytes)
}
fn manifest_with_logical_bytes(p: &Publication) -> Result<(Vec<u8>, u64), MachineError> {
    let identity = encode(&p.result)?;
    let mut page_bytes = 0u64;
    let pages = p
        .page_ids
        .iter()
        .enumerate()
        .map(|(index, id)| {
            let bytes = manifest_page(p, index)?;
            page_bytes = page_bytes
                .checked_add(bytes.len() as u64)
                .ok_or_else(integrity)?;
            Ok(generated_ref(p, *id, &bytes))
        })
        .collect::<Result<Vec<_>, MachineError>>()?;
    let bytes = encode(
        &json!({"schema":"dolgorae.result-component-manifest/v1","result_id":p.result_id,"result_digest":p.result.digest()?,
        "identity":{"content":BytesRef::from_bytes(&identity),"chunks":p.identity_ids.iter().enumerate().map(|(index,id)| Ok(generated_ref_media(p,*id,identity_chunk(&identity,index)?,"application/octet-stream"))).collect::<Result<Vec<_>,MachineError>>()?},"component_pages":pages}),
    )?;
    let generated = (bytes.len() as u64)
        .checked_add(identity.len() as u64)
        .and_then(|total| total.checked_add(page_bytes))
        .ok_or_else(integrity)?;
    let total = p.components.iter().try_fold(generated, |total, c| {
        total
            .checked_add(c.reference.byte_length)
            .ok_or_else(integrity)
    })?;
    Ok((bytes, total))
}
fn identity_chunk(bytes: &[u8], index: usize) -> Result<&[u8], MachineError> {
    let start = index
        .checked_mul(MAX_ARTIFACT as usize)
        .ok_or_else(integrity)?;
    if start >= bytes.len() {
        return Err(integrity());
    }
    bytes
        .get(start..start.saturating_add(MAX_ARTIFACT as usize).min(bytes.len()))
        .ok_or_else(integrity)
}
fn generated(p: &Publication, id: Uuid) -> Result<Option<Vec<u8>>, MachineError> {
    if id == p.manifest_id {
        return manifest(p).map(Some);
    }
    if let Some(index) = p.identity_ids.iter().position(|chunk| *chunk == id) {
        return identity_chunk(&encode(&p.result)?, index).map(|bytes| Some(bytes.to_vec()));
    }
    p.page_ids
        .iter()
        .position(|page| *page == id)
        .map(|index| manifest_page(p, index))
        .transpose()
}
impl Publication {
    pub(crate) fn prepare(
        view: &WorkspaceView,
        run_id: Uuid,
        result: &CompleteResult,
    ) -> Result<Self, MachineError> {
        result.validate()?;
        if result.source.worker_ref != run_id.to_string() || !result.attachments.is_empty() {
            return Err(integrity());
        }
        let mut components = Vec::new();
        let mut add = |selector: String,
                       reference: &BytesRef,
                       media_type: &str|
         -> Result<(), MachineError> {
            if reference.byte_length > MAX_ARTIFACT {
                return Err(unavailable());
            }
            components.push(Component {
                selector,
                reference: reference.clone(),
                media_type: media_type.to_owned(),
                artifact_id: Uuid::now_v7(),
            });
            Ok(())
        };
        if let Some(r) = &result.final_response {
            add("final_response".into(), r, "text/plain; charset=utf-8")?;
        }
        if let Some(r) = &result.structured_output {
            add("structured_output".into(), r, "application/json")?;
        }
        for (i, c) in result.changes.iter().enumerate() {
            for (side, r) in [("before", &c.before), ("after", &c.after)] {
                if let Some(r) = r {
                    add(format!("change:{i}:{side}"), r, "application/octet-stream")?;
                }
            }
        }
        let mut publication = Self {
            schema: "dolgorae.result-component-binding/v1".into(),
            result_id: Uuid::now_v7(),
            workspace_id: view.workspace_id.clone(),
            workspace_path: view
                .canonical_path
                .to_path_buf()?
                .to_str()
                .ok_or_else(integrity)?
                .to_owned(),
            run_id,
            result: result.clone(),
            page_ids: (0..components.len().div_ceil(COMPONENTS_PER_PAGE))
                .map(|_| Uuid::now_v7())
                .collect(),
            components,
            manifest_id: Uuid::now_v7(),
            identity_ids: (0..encode(result)?.len().div_ceil(MAX_ARTIFACT as usize))
                .map(|_| Uuid::now_v7())
                .collect(),
            prepared_logical_bytes: None,
        };
        let (manifest, logical_bytes) = manifest_with_logical_bytes(&publication)?;
        if manifest.len() as u64 > MAX_ARTIFACT {
            return Err(unavailable());
        }
        publication.prepared_logical_bytes = Some(logical_bytes);
        Ok(publication)
    }
    fn validate(&self, task: &str) -> Result<(), MachineError> {
        if self.schema != "dolgorae.result-component-binding/v1"
            || self.result.source.task_ref != task
            || self.result.source.worker_ref != self.run_id.to_string()
            || self.run_id.get_version_num() != 7
            || !self.result.attachments.is_empty()
            || self.page_ids.len() != self.components.len().div_ceil(COMPONENTS_PER_PAGE)
            || self.identity_ids.len()
                != encode(&self.result)?.len().div_ceil(MAX_ARTIFACT as usize)
        {
            return Err(integrity());
        }
        self.result.validate()?;
        let mut expected = Vec::new();
        if let Some(reference) = &self.result.final_response {
            expected.push((
                "final_response".to_owned(),
                reference,
                "text/plain; charset=utf-8",
            ));
        }
        if let Some(reference) = &self.result.structured_output {
            expected.push((
                "structured_output".to_owned(),
                reference,
                "application/json",
            ));
        }
        for (index, change) in self.result.changes.iter().enumerate() {
            for (side, reference) in [("before", &change.before), ("after", &change.after)] {
                if let Some(reference) = reference {
                    expected.push((
                        format!("change:{index}:{side}"),
                        reference,
                        "application/octet-stream",
                    ));
                }
            }
        }
        if expected.len() != self.components.len()
            || expected.iter().zip(&self.components).any(
                |((selector, reference, media), component)| {
                    selector != &component.selector
                        || *reference != &component.reference
                        || *media != component.media_type
                        || component.reference.byte_length > MAX_ARTIFACT
                },
            )
        {
            return Err(integrity());
        }
        let mut ids = std::collections::BTreeSet::new();
        for id in self
            .components
            .iter()
            .map(|c| c.artifact_id)
            .chain([self.run_id, self.result_id, self.manifest_id])
            .chain(self.identity_ids.iter().copied())
            .chain(self.page_ids.iter().copied())
        {
            if id.get_version_num() != 7 || !ids.insert(id) {
                return Err(integrity());
            }
        }
        Ok(())
    }
    /// Publication identity survives content retirement; this does not read bodies.
    pub(crate) fn retained_lookup_identity(
        connection: &Connection,
        view: &WorkspaceView,
        run: Uuid,
        task: Uuid,
        body_free: &CompleteResult,
    ) -> Result<Option<CompleteResult>, MachineError> {
        if let Some(publication) = load(connection, &task.to_string())? {
            if publication.run_id != run
                || publication.workspace_id != view.workspace_id
                || publication.workspace_path
                    != view
                        .canonical_path
                        .to_path_buf()?
                        .to_str()
                        .ok_or_else(integrity)?
            {
                return Err(integrity());
            }
            return Ok(Some(publication.result));
        }
        let Some(raw) = crate::engagement::result_component_record(
            connection,
            &format!("{IDENTITY_PREFIX}{task}"),
        )?
        else {
            return Ok(None);
        };
        let record: IdentityPublication = serde_json::from_str(&raw).map_err(|_| integrity())?;
        if record.schema != "dolgorae.result-component-identity/v1"
            || record.result_id.get_version_num() != 7
            || record.workspace_id != view.workspace_id
            || record.workspace_path
                != view
                    .canonical_path
                    .to_path_buf()?
                    .to_str()
                    .ok_or_else(integrity)?
            || record.run_id != run
            || body_free.terminal_outcome != "expired"
            || body_free.source.task_ref != task.to_string()
            || body_free.source.worker_ref != run.to_string()
            || body_free.digest()? != record.result_digest
        {
            return Err(integrity());
        }
        Ok(Some(body_free.clone()))
    }

    pub(crate) fn logical_bytes(&self) -> Result<u64, MachineError> {
        self.prepared_logical_bytes.map_or_else(
            || manifest_with_logical_bytes(self).map(|(_, total)| total),
            Ok,
        )
    }
    pub(crate) fn retain(&self, tx: &Transaction<'_>) -> Result<(), MachineError> {
        let body = String::from_utf8(encode(self)?).map_err(|_| integrity())?;
        crate::engagement::retain_result_component_record(
            tx,
            &key(&self.result.source.task_ref),
            &body,
        )?;
        crate::engagement::retain_result_component_record(
            tx,
            &format!(
                "result-component-run:v1:{}:{}",
                self.run_id, self.result.source.task_ref
            ),
            &self.result.source.task_ref,
        )?;
        for id in self
            .components
            .iter()
            .map(|c| c.artifact_id)
            .chain([self.manifest_id])
            .chain(self.identity_ids.iter().copied())
            .chain(self.page_ids.iter().copied())
        {
            crate::engagement::retain_result_component_record(
                tx,
                &format!("result-component-id:v1:{id}"),
                &self.result.source.task_ref,
            )?;
        }
        Ok(())
    }
    /// A body-free terminal identity remains available when artifact quota is
    /// exhausted. This publishes no ArtifactRef or uncharged artifact bytes.
    pub(crate) fn retain_identity_only(&self, tx: &Transaction<'_>) -> Result<(), MachineError> {
        if !self.components.is_empty()
            || !self.result.changes.is_empty()
            || !self.result.attachments.is_empty()
        {
            return Err(integrity());
        }
        let record = IdentityPublication {
            schema: "dolgorae.result-component-identity/v1".into(),
            result_id: self.result_id,
            workspace_id: self.workspace_id.clone(),
            workspace_path: self.workspace_path.clone(),
            run_id: self.run_id,
            result_digest: self.result.digest()?,
        };
        let value = String::from_utf8(encode(&record)?).map_err(|_| integrity())?;
        crate::engagement::retain_result_component_record(
            tx,
            &format!("{IDENTITY_PREFIX}{}", self.result.source.task_ref),
            &value,
        )
    }
}
fn load(connection: &Connection, task: &str) -> Result<Option<Publication>, MachineError> {
    let Some(raw) = crate::engagement::result_component_record(connection, &key(task))? else {
        return Ok(None);
    };
    let publication: Publication = serde_json::from_str(&raw).map_err(|_| integrity())?;
    publication.validate(task)?;
    Ok(Some(publication))
}
pub(crate) fn logical_bytes(state_root: &Path, run: Uuid) -> Result<u64, MachineError> {
    let store =
        EngagementStore::open_read_only(&EngagementStore::workspace_database_path(state_root))?;
    crate::engagement::result_component_records(
        store.connection(),
        &format!("result-component-run:v1:{run}:"),
    )?
    .into_iter()
    .try_fold(0u64, |total, raw| {
        let publication = load(store.connection(), &raw)?.ok_or_else(integrity)?;
        if publication.run_id != run {
            return Err(integrity());
        }
        total
            .checked_add(publication.logical_bytes()?)
            .ok_or_else(integrity)
    })
}
fn verify_source(state_root: &Path, p: &Publication) -> Result<(), MachineError> {
    verify_complete_source(state_root, p.run_id, &p.result)
}
fn verify_complete_source(
    state_root: &Path,
    run_id: Uuid,
    result: &CompleteResult,
) -> Result<(), MachineError> {
    let store =
        EngagementStore::open_read_only(&EngagementStore::workspace_database_path(state_root))?;
    let scope = Uuid::parse_str(&result.source.scope_ref).map_err(|_| integrity())?;
    let task = Uuid::parse_str(&result.source.task_ref).map_err(|_| integrity())?;
    let (owner, state) = match result.source.scope_kind.as_str() {
        "external_engagement" => {
            let facts = store.external_task(scope, task)?;
            if facts.specialist_run_id != run_id {
                return Err(integrity());
            }
            (
                crate::delegation_service::SettlementOwner::External {
                    engagement_id: scope,
                },
                facts.state,
            )
        }
        "orchestrated_session" => {
            let facts = crate::orchestration::successor_task(store.connection(), scope, task)?
                .ok_or_else(unavailable)?;
            if facts.member_ref != run_id.to_string() {
                return Err(integrity());
            }
            (
                crate::delegation_service::SettlementOwner::Managed { session_id: scope },
                facts.state,
            )
        }
        _ => return Err(integrity()),
    };
    let (digest, _, _) = crate::delegation_service::complete_result_of(
        &store,
        owner,
        run_id,
        task,
        &result.source.accepted_task_digest,
        &state,
        "result_component",
    )?
    .ok_or_else(unavailable)?;
    if digest != result.digest()? {
        return Err(integrity());
    }
    crate::delegation_service::original_delegation_task(state_root, result)?;
    Ok(())
}
fn read_component(
    state_root: &Path,
    p: &Publication,
    c: &Component,
) -> Result<Vec<u8>, MachineError> {
    let result = &p.result;
    let bytes = match c.selector.as_str() {
        "final_response" => {
            crate::delegation_service::original_delegation_response(state_root, result, false)?
                .ok_or_else(unavailable)?
        }
        "structured_output" => {
            let retained =
                crate::delegation_service::original_delegation_response(state_root, result, true)?;
            let projected =
                if result.structured_output_schema.as_deref() == Some("structured_review_v3") {
                    let raw = crate::delegation_service::original_delegation_response(
                        state_root, result, false,
                    )?
                    .ok_or_else(unavailable)?;
                    let accepted =
                        crate::delegation_service::original_delegation_task(state_root, result)?;
                    let store = EngagementStore::open_read_only(
                        &EngagementStore::workspace_database_path(state_root),
                    )?;
                    let task = crate::delegation_service::research_merge::task_projection(
                        store.connection(),
                        Uuid::parse_str(&result.source.task_ref).map_err(|_| integrity())?,
                        &accepted.task_body,
                    )?;
                    let output = crate::specialist::parse_reviewer_output_v3(
                        Some(std::str::from_utf8(&raw).map_err(|_| integrity())?),
                        &task,
                    )
                    .map_err(|_| integrity())?;
                    Some(crate::trusted_host::canonical(&output)?)
                } else {
                    None
                };
            if retained
                .as_ref()
                .zip(projected.as_ref())
                .is_some_and(|(a, b)| a != b)
            {
                return Err(integrity());
            }
            retained.or(projected).ok_or_else(unavailable)?
        }
        selector => {
            let mut parts = selector.split(':');
            if parts.next() != Some("change") {
                return Err(integrity());
            }
            let index: usize = parts
                .next()
                .ok_or_else(integrity)?
                .parse()
                .map_err(|_| integrity())?;
            let side = parts
                .next()
                .filter(|s| matches!(*s, "before" | "after"))
                .ok_or_else(integrity)?;
            if parts.next().is_some() {
                return Err(integrity());
            }
            let change = result.changes.get(index).ok_or_else(integrity)?;
            let path = if side == "before" {
                change.old_path.as_deref().unwrap_or(&change.path)
            } else {
                &change.path
            };
            let store = EngagementStore::open_read_only(
                &EngagementStore::workspace_database_path(state_root),
            )?;
            crate::engagement::retained_change_side(
                store.connection(),
                Uuid::parse_str(&result.source.task_ref).map_err(|_| integrity())?,
                path,
                side,
            )?
            .ok_or_else(unavailable)?
        }
    };
    if matches!(c.selector.as_str(), "final_response" | "structured_output")
        && std::str::from_utf8(&bytes).is_err()
    {
        return Err(integrity());
    }
    if BytesRef::from_bytes(&bytes) != c.reference {
        return Err(integrity());
    }
    Ok(bytes)
}
pub(crate) fn retained_source_identity(
    connection: &Connection,
    task: &str,
    digest: &str,
    body_free: Option<&CompleteResult>,
) -> Result<(CompleteResult, Value), MachineError> {
    if let Some(publication) = load(connection, task)? {
        if publication.result.digest()? != digest {
            return Err(integrity());
        }
        let reference = json!({"result_id":publication.result_id,"result_digest":digest,"source_manifest":generated_ref(&publication,publication.manifest_id,&manifest(&publication)?)});
        return Ok((publication.result, reference));
    }
    let record: IdentityPublication = serde_json::from_str(
        &crate::engagement::result_component_record(
            connection,
            &format!("{IDENTITY_PREFIX}{task}"),
        )?
        .ok_or_else(unavailable)?,
    )
    .map_err(|_| integrity())?;
    let result = body_free.ok_or_else(unavailable)?;
    if record.schema != "dolgorae.result-component-identity/v1"
        || record.result_id.get_version_num() != 7
        || record.result_digest != digest
        || result.digest()? != digest
        || result.source.task_ref != task
        || result.source.worker_ref != record.run_id.to_string()
        || result.final_response.is_some()
        || result.structured_output.is_some()
        || !result.changes.is_empty()
        || !result.attachments.is_empty()
    {
        return Err(integrity());
    }
    Ok((
        result.clone(),
        json!({"result_id":record.result_id,"result_digest":digest,"source_manifest":null,"discovery_unavailable_reason":"artifact_capacity_exceeded"}),
    ))
}
pub(crate) fn source_identity(
    state_root: &Path,
    view: &WorkspaceView,
    result: &CompleteResult,
) -> Result<Value, MachineError> {
    let store =
        EngagementStore::open_read_only(&EngagementStore::workspace_database_path(state_root))?;
    if load(store.connection(), &result.source.task_ref)?.is_none() {
        let row = public_result(state_root, view, result)?;
        return Ok(
            json!({"result_id":row["result_id"],"result_digest":row["result_digest"],"source_manifest":null,"discovery_unavailable_reason":row["discovery_unavailable_reason"]}),
        );
    }
    let publication = load(store.connection(), &result.source.task_ref)?.ok_or_else(unavailable)?;
    if publication.workspace_id != view.workspace_id
        || publication.result.digest()? != result.digest()?
    {
        return Err(integrity());
    }
    verify_source(state_root, &publication)?;
    Ok(
        json!({"result_id":publication.result_id,"result_digest":result.digest()?,"source_manifest":generated_ref(&publication,publication.manifest_id,&manifest(&publication)?)}),
    )
}
pub(crate) fn public_result(
    state_root: &Path,
    view: &WorkspaceView,
    result: &CompleteResult,
) -> Result<Value, MachineError> {
    let store =
        EngagementStore::open_read_only(&EngagementStore::workspace_database_path(state_root))?;
    let Some(p) = load(store.connection(), &result.source.task_ref)? else {
        let raw = crate::engagement::result_component_record(
            store.connection(),
            &format!("{IDENTITY_PREFIX}{}", result.source.task_ref),
        )?
        .ok_or_else(unavailable)?;
        let record: IdentityPublication = serde_json::from_str(&raw).map_err(|_| integrity())?;
        if record.schema != "dolgorae.result-component-identity/v1"
            || record.result_id.get_version_num() != 7
            || record.workspace_id != view.workspace_id
            || record.workspace_path
                != view
                    .canonical_path
                    .to_path_buf()?
                    .to_str()
                    .ok_or_else(integrity)?
            || record.run_id.to_string() != result.source.worker_ref
            || record.result_digest != result.digest()?
            || result.final_response.is_some()
            || result.structured_output.is_some()
            || !result.changes.is_empty()
            || !result.attachments.is_empty()
        {
            return Err(integrity());
        }
        verify_complete_source(state_root, record.run_id, result)?;
        return Ok(
            json!({"result_id":record.result_id,"identity":result,"result_digest":record.result_digest,"final_response":null,"structured_output":null,"change_contents":[],"attachment_contents":[],"source_manifest":null,"discovery_unavailable_reason":"artifact_capacity_exceeded"}),
        );
    };
    if p.workspace_id != view.workspace_id
        || p.workspace_path
            != view
                .canonical_path
                .to_path_buf()?
                .to_str()
                .ok_or_else(integrity)?
        || p.result.digest()? != result.digest()?
    {
        return Err(integrity());
    }
    verify_source(state_root, &p)?;
    let mut value = json!({"result_id":p.result_id,"identity":result,"result_digest":result.digest()?,"final_response":null,"structured_output":null,"change_contents":[],"attachment_contents":[]});
    // A result that already needs manifest discovery must not open every raw
    // change source merely to discard those inline bodies at page packing.
    // These exact artifact bindings remain readable through the source owner.
    let projection_bytes = p
        .components
        .iter()
        .try_fold(encode(result)?.len(), |bytes, c| {
            Ok::<_, MachineError>(bytes.saturating_add(encode(&component_ref(&p, c))?.len()))
        })?;
    let materialize_inline = projection_bytes <= INLINE as usize;
    let mut inline_remaining = INLINE;
    for c in &p.components {
        let content = if materialize_inline && c.reference.byte_length <= inline_remaining {
            let bytes = read_component(state_root, &p, c)?;
            inline_remaining -= c.reference.byte_length;
            json!({"content":c.reference,"media_type":c.media_type,"inline_base64":base64::engine::general_purpose::STANDARD.encode(bytes)})
        } else {
            component_ref(&p, c)
        };
        if matches!(c.selector.as_str(), "final_response" | "structured_output") {
            value[&c.selector] = content;
        } else {
            value["change_contents"]
                .as_array_mut()
                .ok_or_else(integrity)?
                .push(content);
        }
    }
    let bytes = manifest(&p)?;
    let manifest_component = Component {
        selector: "manifest".into(),
        reference: BytesRef::from_bytes(&bytes),
        media_type: "application/json".into(),
        artifact_id: p.manifest_id,
    };
    value["source_manifest"] = component_ref(&p, &manifest_component);
    verify_source(state_root, &p)?;
    Ok(value)
}
fn authorize(
    state_root: &Path,
    p: &Publication,
    carrier: Option<&CredentialCarrier>,
) -> Result<(), MachineError> {
    let carrier = carrier.ok_or_else(|| {
        MachineError::new(
            "CONTROLLER_REQUIRED",
            "this result artifact requires its aggregate Controller",
            false,
            json!({}),
        )
    })?;
    crate::delegation_service::research_group::public_owner_binding(
        state_root,
        &p.workspace_id,
        carrier,
        &p.result.source.scope_kind,
        Uuid::parse_str(&p.result.source.scope_ref).map_err(|_| integrity())?,
        None,
    )?;
    Ok(())
}
pub(crate) fn read_bound(
    state_root: &Path,
    run: Uuid,
    id: Uuid,
    carrier: Option<&CredentialCarrier>,
) -> Result<Option<(ArtifactReference, Vec<u8>)>, MachineError> {
    if let Some(artifact) =
        crate::delegation_service::research_merge::read_artifact(state_root, run, id, carrier)?
    {
        return Ok(Some(artifact));
    }
    let database = EngagementStore::workspace_database_path(state_root);
    if !database.exists() {
        return Ok(None);
    }
    let store = EngagementStore::open_read_only(&database)?;
    let Some(task) = crate::engagement::result_component_record(
        store.connection(),
        &format!("result-component-id:v1:{id}"),
    )?
    else {
        return Ok(None);
    };
    let p = load(store.connection(), &task)?.ok_or_else(integrity)?;
    if p.run_id != run {
        return Err(MachineError::new(
            "ARTIFACT_NOT_FOUND",
            "artifact is not owned by this Run",
            false,
            json!({"run_id":run,"artifact_id":id}),
        ));
    }
    authorize(state_root, &p, carrier)?;
    verify_source(state_root, &p)?;
    let (reference, media_type, bytes) = if let Some(bytes) = generated(&p, id)? {
        (
            BytesRef::from_bytes(&bytes),
            if p.identity_ids.contains(&id) {
                "application/octet-stream"
            } else {
                "application/json"
            }
            .to_owned(),
            bytes,
        )
    } else {
        let c = p
            .components
            .iter()
            .find(|c| c.artifact_id == id)
            .ok_or_else(integrity)?;
        (
            c.reference.clone(),
            c.media_type.clone(),
            read_component(state_root, &p, c)?,
        )
    };
    verify_source(state_root, &p)?;
    authorize(state_root, &p, carrier)?;
    Ok(Some((
        ArtifactReference {
            artifact_id: id.to_string(),
            created_at: String::new(),
            interaction_request_id: None,
            kind: ArtifactKind::FinalResponse,
            visibility: ArtifactVisibility::ControllerOnly,
            media_type,
            byte_length: reference.byte_length,
            sha256: reference
                .sha256
                .strip_prefix("sha256:")
                .ok_or_else(integrity)?
                .to_owned(),
        },
        bytes,
    )))
}
/// Ordinary lookup identity and published bindings remain last consumers.
pub(crate) fn protects_external_artifact(
    connection: &Connection,
    artifact: &str,
) -> Result<bool, MachineError> {
    let task: Option<String> = connection
        .query_row(
            "SELECT task_id FROM artifacts WHERE artifact_id=?1",
            [artifact],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| integrity())?;
    let Some(task) = task else {
        return Ok(false);
    };
    if load(connection, &task)?.is_some() {
        return Ok(true);
    }
    // A historical ordinary result can be the last retained lookup identity.
    // Retirement eligibility observes this proof; it never backfills artifacts.
    let ordinary: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM tasks WHERE task_id=?1 AND CASE WHEN json_valid(request_json) THEN json_extract(request_json,'$.schema') END='dolgorae.accepted-task/v1')",
        [&task], |row| row.get(0),
    ).map_err(|_| integrity())?;
    if ordinary {
        return Ok(true);
    }
    let namespace: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='delegation_operations')", [], |row| row.get(0),
    ).map_err(|_|integrity())?;
    if !namespace {
        return Ok(false);
    }
    connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM delegation_operations WHERE operation_kind='assign_external_specialist_task' AND state='accepted' AND json_extract(accepted_subjects_json,'$.task_ref')=?1)",
        [&task], |row|row.get(0),
    ).map_err(|_|integrity())
}
use rusqlite::OptionalExtension;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delegation_identity::{COMPLETE_RESULT_SCHEMA, ChangeRef, ResultSource};
    use crate::workspace::{LosslessPath, WorkspaceMode};
    use std::os::unix::fs::PermissionsExt as _;
    struct Fixture {
        root: std::path::PathBuf,
        view: WorkspaceView,
        run: Uuid,
    }
    impl Fixture {
        fn new() -> Self {
            let root =
                Path::new("/tmp").join(format!("dolgorae-result-components-{}", Uuid::now_v7()));
            std::fs::create_dir(&root).unwrap();
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
            let root = root.canonicalize().unwrap();
            let view = WorkspaceView {
                workspace_id: crate::workspace::workspace_id(&root),
                canonical_path: LosslessPath::Utf8(root.to_str().unwrap().into()),
                mode: WorkspaceMode::Git,
                created: true,
            };
            drop(EngagementStore::open(&EngagementStore::workspace_database_path(&root)).unwrap());
            Self {
                root,
                view,
                run: Uuid::now_v7(),
            }
        }
        fn result(&self, bytes: &[u8]) -> CompleteResult {
            CompleteResult {
                schema: COMPLETE_RESULT_SCHEMA.into(),
                source: ResultSource {
                    scope_kind: "external_engagement".into(),
                    scope_ref: Uuid::now_v7().to_string(),
                    worker_ref: self.run.to_string(),
                    task_ref: Uuid::now_v7().to_string(),
                    role_snapshot_digest: format!("sha256:{}", "a".repeat(64)),
                    agent_configuration_digest: format!("sha256:{}", "b".repeat(64)),
                    accepted_task_digest: format!("sha256:{}", "c".repeat(64)),
                },
                terminal_outcome: "completed".into(),
                final_response: Some(BytesRef::from_bytes(bytes)),
                structured_output_schema: None,
                structured_output: None,
                changes: vec![],
                attachments: vec![],
                safe_failure_code: None,
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).unwrap();
        }
    }
    #[test]
    fn identity_discovery_chunks_preserve_bytes_beyond_one_artifact() {
        let bytes = vec![b'a'; MAX_ARTIFACT as usize + 123];
        assert_eq!(
            identity_chunk(&bytes, 0).unwrap().len(),
            MAX_ARTIFACT as usize
        );
        assert_eq!(identity_chunk(&bytes, 1).unwrap().len(), 123);
        assert_eq!(
            [
                identity_chunk(&bytes, 0).unwrap(),
                identity_chunk(&bytes, 1).unwrap()
            ]
            .concat(),
            bytes
        );
        assert!(identity_chunk(&bytes, 2).is_err());
    }
    #[test]
    fn publication_binding_rolls_back_replays_and_rejects_identity_replacement() {
        let f = Fixture::new();
        let result = f.result(b"original");
        let p = Publication::prepare(&f.view, f.run, &result).unwrap();
        let encoded = encode(&p).unwrap();
        let reopened: Publication = serde_json::from_slice(&encoded).unwrap();
        assert!(reopened.prepared_logical_bytes.is_none());
        assert_eq!(
            p.logical_bytes().unwrap(),
            reopened.logical_bytes().unwrap()
        );
        let mut injected: Value = serde_json::from_slice(&encoded).unwrap();
        injected["prepared_logical_bytes"] = json!(0);
        assert!(serde_json::from_value::<Publication>(injected).is_err());
        let mut store =
            EngagementStore::open(&EngagementStore::workspace_database_path(&f.root)).unwrap();
        assert!(
            store
                .transact(|tx| {
                    p.retain(tx)?;
                    Err::<(), _>(unavailable())
                })
                .is_err()
        );
        assert!(
            load(store.connection(), &result.source.task_ref)
                .unwrap()
                .is_none()
        );
        store.transact(|tx| p.retain(tx)).unwrap();
        store.transact(|tx| p.retain(tx)).unwrap();
        assert_eq!(
            load(store.connection(), &result.source.task_ref)
                .unwrap()
                .unwrap()
                .result_id,
            p.result_id
        );
        assert_eq!(
            logical_bytes(&f.root, f.run).unwrap(),
            p.logical_bytes().unwrap()
        );
        let mut replacement = p.clone();
        replacement.result_id = Uuid::now_v7();
        assert!(store.transact(|tx| replacement.retain(tx)).is_err());
        let mut corrupt = p.clone();
        corrupt.components[0].selector = "structured_output".into();
        assert!(corrupt.validate(&result.source.task_ref).is_err());
    }
    #[test]
    fn complete_twenty_thousand_changes_use_bounded_manifest_pages() {
        let f = Fixture::new();
        let mut result = f.result(b"answer");
        result.changes = (0..20_000)
            .map(|index| ChangeRef {
                kind: "modify".into(),
                path: format!("file-{index:05}.rs"),
                old_path: None,
                before: Some(BytesRef::from_bytes(b"a")),
                after: Some(BytesRef::from_bytes(b"b")),
                before_mode: Some(0o100644),
                after_mode: Some(0o100644),
            })
            .collect();
        let p = Publication::prepare(&f.view, f.run, &result).unwrap();
        assert_eq!(p.components.len(), 40_001);
        assert_eq!(p.page_ids.len(), 157);
        assert!(encode(&result).unwrap().len() > 4 * 1024 * 1024);
        assert!(manifest(&p).unwrap().len() < INLINE as usize);
        for index in 0..p.page_ids.len() {
            assert!(manifest_page(&p, index).unwrap().len() < INLINE as usize);
        }
        let root: Value = serde_json::from_slice(&manifest(&p).unwrap()).unwrap();
        let identity = encode(&result).unwrap();
        let generated_bytes = (manifest(&p).unwrap().len() + identity.len()) as u64
            + (0..p.page_ids.len())
                .map(|index| manifest_page(&p, index).unwrap().len() as u64)
                .sum::<u64>();
        let content_bytes = p
            .components
            .iter()
            .map(|c| c.reference.byte_length)
            .sum::<u64>();
        assert_eq!(p.logical_bytes().unwrap(), generated_bytes + content_bytes);
        let reopened: Publication = serde_json::from_slice(&encode(&p).unwrap()).unwrap();
        assert_eq!(
            reopened.logical_bytes().unwrap(),
            p.logical_bytes().unwrap()
        );
        assert_eq!(root["result_digest"], result.digest().unwrap());
        assert_eq!(
            root["identity"]["content"],
            serde_json::to_value(BytesRef::from_bytes(&encode(&result).unwrap())).unwrap()
        );
        assert!(p.logical_bytes().unwrap() < crate::artifact::MAX_RUN_ARTIFACT_BYTES);
    }
    #[test]
    fn large_component_reads_exact_retained_bytes_and_rejects_corruption_and_retirement() {
        let f = Fixture::new();
        let bytes = (0..1_100_003)
            .map(|i| (32 + i % 95) as u8)
            .collect::<Vec<_>>();
        let result = f.result(&bytes);
        let task = Uuid::parse_str(&result.source.task_ref).unwrap();
        let p = Publication::prepare(&f.view, f.run, &result).unwrap();
        let mut store =
            EngagementStore::open(&EngagementStore::workspace_database_path(&f.root)).unwrap();
        store
            .transact(|tx| crate::engagement::retain_checked_task_output(tx, task, b"{}", &bytes))
            .unwrap();
        assert_eq!(
            read_component(&f.root, &p, &p.components[0]).unwrap(),
            bytes
        );
        let mut incorrect = p.components[0].clone();
        incorrect.reference.byte_length -= 1;
        assert_eq!(
            read_component(&f.root, &p, &incorrect).unwrap_err().code,
            "ARTIFACT_INTEGRITY_FAILURE"
        );
        store.connection().execute("UPDATE result_content_refs SET retired_operation=?1 WHERE owner_kind='accepted_task' AND owner_id=?2 AND representation='final_response'",rusqlite::params![Uuid::now_v7().to_string(),task.to_string()]).unwrap();
        assert!(read_component(&f.root, &p, &p.components[0]).is_err());
    }
}
