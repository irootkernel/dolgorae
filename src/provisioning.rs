//! Content-addressed successor provisioning snapshots.
//!
//! One installed provisioning snapshot retains the exact captured Markdown
//! bytes, the selected Profile binding and its admitted configuration, so a
//! later hire or spawn never re-reads a mutable Role source (MDC:304-306,
//! CW:233-241). `validate_provisioning` is a read-only call that writes nothing;
//! `install_provisioning` publishes the snapshot at its content address and
//! re-reads it under the same secure-read rules before anyone may consume it.

use crate::delegation_identity::{
    AdmittedConfiguration, BytesRef, InstructionIdentity, InstructionSnapshot, RoleSnapshot,
};
use crate::global_runtime::GlobalProfileBinding;
use crate::instructions::{
    CapturedNamedSource, DelegationCapture, DelegationSourceSelection, InstructionSourceStore,
    RoleCapture, SelectedInstructionSource, SelectedNamedSource, SourceCapture,
};
use crate::jcs::{canonicalize, parse, sha256_hex};
use crate::machine::MachineError;
use crate::run::{
    ADMITTED_PROFILE_BINDING_SCHEMA, AdmittedConfigurationInput, AgentConfigurationSnapshot,
};
use serde::{Deserialize, Serialize};
use std::io::Read as _;
use std::path::{Path, PathBuf};

/// The runtime shape of one retained provisioning snapshot. It carries the
/// exact accepted bytes, so it is a runtime object rather than a protocol
/// schema: the closed identity it produces is `instructions/v3` plus
/// `admitted-configuration/v1`.
pub const SNAPSHOT_SCHEMA: &str = "dolgorae.provisioning-snapshot/runtime-v1";
/// The content address prefix of one installed snapshot.
pub const PROVISIONING_REF_PREFIX: &str = "provisioning:sha256:";
/// A snapshot is a Profile binding plus admitted configuration plus at most the
/// 65,536 aggregate captured source bytes; the retained-file read is bounded by
/// the same ceiling the caller-start reservation already uses (src/run.rs:24).
const MAX_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;

/// The admission policy one provisioning selection carries. Accepting the
/// policy fields without their digests keeps every digest derived from the
/// captured bytes instead of caller-supplied.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProvisioningAdmission {
    pub model: String,
    pub effort: String,
    pub purpose_kind: String,
    pub purpose_label: Option<String>,
    pub required_capabilities: Vec<String>,
    pub execution_lane: String,
    pub requested_access: String,
    pub required_assurance: String,
    pub native_subagent_policy: String,
}

/// The trusted Markdown selection and Profile authority one provisioning uses.
pub struct ProvisioningSources<'a> {
    pub instruct: &'a InstructionSourceStore,
    pub role_name: &'a str,
    pub role: SelectedInstructionSource,
    pub dependencies: Vec<SelectedNamedSource>,
    pub selected_guidance: Vec<SelectedNamedSource>,
    pub profile_binding: GlobalProfileBinding,
    pub admission: ProvisioningAdmission,
}

/// The verified identity of one provisioning candidate.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProvisioningCandidate {
    pub provisioning_ref: String,
    pub role_snapshot_digest: String,
    pub instruction_identity_digest: String,
    /// `dolgorae.admitted-configuration/v1` domain digest of the admitted
    /// configuration (MDC:608). The runtime Agent Configuration hash is a
    /// distinct internal value and never appears here.
    pub agent_configuration_digest: String,
    pub profile_binding_digest: String,
    pub source_bytes: u64,
    pub installed: bool,
}

/// The closed `provisioning_sources` identity the facade request carries: only
/// identities, never bytes (facade `$defs/provisioning_sources`).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProvisioningSourcesIdentity {
    pub common: BytesRef,
    pub role: RoleSnapshot,
    pub instruction_identity: InstructionIdentity,
    pub configuration: AdmittedConfiguration,
}

