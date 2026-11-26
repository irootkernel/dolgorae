//! Markdown + YAML frontmatter Role / Persona loader.
//!
//! Resolution order: `<workspace>/.dolgorae/roles/*.md` before `<home_dir>/roles/*.md`.
//! Workspace role files are treated as untrusted inputs capped at 64 KiB each.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use dolgorae_spi::{AccessMode, ApprovalPolicy};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::CoreError;
use crate::types::{RoleDetail, RoleSource, RoleSummary};

/// Maximum permitted size of a single role Markdown file (64 KiB).
pub const MAX_ROLE_FILE_BYTES: usize = 64 * 1024;

#[derive(Debug, Default, Deserialize)]
struct RoleFrontmatter {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    driver: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    access: Option<AccessMode>,
    #[serde(default)]
    approval_policy: Option<ApprovalPolicy>,
    #[serde(default)]
    output_schema: Option<Value>,
}

/// Validates a role identifier against `[a-zA-Z0-9_-]+` (rejecting path traversal).
pub fn validate_role_name(name: &str) -> Result<(), CoreError> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(CoreError::bad_request(format!(
            "invalid role name '{name}': must match [a-zA-Z0-9_-]+"
        )));
    }
    Ok(())
}

/// Parses raw Markdown bytes into a `RoleDetail` with immutable SHA-256 snapshot digest.
pub fn parse_role_markdown(
    fallback_name: &str,
    source: RoleSource,
    raw: &[u8],
) -> Result<RoleDetail, CoreError> {
    validate_role_name(fallback_name)?;
    if raw.len() > MAX_ROLE_FILE_BYTES {
        return Err(CoreError::bad_request(format!(
            "role '{fallback_name}' exceeds maximum size of {MAX_ROLE_FILE_BYTES} bytes"
        )));
    }
    let text = std::str::from_utf8(raw).map_err(|_| {
        CoreError::bad_request(format!("role '{fallback_name}' is not valid UTF-8"))
    })?;

    let mut hasher = Sha256::new();
    hasher.update(raw);
    let snapshot_sha256 = hex_lower(&hasher.finalize());

    let (frontmatter, body) = split_frontmatter(text)?;
    let fm: RoleFrontmatter = if let Some(yaml) = frontmatter {
        serde_yaml_ng::from_str(yaml)
            .map_err(|e| CoreError::bad_request(format!("invalid role frontmatter: {e}")))?
    } else {
        RoleFrontmatter::default()
    };

    let name = match fm.name {
        Some(n) => {
            validate_role_name(&n)?;
            n
        }
        None => fallback_name.to_owned(),
    };

    Ok(RoleDetail {
        summary: RoleSummary {
            name,
            source,
            driver: fm.driver,
            model: fm.model,
            effort: fm.effort,
            access: fm.access.unwrap_or(AccessMode::ReadOnly),
            approval_policy: fm.approval_policy.unwrap_or(ApprovalPolicy::Interactive),
            snapshot_sha256,
        },
        instructions: body.trim().to_owned(),
        output_schema: fm.output_schema,
    })
}

fn split_frontmatter(text: &str) -> Result<(Option<&str>, &str), CoreError> {
    let normalized = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rest = if let Some(r) = normalized.strip_prefix("---\n") {
        r
    } else if let Some(r) = normalized.strip_prefix("---\r\n") {
        r
    } else {
        return Ok((None, normalized));
    };

    let mut offset = 0usize;
    for line in rest.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed == "---" {
            let yaml = &rest[..offset];
            let body = &rest[offset + line.len()..];
            return Ok((Some(yaml), body));
        }
        offset += line.len();
    }
    Err(CoreError::bad_request(
        "unterminated YAML frontmatter block in role file",
    ))
}

fn read_bounded_file(path: &Path, role_name: &str) -> Result<Vec<u8>, CoreError> {
    let sym_meta = fs::symlink_metadata(path)?;
    if sym_meta.file_type().is_symlink() {
        return Err(CoreError::forbidden_scope(format!(
            "role '{role_name}' must not be a symlink"
        )));
    }
    if !sym_meta.is_file() {
        return Err(CoreError::bad_request(format!(
            "role path '{}' is not a regular file",
            path.display()
        )));
    }
    if sym_meta.len() > MAX_ROLE_FILE_BYTES as u64 {
        return Err(CoreError::bad_request(format!(
            "role '{role_name}' exceeds maximum size of {MAX_ROLE_FILE_BYTES} bytes"
        )));
    }
    Ok(fs::read(path)?)
}

/// Loads one role by name, checking `<workspace>/.dolgorae/roles` before `<home_dir>/roles`.
pub fn load_role(
    home_dir: &Path,
    workspace: Option<&Path>,
    name: &str,
) -> Result<RoleDetail, CoreError> {
    validate_role_name(name)?;
    let filename = format!("{name}.md");

    if let Some(ws) = workspace {
        let ws_path = ws.join(".dolgorae").join("roles").join(&filename);
        if fs::symlink_metadata(&ws_path).is_ok() {
            let raw = read_bounded_file(&ws_path, name)?;
            return parse_role_markdown(name, RoleSource::Workspace, &raw);
        }
    }

    let home_path = home_dir.join("roles").join(&filename);
    if fs::symlink_metadata(&home_path).is_ok() {
        let raw = read_bounded_file(&home_path, name)?;
        return parse_role_markdown(name, RoleSource::UserGlobal, &raw);
    }

    Err(CoreError::not_found(format!("role '{name}' not found")))
}

/// Lists all resolved roles with workspace roles shadowing user-global roles.
pub fn list_roles(
    home_dir: &Path,
    workspace: Option<&Path>,
) -> Result<Vec<RoleSummary>, CoreError> {
    let mut by_name: BTreeMap<String, RoleSummary> = BTreeMap::new();

    // Load user-global first so workspace roles cleanly shadow matching names.
    scan_role_dir(
        &home_dir.join("roles"),
        RoleSource::UserGlobal,
        &mut by_name,
    )?;
    if let Some(ws) = workspace {
        scan_role_dir(
            &ws.join(".dolgorae").join("roles"),
            RoleSource::Workspace,
            &mut by_name,
        )?;
    }

    Ok(by_name.into_values().collect())
}

fn scan_role_dir(
    dir: &Path,
    source: RoleSource,
    out: &mut BTreeMap<String, RoleSummary>,
) -> Result<(), CoreError> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if validate_role_name(stem).is_err() {
            continue;
        }
        let raw = match read_bounded_file(&path, stem) {
            Ok(bytes) => bytes,
            Err(err) => {
                tracing::warn!(path = %path.display(), %err, "skipping unreadable or unsafe role file");
                continue;
            }
        };
        match parse_role_markdown(stem, source, &raw) {
            Ok(detail) => {
                out.insert(detail.summary.name.clone(), detail.summary);
            }
            Err(err) => {
                tracing::warn!(path = %path.display(), %err, "skipping malformed role file");
            }
        }
    }
    Ok(())
}

pub(crate) fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}