/// One captured named source retained with its exact bytes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetainedNamedSource {
    pub name: String,
    pub body: String,
}

/// One installed provisioning snapshot, re-readable without any Role source.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetainedSnapshot {
    pub schema: String,
    pub common: SourceCapture,
    pub role: RoleCapture,
    pub dependencies: Vec<RetainedNamedSource>,
    pub selected_guidance: Vec<RetainedNamedSource>,
    pub profile: String,
    pub profile_binding: GlobalProfileBinding,
    pub admitted: AdmittedConfiguration,
}

impl RetainedSnapshot {
    /// Build the retained snapshot from accepted capture bytes and admission.
    fn from_capture(
        capture: DelegationCapture,
        profile_binding: GlobalProfileBinding,
        admission: ProvisioningAdmission,
    ) -> Result<Self, MachineError> {
        // The closed identity is the only admission gate for the capture rules
        // (sorted unique dependency aliases, ordered guidance, 65,536-byte
        // aggregate). Nothing is retained that it would refuse.
        let instructions = InstructionSnapshot::from_capture(capture.clone())?;
        profile_binding.validate_for_recovery()?;
        let mut required_capabilities = admission.required_capabilities.clone();
        required_capabilities.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
        required_capabilities.dedup();
        let admitted = AdmittedConfiguration {
            schema: "dolgorae.admitted-configuration/v1".to_owned(),
            profile_binding_schema: ADMITTED_PROFILE_BINDING_SCHEMA.to_owned(),
            profile_binding_digest: format!("sha256:{}", profile_binding.digest()?),
            model: admission.model,
            effort: admission.effort,
            purpose_kind: admission.purpose_kind,
            purpose_label: admission.purpose_label,
            required_capabilities,
            execution_lane: admission.execution_lane,
            requested_access: admission.requested_access,
            required_assurance: admission.required_assurance,
            native_subagent_policy: admission.native_subagent_policy,
            instruction_identity_digest: instructions.instruction_identity_digest.clone(),
        };
        // Refuse an admission the closed configuration branches cannot carry
        // (MDC:335-337) before any snapshot bytes exist.
        admitted.digest()?;
        Ok(Self {
            schema: SNAPSHOT_SCHEMA.to_owned(),
            common: capture.common,
            role: capture.role,
            dependencies: capture
                .dependencies
                .into_iter()
                .map(|source| RetainedNamedSource {
                    name: source.name,
                    body: source.body,
                })
                .collect(),
            selected_guidance: capture
                .selected_guidance
                .into_iter()
                .map(|source| RetainedNamedSource {
                    name: source.name,
                    body: source.body,
                })
                .collect(),
            profile: profile_binding.selected_name.clone(),
            profile_binding,
            admitted,
        })
    }

    /// The exact accepted capture bytes this snapshot retains.
    #[must_use]
    pub fn capture(&self) -> DelegationCapture {
        let named = |sources: &[RetainedNamedSource]| {
            sources
                .iter()
                .map(|source| CapturedNamedSource {
                    name: source.name.clone(),
                    body: source.body.clone(),
                })
                .collect::<Vec<_>>()
        };
        DelegationCapture {
            common: self.common.clone(),
            role: self.role.clone(),
            dependencies: named(&self.dependencies),
            selected_guidance: named(&self.selected_guidance),
        }
    }

    /// The selected Profile authority the admitted member runs under.
    #[must_use]
    pub fn profile_binding(&self) -> &GlobalProfileBinding {
        &self.profile_binding
    }

    /// The Role name this snapshot was captured for.
    #[must_use]
    pub fn role_name(&self) -> &str {
        &self.role.name
    }

    /// Rebuild the closed instruction identity and the generation-4 Agent
    /// Configuration from the retained bytes alone.
    ///
    /// This is the single admission gate every allocation must pass: a start
    /// that has not produced this pair may not allocate a Run.
    pub fn admitted_configuration(
        &self,
    ) -> Result<(InstructionSnapshot, AgentConfigurationSnapshot), MachineError> {
        let instructions = InstructionSnapshot::from_capture(self.capture())?;
        let configuration =
            AgentConfigurationSnapshot::from_admitted(AdmittedConfigurationInput {
                admitted: self.admitted.clone(),
                instructions: instructions.clone(),
                profile: &self.profile,
                profile_binding: &self.profile_binding,
                role_reference: Some(self.role.name.as_str()),
            })?;
        Ok((instructions, configuration))
    }

    /// The closed `provisioning_sources` identity of this snapshot.
    pub fn sources_identity(&self) -> Result<ProvisioningSourcesIdentity, MachineError> {
        let instructions = InstructionSnapshot::from_capture(self.capture())?;
        Ok(ProvisioningSourcesIdentity {
            common: BytesRef::from_bytes(self.common.body.as_bytes()),
            role: instructions.role_snapshot,
            instruction_identity: instructions.instruction_identity,
            configuration: self.admitted.clone(),
        })
    }

    /// The content address of this snapshot's exact identity.
    pub fn provisioning_ref(&self) -> Result<String, MachineError> {
        let bytes = self.canonical_bytes()?;
        Ok(format!("{PROVISIONING_REF_PREFIX}{}", sha256_hex(&bytes)))
    }

    fn canonical_bytes(&self) -> Result<Vec<u8>, MachineError> {
        let text = serde_json::to_string(self)
            .map_err(|_| snapshot_invalid("the provisioning snapshot is unrepresentable"))?;
        canonicalize(
            &parse(&text).map_err(|_| {
                snapshot_invalid("the provisioning snapshot is not canonicalizable")
            })?,
        )
        .map_err(|_| snapshot_invalid("the provisioning snapshot is not canonicalizable"))
    }

    fn verify(&self) -> Result<(), MachineError> {
        if self.schema != SNAPSHOT_SCHEMA {
            return Err(snapshot_corrupt(
                "the provisioning snapshot has another schema",
            ));
        }
        self.common.validate()?;
        self.role.validate()?;
        for source in self.dependencies.iter().chain(&self.selected_guidance) {
            SourceCapture::from_body(&source.body)?;
            if source.name.is_empty() {
                return Err(snapshot_corrupt("a retained source alias is empty"));
            }
        }
        self.profile_binding.validate_for_recovery()?;
        let expected = format!("sha256:{}", self.profile_binding.digest()?);
        if self.admitted.profile_binding_digest != expected
            || self.profile_binding.selected_name != self.profile
        {
            return Err(snapshot_corrupt(
                "the retained Profile binding differs from its admission",
            ));
        }
        self.admitted.digest()?;
        Ok(())
    }
}

/// Capture and validate one provisioning selection without any effect.
///
/// The returned snapshot is a complete in-memory accepted identity; calling it
/// twice with unchanged sources yields the same bytes and content address.
pub fn capture_provisioning(
    sources: &ProvisioningSources<'_>,
) -> Result<RetainedSnapshot, MachineError> {
    capture_snapshot(sources)
}

/// Validate one provisioning selection without any filesystem or database effect.
pub fn validate_provisioning(
    sources: &ProvisioningSources<'_>,
) -> Result<ProvisioningCandidate, MachineError> {
    let snapshot = capture_snapshot(sources)?;
    snapshot.candidate(false)
}

/// Publish one verified provisioning snapshot and re-read it securely.
///
/// The publication is content addressed, so an interrupted install can only
/// leave an unreferenced snapshot behind; a retry converges on the same bytes.
/// Only a successful install makes the snapshot referencable.
pub fn install_provisioning(
    sources: &ProvisioningSources<'_>,
    state_root: &Path,
) -> Result<(ProvisioningCandidate, RetainedSnapshot), MachineError> {
    let snapshot = capture_snapshot(sources)?;
    install_snapshot(state_root, &snapshot)?;
    let candidate = snapshot.candidate(true)?;
    Ok((candidate, snapshot))
}

/// Publish one already captured snapshot at its content address.
pub fn install_snapshot(
    state_root: &Path,
    snapshot: &RetainedSnapshot,
) -> Result<(), MachineError> {
    let reference = snapshot.provisioning_ref()?;
    publish_snapshot(state_root, snapshot)?;
    let retained = retained_snapshot(state_root, &reference)?;
    if retained != *snapshot {
        return Err(snapshot_corrupt(
            "the retained provisioning snapshot differs from its accepted identity",
        ));
    }
    Ok(())
}

/// Read one installed provisioning snapshot under the secure-read rules.
pub fn retained_snapshot(
    state_root: &Path,
    provisioning_ref: &str,
) -> Result<RetainedSnapshot, MachineError> {
    let hex = provisioning_ref
        .strip_prefix(PROVISIONING_REF_PREFIX)
        .filter(|hex| {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .ok_or_else(provisioning_ref_unknown)?;
    let path = snapshot_path(state_root, hex);
    // The retained snapshot is opened through the existing descriptor-based
    // secure opener (O_NOFOLLOW plus a verified opened file), and its recorded
    // length is checked before any buffer is allocated.
    let Some(file) = crate::workspace::open_secure_file_if_present(&path, current_uid())? else {
        return Err(provisioning_ref_unknown());
    };
    let length = file
        .metadata()
        .map_err(|_| snapshot_corrupt("the provisioning snapshot is unreadable"))?
        .len();
    let maximum = u64::try_from(MAX_SNAPSHOT_BYTES).unwrap_or(u64::MAX);
    if length > maximum {
        return Err(snapshot_corrupt(
            "the provisioning snapshot exceeds its bound",
        ));
    }
    // Read through one byte past the bound, so a file that grows after the
    // metadata check can never be read or allocated beyond it.
    let mut bytes = Vec::with_capacity(usize::try_from(length).unwrap_or(0));
    let mut limited = file.take(maximum.saturating_add(1));
    limited
        .read_to_end(&mut bytes)
        .map_err(|_| snapshot_corrupt("the provisioning snapshot is unreadable"))?;
    if bytes.len() > MAX_SNAPSHOT_BYTES {
        return Err(snapshot_corrupt(
            "the provisioning snapshot exceeds its bound",
        ));
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    if sha256_hex(&bytes) != hex {
        return Err(snapshot_corrupt(
            "the provisioning snapshot does not match its content address",
        ));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| snapshot_corrupt("the provisioning snapshot is not UTF-8"))?;
    let canonical = canonicalize(
        &parse(text).map_err(|_| snapshot_corrupt("the provisioning snapshot is invalid"))?,
    )
    .map_err(|_| snapshot_corrupt("the provisioning snapshot is invalid"))?;
    let snapshot: RetainedSnapshot = serde_json::from_slice(&canonical)
        .map_err(|_| snapshot_corrupt("the provisioning snapshot shape is invalid"))?;
    snapshot.verify()?;
    if snapshot.provisioning_ref()? != provisioning_ref {
        return Err(snapshot_corrupt(
            "the provisioning snapshot identity differs from its content address",
        ));
    }
    Ok(snapshot)
}

fn capture_snapshot(sources: &ProvisioningSources<'_>) -> Result<RetainedSnapshot, MachineError> {
    let selection = DelegationSourceSelection {
        role_name: sources.role_name.to_owned(),
        role: sources.role.clone(),
        dependencies: sources.dependencies.clone(),
        selected_guidance: sources.selected_guidance.clone(),
    };
    let capture = sources.instruct.capture_delegation(&selection)?;
    RetainedSnapshot::from_capture(
        capture,
        sources.profile_binding.clone(),
        sources.admission.clone(),
    )
}

impl RetainedSnapshot {
    /// The candidate identity this snapshot publishes at install time.
    pub fn candidate(&self, installed: bool) -> Result<ProvisioningCandidate, MachineError> {
        let identity = self.sources_identity()?;
        // MDC:608: the published digest is the admitted configuration's own
        // domain digest; the runtime snapshot hash stays internal.
        let agent_configuration_digest = self.admitted.digest()?;
        Ok(ProvisioningCandidate {
            provisioning_ref: self.provisioning_ref()?,
            role_snapshot_digest: identity.instruction_identity.role_snapshot_digest.clone(),
            instruction_identity_digest: identity.instruction_identity.digest()?,
            agent_configuration_digest,
            profile_binding_digest: identity.configuration.profile_binding_digest.clone(),
            source_bytes: u64::try_from(
                self.common.body.len()
                    + self.role.body.len()
                    + self
                        .dependencies
                        .iter()
                        .chain(&self.selected_guidance)
                        .map(|source| source.body.len())
                        .sum::<usize>(),
            )
            .map_err(|_| snapshot_invalid("the captured source size is invalid"))?,
            installed,
        })
    }
}

fn publish_snapshot(state_root: &Path, snapshot: &RetainedSnapshot) -> Result<(), MachineError> {
    let reference = snapshot.provisioning_ref()?;
    let hex = reference
        .strip_prefix(PROVISIONING_REF_PREFIX)
        .ok_or_else(provisioning_ref_unknown)?;
    let path = snapshot_path(state_root, hex);
    let root = path
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| snapshot_invalid("the provisioning path is invalid"))?;
    let platform = crate::workspace::SystemWorkspacePlatform;
    let uid = current_uid();
    let directory = path
        .parent()
        .ok_or_else(|| snapshot_invalid("the provisioning path is invalid"))?;
    for directory in [root, directory] {
        ensure_private_directory(directory, uid)?;
    }
    let mut bytes = snapshot.canonical_bytes()?;
    bytes.push(b'\n');
    match crate::workspace::atomic_create(&platform, &path, &bytes, 0o600) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            // The content address already holds these exact bytes; a mismatch
            // is a corrupted retained snapshot, never a silent replacement.
            let retained = retained_snapshot(state_root, &reference)?;
            if retained == *snapshot {
                Ok(())
            } else {
                Err(snapshot_corrupt(
                    "the installed provisioning snapshot differs from these sources",
                ))
            }
        }
        Err(_) => Err(snapshot_invalid(
            "the provisioning snapshot cannot be published",
        )),
    }
}

fn current_uid() -> u32 {
    use crate::workspace::WorkspacePlatform as _;
    crate::workspace::SystemWorkspacePlatform.current_uid()
}

/// Require one private directory without ever changing a mode through a path
/// that may be a symlink: an existing entry is verified, a missing one is
/// created at 0700.
fn ensure_private_directory(path: &Path, uid: u32) -> Result<(), MachineError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => crate::workspace::verify_secure_directory(path, uid),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            crate::workspace::create_directory(path, 0o700)
                .map_err(|_| snapshot_invalid("the provisioning directory cannot be created"))?;
            crate::workspace::verify_secure_directory(path, uid)
        }
        Err(_) => Err(snapshot_invalid("the provisioning directory is unreadable")),
    }
}

fn snapshot_path(state_root: &Path, hex: &str) -> PathBuf {
    state_root
        .join("provisioning")
        .join("sha256")
        .join(format!("{hex}.json"))
}

fn provisioning_ref_unknown() -> MachineError {
    MachineError::new(
        "PROVISIONING_REF_UNKNOWN",
        "the provisioning reference names no installed snapshot",
        false,
        serde_json::json!({"required_action":"fix_request","effect":"none"}),
    )
}

fn snapshot_corrupt(reason: &str) -> MachineError {
    MachineError::new(
        "RECOVERY_REQUIRED",
        reason,
        false,
        serde_json::json!({
            "subject":"provisioning_snapshot",
            "required_action":"restore_state",
            "effect":"none"
        }),
    )
}

fn snapshot_invalid(reason: &str) -> MachineError {
    MachineError::new(
        "INVALID_ARGUMENT",
        reason,
        false,
        serde_json::json!({"argument":"provisioning","effect":"none"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instructions::SelectedInstructionSource;
    use crate::paths::DolgoraeHome;
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;
    use uuid::Uuid;

    struct Fixture {
        root: PathBuf,
        home_root: PathBuf,
        state_root: PathBuf,
        codex_home: PathBuf,
        store: InstructionSourceStore,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("dolgorae-prov-{}", Uuid::now_v7()));
            fs::create_dir(&root).unwrap();
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
            let root = root.canonicalize().unwrap();
            let home = DolgoraeHome::from_canonical_home(root.clone()).unwrap();
            fs::create_dir(home.root()).unwrap();
            fs::set_permissions(home.root(), fs::Permissions::from_mode(0o700)).unwrap();
            let home_root = home.root().to_path_buf();
            let store = InstructionSourceStore::new(&home);
            store.initialize_templates().unwrap();
            let state_root = root.join("state");
            fs::create_dir(&state_root).unwrap();
            fs::set_permissions(&state_root, fs::Permissions::from_mode(0o700)).unwrap();
            let codex_home = root.join("codex-home");
            fs::create_dir(&codex_home).unwrap();
            fs::set_permissions(&codex_home, fs::Permissions::from_mode(0o700)).unwrap();
            Self {
                root,
                home_root,
                state_root,
                codex_home,
                store,
            }
        }

        fn role(&self, name: &str) -> PathBuf {
            self.home_root.join("roles").join(name).join("AGENTS.md")
        }

        fn sources(&self, binding: GlobalProfileBinding) -> ProvisioningSources<'_> {
            ProvisioningSources {
                instruct: &self.store,
                role_name: "reviewer",
                role: SelectedInstructionSource::File(vec![
                    "roles".to_owned(),
                    "reviewer".to_owned(),
                ]),
                dependencies: vec![SelectedNamedSource {
                    name: "contract".to_owned(),
                    source: SelectedInstructionSource::Inline(
                        "The accepted contract is complete.\n".to_owned(),
                    ),
                }],
                selected_guidance: Vec::new(),
                profile_binding: binding,
                admission: ProvisioningAdmission {
                    model: "gpt-5.6".to_owned(),
                    effort: "high".to_owned(),
                    purpose_kind: "review".to_owned(),
                    purpose_label: None,
                    required_capabilities: Vec::new(),
                    execution_lane: "dedicated".to_owned(),
                    requested_access: "read_only".to_owned(),
                    required_assurance: "best_effort_personal_alpha".to_owned(),
                    native_subagent_policy: "disabled".to_owned(),
                },
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn fixture_binding() -> GlobalProfileBinding {
        let reservation: crate::run::CallerStartReservation = serde_json::from_str(include_str!(
            "../docs/protocol/examples/workspace-execution-start-reservation.valid.json"
        ))
        .unwrap();
        reservation.accepted_intent.global_profile_binding
    }

    fn binding(codex_home: &Path) -> GlobalProfileBinding {
        let mut binding = fixture_binding();
        binding.launch_snapshot.canonical_codex_home = codex_home.to_str().unwrap().to_owned();
        binding.launch_snapshot_sha256 =
            sha256_hex(&canonical_bytes(&binding.launch_snapshot).unwrap());
        binding
    }

    fn canonical_bytes(value: &impl Serialize) -> Result<Vec<u8>, MachineError> {
        let text = serde_json::to_string(value).map_err(|_| snapshot_invalid("unrepresentable"))?;
        canonicalize(&parse(&text).map_err(|_| snapshot_invalid("invalid"))?)
            .map_err(|_| snapshot_invalid("invalid"))
    }

    #[test]
    fn validate_writes_nothing_and_derives_a_content_address() {
        let fixture = Fixture::new();
        let sources = fixture.sources(binding(&fixture.codex_home));
        let candidate = validate_provisioning(&sources).unwrap();
        assert!(!candidate.installed);
        assert!(
            candidate
                .provisioning_ref
                .starts_with(PROVISIONING_REF_PREFIX)
        );
        assert_eq!(
            candidate.provisioning_ref.len(),
            PROVISIONING_REF_PREFIX.len() + 64
        );
        assert!(candidate.source_bytes > 0);
        let again = validate_provisioning(&fixture.sources(binding(&fixture.codex_home))).unwrap();
        assert_eq!(candidate, again);
        assert!(
            !fixture.state_root.join("provisioning").exists(),
            "validation creates no provisioning state"
        );
    }

    #[test]
    fn install_is_content_addressed_and_rereads_securely() {
        let fixture = Fixture::new();
        let (candidate, snapshot) = install_provisioning(
            &fixture.sources(binding(&fixture.codex_home)),
            &fixture.state_root,
        )
        .unwrap();
        assert!(candidate.installed);
        let hex = candidate
            .provisioning_ref
            .strip_prefix(PROVISIONING_REF_PREFIX)
            .unwrap();
        let path = fixture
            .state_root
            .join("provisioning/sha256")
            .join(format!("{hex}.json"));
        assert!(path.is_file());
        let retained = retained_snapshot(&fixture.state_root, &candidate.provisioning_ref).unwrap();
        assert_eq!(retained, snapshot);
        // A second install of the same bytes converges on the same address.
        let (again, _) = install_provisioning(
            &fixture.sources(binding(&fixture.codex_home)),
            &fixture.state_root,
        )
        .unwrap();
        assert_eq!(again, candidate);

        // A tampered snapshot is a recovery condition, never a silent reuse.
        let mut bytes = fs::read(&path).unwrap();
        bytes.extend_from_slice(b" ");
        fs::write(&path, &bytes).unwrap();
        assert_eq!(
            retained_snapshot(&fixture.state_root, &candidate.provisioning_ref)
                .unwrap_err()
                .code,
            "RECOVERY_REQUIRED"
        );
        assert_eq!(
            retained_snapshot(&fixture.state_root, "provisioning:sha256:absent")
                .unwrap_err()
                .code,
            "PROVISIONING_REF_UNKNOWN"
        );
    }

    #[test]
    fn source_drift_changes_the_identity_without_mutating_an_installed_snapshot() {
        let fixture = Fixture::new();
        let (installed, snapshot) = install_provisioning(
            &fixture.sources(binding(&fixture.codex_home)),
            &fixture.state_root,
        )
        .unwrap();
        let path = fixture.state_root.join("provisioning/sha256").join(format!(
            "{}.json",
            installed
                .provisioning_ref
                .strip_prefix(PROVISIONING_REF_PREFIX)
                .unwrap()
        ));
        let bytes = fs::read(&path).unwrap();
        let role = fixture.role("reviewer");
        let mut edited = fs::read(&role).unwrap();
        edited.extend_from_slice(b"Changed.\n");
        fs::write(&role, &edited).unwrap();
        let drifted =
            validate_provisioning(&fixture.sources(binding(&fixture.codex_home))).unwrap();
        assert_ne!(drifted.provisioning_ref, installed.provisioning_ref);
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(
            retained_snapshot(&fixture.state_root, &installed.provisioning_ref).unwrap(),
            snapshot
        );
    }

    #[test]
    fn a_capture_the_closed_identity_refuses_is_never_retained() {
        let fixture = Fixture::new();
        // One alias may name only one dependency.
        let mut duplicate = fixture.sources(binding(&fixture.codex_home));
        duplicate.dependencies.push(SelectedNamedSource {
            name: "contract".to_owned(),
            source: SelectedInstructionSource::Inline("Second copy.\n".to_owned()),
        });
        assert_eq!(
            validate_provisioning(&duplicate).unwrap_err().code,
            "INSTRUCTION_SOURCE_INVALID"
        );
        // The 65,536-byte aggregate covers common, Role, dependencies and
        // guidance: two in-bound sources can still exceed it together.
        let mut oversized = fixture.sources(binding(&fixture.codex_home));
        oversized.dependencies = vec![
            SelectedNamedSource {
                name: "first".to_owned(),
                source: SelectedInstructionSource::Inline("a".repeat(40_000)),
            },
            SelectedNamedSource {
                name: "second".to_owned(),
                source: SelectedInstructionSource::Inline("b".repeat(40_000)),
            },
        ];
        assert_eq!(
            validate_provisioning(&oversized).unwrap_err().code,
            "INSTRUCTION_SOURCE_INVALID"
        );
        assert!(!fixture.state_root.join("provisioning").exists());
    }

    #[test]
    fn the_published_configuration_digest_is_the_admitted_domain_digest() {
        let fixture = Fixture::new();
        let snapshot =
            capture_provisioning(&fixture.sources(binding(&fixture.codex_home))).unwrap();
        let candidate = snapshot.candidate(false).unwrap();
        // MDC:608: the public digest is the admitted configuration's own domain
        // digest, never the runtime Agent Configuration hash.
        assert_eq!(
            candidate.agent_configuration_digest,
            snapshot.admitted.digest().unwrap()
        );
        let (_, configuration) = snapshot.admitted_configuration().unwrap();
        assert_ne!(
            candidate.agent_configuration_digest,
            format!(
                "sha256:{}",
                crate::run::agent_configuration_digest(&configuration).unwrap()
            )
        );
        let installed = retained_snapshot(
            &fixture.state_root,
            &install_provisioning(
                &fixture.sources(binding(&fixture.codex_home)),
                &fixture.state_root,
            )
            .unwrap()
            .0
            .provisioning_ref,
        )
        .unwrap();
        assert_eq!(
            installed
                .candidate(true)
                .unwrap()
                .agent_configuration_digest,
            candidate.agent_configuration_digest
        );
    }

    #[test]
    fn a_symlinked_or_oversized_snapshot_is_refused() {
        let fixture = Fixture::new();
        let candidate =
            validate_provisioning(&fixture.sources(binding(&fixture.codex_home))).unwrap();
        let hex = candidate
            .provisioning_ref
            .strip_prefix(PROVISIONING_REF_PREFIX)
            .unwrap()
            .to_owned();
        let directory = fixture.state_root.join("provisioning/sha256");
        let path = directory.join(format!("{hex}.json"));
        fs::create_dir_all(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        let target = fixture.codex_home.join("real.json");
        fs::write(&target, b"{}").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        // A symlinked snapshot is refused by the descriptor-based secure opener.
        assert_eq!(
            retained_snapshot(&fixture.state_root, &candidate.provisioning_ref)
                .unwrap_err()
                .code,
            "RUNTIME_PATH_INVALID"
        );
        fs::remove_file(&path).unwrap();
        // An oversized file is refused from its recorded length before any read.
        let file = fs::File::create(&path).unwrap();
        file.set_len(u64::try_from(MAX_SNAPSHOT_BYTES).unwrap() + 1)
            .unwrap();
        drop(file);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            retained_snapshot(&fixture.state_root, &candidate.provisioning_ref)
                .unwrap_err()
                .code,
            "RECOVERY_REQUIRED"
        );
        fs::remove_file(&path).unwrap();
        // A symlinked private directory is refused before any publication.
        fs::remove_dir(&directory).unwrap();
        std::os::unix::fs::symlink(&fixture.codex_home, &directory).unwrap();
        assert_eq!(
            install_provisioning(
                &fixture.sources(binding(&fixture.codex_home)),
                &fixture.state_root
            )
            .unwrap_err()
            .code,
            "RUNTIME_PATH_INVALID"
        );
    }
}
