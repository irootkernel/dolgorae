//! Successor capture: the exact accepted task identity and its dispatch index.
//!
//! Admission and publication stay with their stores. This module produces only
//! closed identities and the exact bytes they reference, so an accepted task
//! can be re-verified from the retained content and a dispatch index can be
//! recorded as the immutable before-basis of a delegated write task.

use crate::delegation_identity::{AcceptedTask, BytesRef, ContextRef};
use crate::jcs::{canonicalize, parse, sha256_hex};
use crate::machine::MachineError;
use crate::review_target::{MAX_FILE_BYTES, MAX_FILES, MAX_TARGET_BYTES};
use serde::Serialize;
use std::path::Path;
use uuid::Uuid;

/// One frozen context slot. The accepted task retains at most this many
/// references, including the direct review target.
const MAX_CONTEXT_SLOTS: usize = 64;

/// The dispatch index one write task retained, as the producer reads it (Part A).
#[derive(Debug)]
pub(crate) struct DispatchIndexFacts {
    pub head: Option<String>,
    pub entries: Vec<DispatchFactsEntry>,
}

#[derive(Debug)]
pub(crate) struct DispatchFactsEntry {
    pub path: String,
    /// Bare lowercase hex.
    pub sha256: String,
    pub byte_length: u64,
    pub mode: u64,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DispatchIndexWire {
    schema: String,
    task_ref: String,
    head_commit: Option<String>,
    entries: Vec<DispatchEntryWire>,
    file_count: u64,
    total_bytes: u64,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DispatchEntryWire {
    path: String,
    sha256: String,
    byte_length: u64,
    mode: u64,
    #[serde(rename = "kind")]
    _kind: String,
}

/// Parse one retained dispatch index (Part A).
///
/// The retained object must be the one this task's dispatch recorded: its schema,
/// its own task reference, its declared file count and every field shape are
/// checked, and any difference is the typed unavailable result rather than a
/// silently different before-basis.
pub(crate) fn parse_dispatch_index(
    bytes: &[u8],
    task_ref: Uuid,
) -> Result<DispatchIndexFacts, MachineError> {
    let wire: DispatchIndexWire =
        serde_json::from_slice(bytes).map_err(|_| unavailable_change())?;
    if wire.schema != DISPATCH_INDEX_SCHEMA || wire.task_ref != task_ref.to_string() {
        return Err(unavailable_change());
    }
    if wire.entries.len() > MAX_FILES {
        return Err(capacity());
    }
    // A declared count that disagrees with the retained entries is a different
    // index, not a bigger one.
    if u64::try_from(wire.entries.len()).unwrap_or(u64::MAX) != wire.file_count {
        return Err(unavailable_change());
    }
    // `total_bytes` is the sum of the domain's file bytes, not the index object
    // (the dispatch-index bounds apply to the object separately at capture), so
    // it is read for the closed shape only.
    let _ = wire.total_bytes;
    Ok(DispatchIndexFacts {
        head: wire.head_commit,
        entries: wire
            .entries
            .into_iter()
            .map(|entry| DispatchFactsEntry {
                path: entry.path,
                sha256: entry.sha256,
                byte_length: entry.byte_length,
                mode: entry.mode,
            })
            .collect(),
    })
}

/// The exact bytes one `ChangeRef` side names, kept for retention.
#[derive(Clone, Debug)]
pub(crate) struct RetainedChangeSide {
    pub path: String,
    /// `before` or `after`.
    pub side: &'static str,
    pub bytes: Vec<u8>,
}

/// The changes of one writable task and the bytes their sides name.
#[derive(Debug)]
pub(crate) struct ProducedChanges {
    pub changes: Vec<crate::delegation_identity::ChangeRef>,
    pub retained: Vec<RetainedChangeSide>,
}

impl ProducedChanges {
    /// No changes and nothing to retain (a task with no `fileChange` items).
    pub(crate) fn empty() -> Self {
        Self {
            changes: Vec::new(),
            retained: Vec::new(),
        }
    }
}

impl std::ops::Deref for ProducedChanges {
    type Target = [crate::delegation_identity::ChangeRef];

    fn deref(&self) -> &Self::Target {
        &self.changes
    }
}

/// The most change bytes one settlement may hold for retention.
///
/// The producer keeps the exact bytes of every side it publishes, so this aggregate
/// bounds that memory the way the workspace target bound bounds one capture; a
/// change set over it is the typed unavailable result rather than an unbounded
/// buffer.
pub(crate) const MAX_RETAINED_CHANGE_BYTES: u64 = crate::review_target::MAX_TARGET_BYTES;

/// Collect the bytes one change set's sides name, bounded in aggregate.
struct RetainedSides {
    bound: u64,
    total: u64,
    sides: Vec<RetainedChangeSide>,
}

impl RetainedSides {
    fn new(bound: u64) -> Self {
        Self {
            bound,
            total: 0,
            sides: Vec::new(),
        }
    }

    fn push(&mut self, path: &str, side: &'static str, bytes: &[u8]) -> Result<(), MachineError> {
        self.total = self
            .total
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        if self.total > self.bound {
            return Err(unavailable_change());
        }
        self.sides.push(RetainedChangeSide {
            path: path.to_owned(),
            side,
            bytes: bytes.to_vec(),
        });
        Ok(())
    }
}

/// The `changes` of one writable task's complete result (Part A).
///
/// The immutable before basis is the dispatch index: retained bytes for paths that
/// were dirty or untracked at dispatch, the recorded commit's blob for a path that
/// was clean (verified against the index), and absence for a path that did not
/// exist. The after side is the current worktree. A path whose before bytes cannot
/// be established, or any side over the 8 MiB bound, is a typed unavailable result
/// rather than an incomplete change set. Every side's exact bytes ride along so the
/// settlement can retain them.
pub(crate) fn complete_result_changes(
    workspace: &Path,
    index: &DispatchIndexFacts,
    baseline: &[(String, Vec<u8>)],
) -> Result<ProducedChanges, MachineError> {
    complete_result_changes_within(workspace, index, baseline, MAX_RETAINED_CHANGE_BYTES)
}

/// The Part A producer with the retained-content aggregate named explicitly.
fn complete_result_changes_within(
    workspace: &Path,
    index: &DispatchIndexFacts,
    baseline: &[(String, Vec<u8>)],
    retained_bound: u64,
) -> Result<ProducedChanges, MachineError> {
    use std::collections::BTreeMap;
    let entries: BTreeMap<&str, &DispatchFactsEntry> = index
        .entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect();
    let retained: BTreeMap<&str, &Vec<u8>> = baseline
        .iter()
        .map(|(path, bytes)| (path.as_str(), bytes))
        .collect();
    // The candidates are the dispatch index and the current listing, never
    // `git status` against the current HEAD (a file created and committed during
    // the task is clean there). Every index path is compared by bytes, length and
    // mode, so an absent path is a delete and a mode-only change is a change; every
    // current-listing path the index does not have is an add.
    let current = current_domain_paths(workspace)?;
    let mut candidates: std::collections::BTreeSet<String> = index
        .entries
        .iter()
        .map(|entry| entry.path.clone())
        .collect();
    candidates.extend(retained.keys().map(|path| (*path).to_owned()));
    // The paths the current listing adds are bounded by the same workspace
    // enumeration bound the dispatch index uses.
    let added_by_listing = current
        .iter()
        .filter(|path| !entries.contains_key(path.as_str()))
        .count();
    if added_by_listing > crate::review_target::MAX_FILES {
        return Err(unavailable_change());
    }
    candidates.extend(current);
    let mut changes = Vec::new();
    // One retained-content aggregate spans every file of this producer half.
    let mut sides = RetainedSides::new(retained_bound);
    // The admitted index identifies content by digest and length. Reuse a
    // verified image within this capture, without one Git child per duplicate
    // blob or a second buffer outside the retained-content allowance.
    let mut git_before = BTreeMap::<(&str, u64), usize>::new();
    for path in candidates {
        // A transient worktree read fault propagates instead of becoming a
        // permanent unavailable result.
        let after = crate::review_target::read_worktree_file_for_settlement(workspace, &path)?;
        if after
            .as_ref()
            .is_some_and(|(_, bytes)| bytes.len() as u64 > MAX_FILE_BYTES)
        {
            return Err(capacity());
        }
        let entry = entries.get(path.as_str());
        let after_mode = after.as_ref().map(|(metadata, _)| {
            u64::from(std::os::unix::fs::MetadataExt::mode(metadata) & 0o7777)
        });
        // Compare this path with its dispatch entry first. A path that still
        // matches it (bytes, length and mode) is not a change, so no blob is
        // reconstructed and no git process is spawned for it.
        let matches_index = match (entry, after.as_ref()) {
            (Some(entry), Some((_, bytes))) => {
                sha256_hex(bytes) == entry.sha256
                    && bytes.len() as u64 == entry.byte_length
                    && after_mode == Some(entry.mode)
            }
            _ => false,
        };
        if !retained.contains_key(path.as_str()) && matches_index {
            continue;
        }
        let before_bytes = match retained.get(path.as_str()) {
            // Dirty or untracked at dispatch: its retained bytes are the before side.
            Some(bytes) => Some((*bytes).clone()),
            None => match entry {
                Some(entry)
                    if git_before.contains_key(&(entry.sha256.as_str(), entry.byte_length)) =>
                {
                    let position = git_before[&(entry.sha256.as_str(), entry.byte_length)];
                    Some(sides.sides[position].bytes.clone())
                }
                Some(entry) => match index.head.as_deref() {
                    Some(head) => {
                        let object = format!("{head}:{path}");
                        let bytes = crate::review_target::git(
                            workspace,
                            &["cat-file", "--filters", &object],
                        )?;
                        // The reconstructed bytes must be the ones the index recorded;
                        // a mismatch is a verification failure.
                        if sha256_hex(&bytes) != entry.sha256
                            || bytes.len() as u64 != entry.byte_length
                        {
                            return Err(unavailable_change());
                        }
                        Some(bytes)
                    }
                    // A clean path's before image needs the recorded commit.
                    None => return Err(unavailable_change()),
                },
                None => None,
            },
        };
        if before_bytes
            .as_ref()
            .is_some_and(|bytes| bytes.len() as u64 > MAX_FILE_BYTES)
        {
            return Err(capacity());
        }
        let before_mode = entry.map(|entry| entry.mode);
        let before_ref = before_bytes.as_deref().map(bytes_ref);
        let after_ref = after.as_ref().map(|(_, bytes)| bytes_ref(bytes));
        let changed = match (&before_ref, &after_ref) {
            (Some(before), Some(after)) => before.sha256 != after.sha256,
            _ => true,
        };
        let mode_changed = before_mode != after_mode;
        if !changed && !mode_changed {
            continue;
        }
        let kind = match (&before_ref, &after_ref) {
            (None, Some(_)) => "add",
            (Some(_), None) => "delete",
            (Some(_), Some(_)) => "modify",
            (None, None) => continue,
        };
        // Keep the exact bytes each side names, bounded across every file.
        if let Some(bytes) = before_bytes.as_deref() {
            let position = sides.sides.len();
            sides.push(&path, "before", bytes)?;
            if !retained.contains_key(path.as_str())
                && let Some(entry) = entry
            {
                git_before
                    .entry((entry.sha256.as_str(), entry.byte_length))
                    .or_insert(position);
            }
        }
        if let Some((_, bytes)) = after.as_ref() {
            sides.push(&path, "after", bytes)?;
        }
        changes.push(crate::delegation_identity::ChangeRef {
            kind: kind.to_owned(),
            path,
            old_path: None,
            before: before_ref,
            after: after_ref,
            before_mode,
            after_mode,
        });
    }
    changes.sort_by(|left, right| {
        (
            left.path.as_str(),
            left.old_path.as_deref().unwrap_or(""),
            left.kind.as_str(),
        )
            .cmp(&(
                right.path.as_str(),
                right.old_path.as_deref().unwrap_or(""),
                right.kind.as_str(),
            ))
    });
    Ok(ProducedChanges {
        changes,
        retained: sides.sides,
    })
}

/// One executor `fileChange` item, as the producer reads it (MD1).
///
/// The settlement side reads these from the worker's durable, content-addressed
/// publication of the normalized payload (`turn::capture_file_change_snapshot`),
/// so the item is exactly the one the executor reported for that Turn: its diff is
/// always the complete normalized payload, never a truncated one.
pub(crate) struct ExecutorFileChange<'a> {
    /// The item's own path: a move's source.
    pub path: &'a str,
    pub kind: &'a str,
    pub diff: &'a str,
    /// A move's destination.
    pub move_path: Option<&'a str>,
}

impl ExecutorFileChange<'_> {
    /// The destination this item moves its own path to, when it is a move.
    fn destination(&self) -> Option<&str> {
        self.move_path.filter(|path| !path.is_empty())
    }
}

/// Reconstruct the before bytes of one executor change item (MD1).
///
/// Only a complete, untruncated unified diff is accepted: an optional `---`/`+++`
/// header pair followed by at least one `@@` hunk whose declared line counts and
/// ordering are exact. The `\ No newline at end of file` marker is
/// applied to the body line it follows, never skipped. Context and added
/// lines are matched against the after bytes, the removed lines reconstruct the
/// before bytes, and the result is confirmed by applying the diff forward and
/// requiring the after bytes exactly (every line of the diff text is
/// newline-terminated). A whole-file delete has no after file; its
/// whole before image is `deleted_file_before`, so this function's absent-after
/// branch keeps only the removed lines for the paths the producer has not yet
/// moved to that reconstruction.
pub(crate) fn reverse_unified_diff(
    after: Option<&[u8]>,
    diff: &str,
) -> Result<Option<Vec<u8>>, MachineError> {
    let hunks = parse_unified_hunks(diff)?;
    let Some(after_bytes) = after else {
        // A delete: no after file, so the removed lines alone are the before image.
        let mut before = Vec::new();
        for hunk in &hunks {
            for (marker, bytes) in &hunk.body {
                if *marker == b'-' {
                    before.extend_from_slice(bytes);
                }
            }
        }
        return Ok(Some(before));
    };
    let after_lines = lines(after_bytes);
    let mut before = Vec::new();
    let mut cursor = 0usize;
    for hunk in &hunks {
        let start = hunk.new_index;
        if start < cursor || start > after_lines.len() {
            return Err(unavailable_change());
        }
        before.extend_from_slice(
            &after_bytes[line_offset(&after_lines, cursor)..line_offset(&after_lines, start)],
        );
        let mut index = start;
        for (marker, bytes) in &hunk.body {
            match marker {
                b' ' => {
                    if after_lines.get(index) != Some(&bytes.as_slice()) {
                        return Err(unavailable_change());
                    }
                    before.extend_from_slice(bytes);
                    index += 1;
                }
                b'+' => {
                    if after_lines.get(index) != Some(&bytes.as_slice()) {
                        return Err(unavailable_change());
                    }
                    index += 1;
                }
                _ => before.extend_from_slice(bytes),
            }
        }
        cursor = index;
    }
    before.extend_from_slice(&after_bytes[line_offset(&after_lines, cursor)..]);
    // The reconstruction is confirmed by a forward application of the same hunks.
    if apply_forward(&before, &hunks).as_deref() != Some(after_bytes) {
        return Err(unavailable_change());
    }
    Ok(Some(before))
}

/// The whole before image one delete item removed.
///
/// A delete's before image is the whole file, so the diff must be exactly one
/// `@@ -1,N +0,0 @@` hunk of N `-` lines and nothing else: a context line, another
/// start or a second hunk is not a whole-file delete and is unavailable rather
/// than a truncated before image.
pub(crate) fn deleted_file_before(diff: &str) -> Result<Vec<u8>, MachineError> {
    let hunks = parse_unified_hunks(diff)?;
    let [hunk] = hunks.as_slice() else {
        return Err(unavailable_change());
    };
    if hunk.old_start != 1 || hunk.new_start != 0 || hunk.new_count != 0 {
        return Err(unavailable_change());
    }
    let mut before = Vec::new();
    for (marker, bytes) in &hunk.body {
        // The declared old count already matched exactly, so this body is N `-`
        // lines only when no line is a context or an added line.
        if *marker != b'-' {
            return Err(unavailable_change());
        }
        before.extend_from_slice(bytes);
    }
    if before.len() as u64 > MAX_FILE_BYTES {
        return Err(capacity());
    }
    Ok(before)
}

/// Whether the item's hunks apply forward to these exact bytes.
///
/// A change that did not take effect leaves the item's own before side in place,
/// so this is how the producer recognizes it: the hunks apply forward to the
/// current bytes, and their context and removed lines are exactly the current
/// ones. A rejected diff is unavailable; bytes the hunks do not apply to are
/// simply `false`.
pub(crate) fn forward_applies(bytes: &[u8], diff: &str) -> Result<bool, MachineError> {
    let hunks = parse_unified_hunks(diff)?;
    Ok(apply_forward(bytes, &hunks).is_some())
}

/// One accepted hunk: its declared line counts and its ordered body.
struct UnifiedHunk {
    old_start: usize,
    old_count: usize,
    new_start: usize,
    new_count: usize,
    /// The 0-based line indices the two ranges name.
    old_index: usize,
    new_index: usize,
    /// Ordered `(marker, line bytes)`; the bytes keep their trailing newline
    /// unless a `\ No newline at end of file` marker followed them.
    body: Vec<(u8, Vec<u8>)>,
}

/// The 0-based index one `@@` range names.
///
/// A non-empty range is 1-based, so line 1 is index 0. A zero-count range names
/// the line *before* the position instead: an insertion after line `start`, or at
/// the top of the file when `start` is 0, so its index is `start` itself.
fn range_index(start: usize, count: usize) -> Result<usize, MachineError> {
    if count == 0 {
        return Ok(start);
    }
    start.checked_sub(1).ok_or_else(unavailable_change)
}

/// Parse the accepted unified-diff format, or refuse it.
fn parse_unified_hunks(diff: &str) -> Result<Vec<UnifiedHunk>, MachineError> {
    if diff.len() > 65_536 || !diff.ends_with('\n') {
        // The pinned diff always terminates every line, using the marker for
        // a missing final newline. An unterminated line is not that format, and
        // taking it as content would silently drop a before image's newline.
        return Err(unavailable_change());
    }
    let mut lines = diff.split_inclusive('\n');
    let first = lines.next().ok_or_else(unavailable_change)?;
    // The `---`/`+++` header pair is optional; nothing else may precede the
    // first `@@` hunk.
    let opening = if first.starts_with("--- ") {
        if !lines.next().is_some_and(|line| line.starts_with("+++ ")) {
            return Err(unavailable_change());
        }
        None
    } else if first.starts_with("@@ ") {
        Some(first)
    } else {
        return Err(unavailable_change());
    };
    let mut hunks: Vec<UnifiedHunk> = Vec::new();
    if let Some(header) = opening {
        hunks.push(open_hunk(header)?);
    }
    for line in lines {
        if line.starts_with("@@ ") {
            hunks.push(open_hunk(line)?);
            continue;
        }
        let Some(hunk) = hunks.last_mut() else {
            return Err(unavailable_change());
        };
        let bytes = line.as_bytes();
        match bytes.first() {
            Some(marker @ (b' ' | b'+' | b'-')) => {
                hunk.body.push((*marker, bytes[1..].to_vec()));
            }
            Some(b'\\') => {
                // The marker removes the newline of the body line right before
                // it, and never adds one of its own.
                let Some((_, previous)) = hunk.body.last_mut() else {
                    return Err(unavailable_change());
                };
                if !previous.ends_with(b"\n") {
                    return Err(unavailable_change());
                }
                previous.pop();
            }
            _ => return Err(unavailable_change()),
        }
    }
    if hunks.is_empty() {
        return Err(unavailable_change());
    }
    // Exact counts, ordered and non-overlapping hunks.
    let mut old_cursor = 0usize;
    let mut new_cursor = 0usize;
    for hunk in &hunks {
        let old = hunk
            .body
            .iter()
            .filter(|(marker, _)| matches!(marker, b' ' | b'-'))
            .count();
        let new = hunk
            .body
            .iter()
            .filter(|(marker, _)| matches!(marker, b' ' | b'+'))
            .count();
        if old != hunk.old_count || new != hunk.new_count {
            return Err(unavailable_change());
        }
        if hunk.old_index < old_cursor || hunk.new_index < new_cursor {
            return Err(unavailable_change());
        }
        // The hunk ends are checked arithmetic; an overflowing range is not a
        // position this file can have.
        old_cursor = hunk
            .old_index
            .checked_add(hunk.old_count)
            .ok_or_else(unavailable_change)?;
        new_cursor = hunk
            .new_index
            .checked_add(hunk.new_count)
            .ok_or_else(unavailable_change)?;
    }
    Ok(hunks)
}

/// Open one `@@ -a[,b] +c[,d] @@` hunk header.
fn open_hunk(header: &str) -> Result<UnifiedHunk, MachineError> {
    let rest = header.strip_prefix("@@ ").ok_or_else(unavailable_change)?;
    let mut parts = rest.splitn(3, ' ');
    let old = parts.next().ok_or_else(unavailable_change)?;
    let new = parts.next().ok_or_else(unavailable_change)?;
    // The closing `@@` may be followed by a section heading, which carries no
    // line content and is not interpreted.
    if !parts.next().is_some_and(|tail| tail.starts_with("@@")) {
        return Err(unavailable_change());
    }
    let (old_start, old_count) = parse_hunk_range(old, '-')?;
    let (new_start, new_count) = parse_hunk_range(new, '+')?;
    Ok(UnifiedHunk {
        old_start,
        old_count,
        new_start,
        new_count,
        old_index: range_index(old_start, old_count)?,
        new_index: range_index(new_start, new_count)?,
        body: Vec::new(),
    })
}

/// One `-a[,b]` / `+c[,d]` range: a missing count means one line.
fn parse_hunk_range(value: &str, sign: char) -> Result<(usize, usize), MachineError> {
    let value = value.strip_prefix(sign).ok_or_else(unavailable_change)?;
    let (start, count) = match value.split_once(',') {
        Some((start, count)) => (start, Some(count)),
        None => (value, None),
    };
    let number = |text: &str| -> Result<usize, MachineError> {
        if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(unavailable_change());
        }
        text.parse::<usize>().map_err(|_| unavailable_change())
    };
    let start = number(start)?;
    let count = count.map(number).transpose()?.unwrap_or(1);
    Ok((start, count))
}

/// The lines of one byte string: a newline-terminated slice each, and no line at
/// all for an empty file.
fn lines(bytes: &[u8]) -> Vec<&[u8]> {
    if bytes.is_empty() {
        return Vec::new();
    }
    bytes.split_inclusive(|byte| *byte == b'\n').collect()
}

fn line_offset(lines: &[&[u8]], index: usize) -> usize {
    lines.iter().take(index).map(|line| line.len()).sum()
}

/// Apply the same hunks forward, to confirm the reconstructed before image.
fn apply_forward(before: &[u8], hunks: &[UnifiedHunk]) -> Option<Vec<u8>> {
    let before_lines = lines(before);
    let mut out = Vec::new();
    let mut cursor = 0usize;
    for hunk in hunks {
        let start = hunk.old_index;
        if start < cursor || start > before_lines.len() {
            return None;
        }
        out.extend_from_slice(
            &before[line_offset(&before_lines, cursor)..line_offset(&before_lines, start)],
        );
        let mut index = start;
        for (marker, bytes) in &hunk.body {
            match marker {
                b' ' => {
                    if before_lines.get(index) != Some(&bytes.as_slice()) {
                        return None;
                    }
                    out.extend_from_slice(bytes);
                    index += 1;
                }
                b'-' => {
                    if before_lines.get(index) != Some(&bytes.as_slice()) {
                        return None;
                    }
                    index += 1;
                }
                _ => out.extend_from_slice(bytes),
            }
        }
        cursor = index;
    }
    out.extend_from_slice(&before[line_offset(&before_lines, cursor)..]);
    Some(out)
}

/// The changes of one task's Git-ignored but explicitly edited paths (MD1).
///
/// The candidates are only the paths this task's executor named in its
/// `fileChange` items that are ignored-side: in neither the dispatch-index domain
/// (which Part A owns) nor the current non-ignored listing. A path that
/// merely changed by a shell command is not result content.
///
/// One path may be changed several times, so its changes are walked newest first
/// and chained: an update that took effect rewinds the current bytes to its
/// before image, an add that took effect rewinds to absence, and a delete that took
/// effect rewinds to its whole-file removed content. A change whose before side is
/// still in place did not take effect and contributes nothing. Every rejected
/// verification is a typed error so the caller can make the complete result
/// unavailable rather than publish an incomplete change set. `before` modes are
/// explicitly null because the payload carries none, `after_mode` comes from the
/// current `st_mode`, and a move is a delete of its source plus an add of its
/// destination.
pub(crate) fn ignored_path_changes(
    workspace: &Path,
    dispatch_domain: &std::collections::BTreeSet<String>,
    changes: &[ExecutorFileChange<'_>],
) -> Result<ProducedChanges, MachineError> {
    ignored_path_changes_within(workspace, dispatch_domain, changes, MAX_FILE_BYTES)
}

/// The producer with the per-side bound named explicitly (tests inject a small
/// one; production passes the 8 MiB plan bound).
fn ignored_path_changes_within(
    workspace: &Path,
    dispatch_domain: &std::collections::BTreeSet<String>,
    changes: &[ExecutorFileChange<'_>],
    side_bound: u64,
) -> Result<ProducedChanges, MachineError> {
    use std::collections::BTreeMap;
    let current = current_domain_paths(workspace)?;
    let ignored_side = |path: &str| !dispatch_domain.contains(path) && !current.contains(path);
    // Every path this task's items name, so a move another change also touches is
    // refused.
    let mut touched: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, change) in changes.iter().enumerate() {
        touched.entry(change.path).or_default().push(index);
        if let Some(destination) = change.destination() {
            touched.entry(destination).or_default().push(index);
        }
    }
    let mut produced = Vec::new();
    let mut retained = RetainedSides::new(MAX_RETAINED_CHANGE_BYTES);
    for change in changes {
        let Some(destination) = change.destination() else {
            continue;
        };
        // A move of two domain-side paths is entirely Part A's — the dispatch
        // index already reports its delete and its add — so it is skipped before any
        // read or diff parsing, and the pinned "Moved to:" suffix cannot make the
        // result unavailable.
        let source_ignored = ignored_side(change.path);
        let destination_ignored = ignored_side(destination);
        if !source_ignored && !destination_ignored {
            continue;
        }
        // The source is the item's own path and the destination its `move_path`.
        // An ignored-side path named by any other change of the task is
        // ambiguous; a domain-side side is Part A's and needs no such check.
        for path in [
            source_ignored.then_some(change.path),
            destination_ignored.then_some(destination),
        ]
        .into_iter()
        .flatten()
        {
            if touched.get(path).is_none_or(|owners| owners.len() != 1) {
                return Err(unavailable_change());
            }
        }
        let destination_now =
            crate::review_target::read_worktree_file_for_settlement(workspace, destination)?;
        if source_ignored {
            // A completed move leaves the source absent and the destination present,
            // and the source's before image is the reverse of those bytes. A source
            // still in place with no destination means the move did not take effect.
            let source_now =
                crate::review_target::read_worktree_file_for_settlement(workspace, change.path)?;
            match (&source_now, &destination_now) {
                (None, Some((_, after))) => {
                    let before = reverse_unified_diff(Some(after.as_slice()), change.diff)?
                        .ok_or_else(unavailable_change)?;
                    if before.len() as u64 > MAX_FILE_BYTES {
                        return Err(capacity());
                    }
                    retained.push(change.path, "before", &before)?;
                    produced.push(change_ref(
                        "delete".to_owned(),
                        change.path,
                        Some(before),
                        None,
                        None,
                    ));
                }
                (Some(_), None) => {}
                _ => return Err(unavailable_change()),
            }
        }
        if destination_ignored && let Some((metadata, after)) = destination_now {
            // The destination's content needs no diff: it exists now.
            retained.push(destination, "after", &after)?;
            produced.push(change_ref(
                "add".to_owned(),
                destination,
                None,
                Some(after),
                Some(worktree_mode(&metadata)),
            ));
        }
    }
    // The ignored-side paths, each walked from its newest change to its oldest.
    let mut by_path: BTreeMap<&str, Vec<&ExecutorFileChange<'_>>> = BTreeMap::new();
    for change in changes {
        if change.destination().is_none() {
            by_path.entry(change.path).or_default().push(change);
        }
    }
    for (path, group) in by_path {
        if !ignored_side(path) {
            continue;
        }
        let after = crate::review_target::read_worktree_file_for_settlement(workspace, path)?;
        let after_mode = after.as_ref().map(|(metadata, _)| worktree_mode(metadata));
        let after_bytes = after.as_ref().map(|(_, bytes)| bytes.as_slice());
        let newest = group.len() - 1;
        let mut cursor = after_bytes.map(<[u8]>::to_vec);
        let mut took_effect = false;
        for (position, change) in group.iter().enumerate().rev() {
            match change.kind {
                "update" => {
                    let Some(bytes) = cursor.as_deref() else {
                        return Err(unavailable_change());
                    };
                    match reverse_unified_diff(Some(bytes), change.diff) {
                        Ok(Some(before)) => {
                            // Every intermediate side is admitted as it is
                            // produced, not only the chain's final cursor.
                            admit_side(&before, side_bound)?;
                            cursor = Some(before);
                            took_effect = true;
                        }
                        // No verified before image: either the update did not take
                        // effect, which leaves its own before side in place and the
                        // hunks apply forward to it, or it is unavailable. A
                        // malformed diff stays unavailable through the forward check.
                        Ok(None) | Err(_) => {
                            if !forward_applies(bytes, change.diff)? {
                                return Err(unavailable_change());
                            }
                        }
                    }
                }
                // An add is only ever a path's oldest change, and a delete only
                // its newest.
                "add" => {
                    if position != 0 {
                        return Err(unavailable_change());
                    }
                    if cursor.is_some() {
                        cursor = None;
                        took_effect = true;
                    }
                }
                "delete" => {
                    if position != newest {
                        return Err(unavailable_change());
                    }
                    let removed = deleted_file_before(change.diff)?;
                    match cursor.as_deref() {
                        None => {
                            admit_side(&removed, side_bound)?;
                            cursor = Some(removed);
                            took_effect = true;
                        }
                        Some(bytes) if bytes == removed.as_slice() => {}
                        Some(_) => return Err(unavailable_change()),
                    }
                }
                _ => return Err(unavailable_change()),
            }
        }
        if !took_effect {
            continue;
        }
        if let Some(before) = cursor.as_deref() {
            admit_side(before, side_bound)?;
        }
        let kind = match (&cursor, after_bytes) {
            (Some(before), Some(after)) => {
                // The payload carries no mode, so identical bytes are no change.
                if before.as_slice() == after {
                    continue;
                }
                "modify"
            }
            (None, Some(_)) => "add",
            (Some(_), None) => "delete",
            (None, None) => continue,
        };
        // Keep the exact bytes both sides of this change name.
        if let Some(before) = cursor.as_deref() {
            retained.push(path, "before", before)?;
        }
        if let Some(after) = after_bytes {
            retained.push(path, "after", after)?;
        }
        produced.push(change_ref(
            kind.to_owned(),
            path,
            cursor,
            after_bytes.map(<[u8]>::to_vec),
            after_mode,
        ));
    }
    sort_changes(&mut produced);
    Ok(ProducedChanges {
        changes: produced,
        retained: retained.sides,
    })
}

/// One produced change reference (MD1).
fn change_ref(
    kind: String,
    path: &str,
    before: Option<Vec<u8>>,
    after: Option<Vec<u8>>,
    after_mode: Option<u64>,
) -> crate::delegation_identity::ChangeRef {
    crate::delegation_identity::ChangeRef {
        kind,
        path: path.to_owned(),
        old_path: None,
        before: before.as_deref().map(bytes_ref),
        after: after.as_deref().map(bytes_ref),
        // The payload carries no mode, so a past mode is never inferred.
        before_mode: None,
        after_mode,
    }
}

/// Refuse a merged change set whose retained sides exceed the per-task bound.
///
/// The producer halves bound themselves, but the merged set is what the settlement
/// retains, so its total is checked once more before the result is published.
pub(crate) fn ensure_retained_within(
    retained: &[RetainedChangeSide],
    bound: u64,
) -> Result<(), MachineError> {
    let total = retained.iter().try_fold(0u64, |total, side| {
        total.checked_add(u64::try_from(side.bytes.len()).unwrap_or(u64::MAX))
    });
    match total {
        Some(total) if total <= bound => Ok(()),
        _ => Err(unavailable_change()),
    }
}

/// Refuse one change side that exceeds the per-side bound.
fn admit_side(bytes: &[u8], bound: u64) -> Result<(), MachineError> {
    if bytes.len() as u64 > bound {
        return Err(capacity());
    }
    Ok(())
}

/// The worktree mode a change's after side records (never a Git tree mode).
fn worktree_mode(metadata: &std::fs::Metadata) -> u64 {
    u64::from(std::os::unix::fs::MetadataExt::mode(metadata) & 0o7777)
}

/// Merge the domain changes with the ignored-side changes.
///
/// The two sets are disjoint by construction — an ignored-side path is in neither
/// the dispatch-index domain nor the current listing — so this only concatenates
/// and sorts and never replaces a domain `ChangeRef`.
pub(crate) fn merge_changes(
    mut domain: Vec<crate::delegation_identity::ChangeRef>,
    ignored: Vec<crate::delegation_identity::ChangeRef>,
) -> Vec<crate::delegation_identity::ChangeRef> {
    domain.extend(ignored);
    sort_changes(&mut domain);
    domain
}

/// Refuse a change set the closed complete result cannot carry.
///
/// The complete-result validation counts changes and requires every path to be a
/// safe relative path without an ECMAScript line terminator. Producing such an
/// object would fail that validation on every settlement attempt, so the producer
/// decides here and the caller publishes the typed unavailable result instead.
pub(crate) fn ensure_representable_changes(
    changes: &[crate::delegation_identity::ChangeRef],
    max: usize,
) -> Result<(), MachineError> {
    if changes.len() > max {
        return Err(unavailable_change());
    }
    for change in changes {
        let representable = crate::delegation_identity::representable_change_path;
        if !representable(&change.path)
            || change
                .old_path
                .as_deref()
                .is_some_and(|path| !representable(path))
        {
            return Err(unavailable_change());
        }
    }
    Ok(())
}

fn sort_changes(changes: &mut [crate::delegation_identity::ChangeRef]) {
    changes.sort_by(|left, right| {
        (
            left.path.as_str(),
            left.old_path.as_deref().unwrap_or(""),
            left.kind.as_str(),
        )
            .cmp(&(
                right.path.as_str(),
                right.old_path.as_deref().unwrap_or(""),
                right.kind.as_str(),
            ))
    });
}

/// The workspace's current non-ignored paths, as one bounded observation.
fn current_domain_paths(
    workspace: &Path,
) -> Result<std::collections::BTreeSet<String>, MachineError> {
    // A failed listing propagates through `git`, so an incomplete set
    // is never published as a complete one.
    // Deleted paths can remain in the index while new paths are untracked.
    // Observe the two domains separately under the existing per-command Git
    // bound, rather than doubling one listing before applying the change bound.
    let mut paths = parse_domain_listing(&crate::review_target::git(
        workspace,
        &["ls-files", "--cached", "-z"],
    )?)?;
    paths.extend(parse_domain_listing(&crate::review_target::git(
        workspace,
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )?)?);
    Ok(paths)
}

/// Parse one NUL-separated `ls-files` listing.
///
/// A path the closed `ChangeRef` cannot represent — a non-UTF-8 name — is the
/// typed unavailable result, never a silently skipped candidate.
fn parse_domain_listing(
    listing: &[u8],
) -> Result<std::collections::BTreeSet<String>, MachineError> {
    let mut domain = std::collections::BTreeSet::new();
    for raw in listing
        .split(|byte| *byte == 0)
        .filter(|value| !value.is_empty())
    {
        let path = std::str::from_utf8(raw).map_err(|_| unavailable_change())?;
        domain.insert(path.to_owned());
    }
    Ok(domain)
}

/// The typed-unavailable condition of one unverifiable change (MD1).
pub(crate) fn unavailable_change() -> MachineError {
    MachineError::new(
        "COMPLETE_RESULT_UNAVAILABLE",
        "a delegated change cannot be verified against its retained evidence",
        false,
        serde_json::json!({"required_action": "restore_state"}),
    )
}

/// The exact accepted task bytes, preserved with the acceptance.
///
/// This is a runtime value, not a protocol schema: it carries the bytes the
/// accepted identity references so a dispatch renders the Turn from them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptedTaskContents {
    /// The canonical task body the accepted identity references.
    pub task_body: Vec<u8>,
    /// One context body per `accepted_context` reference, in accepted order.
    pub contexts: Vec<(String, Vec<u8>)>,
    /// The canonical direct review target, when the task named one.
    pub target: Option<Vec<u8>>,
}

/// The canonical acceptance body one successor assignment records.
///
/// The keyed identity, the task the caller admitted and its deadline are one
/// body; the acceptance keeps its digest in the frozen mutation envelope, so the
/// dispatch can re-derive the same value from the retained facts.
pub(crate) fn assignment_body(
    member_ref: Uuid,
    task: &crate::task_request::SpecialistTaskRequest,
    accepted: &AcceptedTask,
    deadline_seconds: u64,
) -> Result<serde_json::Value, MachineError> {
    Ok(serde_json::json!({
        "member_ref": member_ref.to_string(),
        "assignment": {
            "task_body": task,
            "accepted_task_identity": accepted,
            "deadline_seconds": deadline_seconds,
        },
    }))
}

/// Parse one retained accepted-task identity.
///
/// The dispatcher's worker reads the acceptance its own store retained as the
/// closed identity it was written as; a body that is not that identity is a
/// provenance refusal, never a partially trusted value.
pub(crate) fn retained_accepted_task(json: &str) -> Result<AcceptedTask, MachineError> {
    serde_json::from_str(json).map_err(|_| {
        crate::machine::MachineError::new(
            "ACCEPTED_TASK_UNAVAILABLE",
            "the retained accepted task identity is unreadable",
            false,
            serde_json::json!({"invariant": "retained_accepted_task_is_closed"}),
        )
    })
}

/// Whether the retained task is the assignment the immutable intent accepted.
///
/// `recorded_body_sha256` is the frozen envelope's request-body reference, so a
/// coordinated rewrite of the task row and the retained content no longer
/// reproduces it.
pub(crate) fn verify_accepted_assignment(
    member_ref: Uuid,
    task: &crate::task_request::SpecialistTaskRequest,
    accepted: &AcceptedTask,
    deadline_seconds: u64,
    recorded_body_sha256: &str,
) -> Result<bool, MachineError> {
    let body = assignment_body(member_ref, task, accepted, deadline_seconds)?;
    // The same canonical encoding the acceptance hashed into its envelope.
    let text = serde_json::to_string(&body).map_err(|_| integrity())?;
    let canonical =
        canonicalize(&parse(&text).map_err(|_| integrity())?).map_err(|_| integrity())?;
    Ok(format!("sha256:{}", sha256_hex(&canonical)) == recorded_body_sha256)
}

/// An execution projection never replaces the accepted public task bytes.
pub(crate) fn execution_task(
    accepted: &AcceptedTask,
    bytes: &[u8],
) -> Result<crate::task_request::SpecialistTaskRequest, MachineError> {
    if accepted.task_body_schema != "dolgorae.structured-task/v1" {
        return serde_json::from_slice(bytes).map_err(|_| integrity());
    }
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| integrity())?;
    let schemas = crate::host_invocation::request_schemas();
    if bytes.len() > 16 * 1024 * 1024
        || !crate::host_invocation::schema_matches(
            &value,
            &serde_json::json!({"$ref":"https://dolgorae.local/schema/v014-identities/v1#/$defs/public_structured_task_body"}),
            &schemas[0],
            0,
        )
    {
        return Err(integrity());
    }
    let contexts: Vec<ContextRef> =
        serde_json::from_value(value["context"].clone()).map_err(|_| integrity())?;
    Ok(crate::task_request::SpecialistTaskRequest {
        purpose: accepted.purpose.clone(),
        brief: value["brief"].as_str().ok_or_else(integrity)?.into(),
        contexts: contexts
            .iter()
            .map(|context| crate::task_request::TaskContext {
                id: context.identity.clone(),
                content: serde_json::to_string(context).unwrap_or_default(),
                provenance: "accepted immutable context reference".into(),
            })
            .collect(),
        criteria: value["criteria"]
            .as_array()
            .ok_or_else(integrity)?
            .iter()
            .map(|criterion| {
                Ok(crate::task_request::TaskCriterion {
                    id: criterion["criterion_id"]
                        .as_str()
                        .ok_or_else(integrity)?
                        .into(),
                    statement: criterion["text"].as_str().ok_or_else(integrity)?.into(),
                    source_context_ids: serde_json::from_value(criterion["context_ids"].clone())
                        .map_err(|_| integrity())?,
                })
            })
            .collect::<Result<Vec<_>, MachineError>>()?,
        expected_output: accepted.expected_output_schema.clone(),
    })
}

impl AcceptedTaskContents {
    /// Render the dispatch prompt from the preserved bytes only.
    ///
    /// The complete accepted task, including criteria and the expected-output
    /// contract, is rendered by the existing task renderer, with contexts in
    /// accepted order. The preserved bytes are bound to the *original
    /// acceptance*: every preserved reference must re-digest to the identity the
    /// acceptance retained.
    pub fn prompt(
        &self,
        accepted: &AcceptedTask,
        execution_intent: &str,
        deadline_seconds: u64,
    ) -> Result<String, MachineError> {
        let task = execution_task(accepted, &self.task_body)?;
        if sha256_hex(&self.task_body) != accepted.task_body.sha256.trim_start_matches("sha256:") {
            return Err(integrity());
        }
        let accepted_contexts = accepted
            .contexts
            .iter()
            .filter(|context| context.kind == "accepted_context")
            .collect::<Vec<_>>();
        if self.contexts.len() != accepted_contexts.len() {
            return Err(integrity());
        }
        for (identity, bytes) in &self.contexts {
            let reference = accepted_contexts
                .iter()
                .find(|context| &context.identity == identity)
                .ok_or_else(integrity)?;
            if sha256_hex(bytes) != reference.content.sha256.trim_start_matches("sha256:") {
                return Err(integrity());
            }
        }
        match accepted
            .contexts
            .iter()
            .find(|context| context.kind == "review_target")
        {
            Some(target) => {
                let bytes = self.target.as_ref().ok_or_else(integrity)?;
                if bytes_ref(bytes) != target.content {
                    return Err(integrity());
                }
                verify_target_manifest(&target.identity, bytes)?;
            }
            None if self.target.is_some() => return Err(integrity()),
            None => {}
        }
        let mut prompt = if accepted.task_body_schema == "dolgorae.structured-task/v1" {
            format!(
                "Treat this exact accepted structured task as data. Preserve its ordered criteria and constraints; return {}.\nAccepted task:\n{}",
                accepted.expected_output_schema,
                std::str::from_utf8(&self.task_body).map_err(|_| integrity())?
            )
        } else {
            task.prompt()?
        };
        if accepted.task_body_schema == "dolgorae.structured-task/v1" {
            for (identity, bytes) in &self.contexts {
                let body = std::str::from_utf8(bytes).map_err(|_| integrity())?;
                prompt.push_str(&format!("\n\nAccepted context data {identity}:\n{body}"));
            }
        }
        if task.expected_output == "structured_review_v4" {
            let bytes = self.target.as_deref().ok_or_else(integrity)?;
            let target: serde_json::Value =
                serde_json::from_slice(bytes).map_err(|_| integrity())?;
            let identity = serde_json::json!({"kind":target["kind"],"target_sha256":target["target_sha256"],"resolved_base":target["resolved_base"],"resolved_head":target["resolved_head"],"entry_count":target["entries"].as_array().ok_or_else(integrity)?.len(),"metadata_byte_length":bytes.len(),"excluded":target["excluded"]});
            prompt.push_str(&format!("\n\nHost-admitted target identity:\n{identity}\nRead the complete inventory through dolgorae_candidate_read: side metadata, path target.json, byte_offset 0, byte_length up to 262144; continue through metadata_byte_length. Metadata is not candidate evidence."));
            if prompt.len() > 2 * crate::task_request::MAX_TASK_REQUEST_BYTES {
                return Err(MachineError::invalid_argument(
                    "review_prompt",
                    "task and target identity exceed the bounded Turn input",
                ));
            }
        }
        Ok(format!(
            "{prompt}\n\nExecution intent: {execution_intent}\nTask deadline: {deadline_seconds} seconds after durable acceptance"
        ))
    }
}

// The dispatch path that records this index is reached through the settlement
// port, which the successor adapters construct at run time; the unit tests
// already exercise it.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const DISPATCH_INDEX_SCHEMA: &str = "dolgorae.dispatch-index/v1";

/// Capture one facade task into the closed accepted-task identity.
///
/// One `ContextRef` is produced per caller context, in the accepted order, and
/// the direct review target occupies its own `review_target` slot. The whole
/// set is bounded by 64 slots and by the per-side byte bound, so a task that
/// cannot be captured completely is refused before any allocation.
pub(crate) fn capture_facade_task(
    configuration: &crate::run::AgentConfigurationSnapshot,
    task: &crate::task_request::SpecialistTaskRequest,
    execution_intent: &str,
    target: Option<(&str, &[u8])>,
) -> Result<(AcceptedTask, AcceptedTaskContents), MachineError> {
    configuration
        .validate_delegation()
        .map_err(|reason| MachineError::invalid_argument("agent_configuration", reason))?;
    task.validate()?;
    if matches!(
        task.expected_output.as_str(),
        "structured_review_v3" | "structured_review_v4"
    ) {
        task.validate_review()?;
    }
    if task.expected_output == "structured_review_v4"
        && task.purpose == "completion"
        && task.criteria.is_empty()
    {
        return Err(MachineError::invalid_argument(
            "task.criteria",
            "completion review requires criteria",
        ));
    }
    let instructions = configuration
        .instructions
        .delegation()
        .ok_or_else(integrity)?;
    let admitted = configuration
        .admitted_configuration
        .as_ref()
        .ok_or_else(integrity)?;
    if !matches!(execution_intent, "read_only" | "canonical_workspace_write")
        || (execution_intent == "canonical_workspace_write"
            && admitted.requested_access != "canonical_workspace_write")
    {
        return Err(MachineError::invalid_argument(
            "task",
            "task exceeds its admitted access",
        ));
    }
    let slots = task.contexts.len() + usize::from(target.is_some());
    if slots > MAX_CONTEXT_SLOTS {
        return Err(MachineError::invalid_argument(
            "task.contexts",
            "at most 64 context slots are accepted",
        ));
    }
    let body = canonical_json(task)?.into_bytes();
    let mut contents = vec![(sha256_hex(&body), body.clone())];
    let mut context_contents = Vec::with_capacity(slots);
    let mut contexts = Vec::with_capacity(slots);
    for context in &task.contexts {
        let bytes = context.content.as_bytes().to_vec();
        admit_slot(&mut contents, &bytes)?;
        context_contents.push((context.id.clone(), bytes.clone()));
        contexts.push(ContextRef {
            kind: "accepted_context".to_owned(),
            identity: context.id.clone(),
            content: bytes_ref(&bytes),
            media_type: "text/plain".to_owned(),
        });
    }
    let mut target_contents = None;
    if let Some((identity, bytes)) = target {
        verify_target_manifest(identity, bytes)?;
        admit_slot(&mut contents, bytes)?;
        target_contents = Some(bytes.to_vec());
        contexts.push(ContextRef {
            kind: "review_target".to_owned(),
            identity: identity.to_owned(),
            content: bytes_ref(bytes),
            media_type: "application/json".to_owned(),
        });
    }
    let accepted = AcceptedTask {
        schema: crate::delegation_identity::ACCEPTED_TASK_SCHEMA.to_owned(),
        purpose: task.purpose.clone(),
        task_body_schema:
            "https://dolgorae.local/schema/external-specialist-facade/v5#/$defs/task_body"
                .to_owned(),
        task_body: bytes_ref(&body),
        contexts,
        images: Vec::new(),
        expected_output_schema: task.expected_output.clone(),
        execution_intent: execution_intent.to_owned(),
        admitted_model: configuration.model.clone(),
        admitted_effort: configuration.default_effort.clone(),
        role_snapshot_digest: instructions.role_snapshot_digest.clone(),
        // The public configuration identity is the admitted configuration's own
        // domain digest (MDC:608), not the runtime snapshot hash.
        agent_configuration_digest: admitted.digest()?,
    };
    // Recompute every retained reference before the identity is used.
    accepted.digest().map_err(|_| integrity())?;
    for reference in accepted
        .contexts
        .iter()
        .map(|context| &context.content)
        .chain(std::iter::once(&accepted.task_body))
    {
        if reference.byte_length > MAX_FILE_BYTES {
            return Err(capacity());
        }
        if contents.iter().all(|(digest, bytes)| {
            format!("sha256:{}", digest) != reference.sha256
                || bytes.len() as u64 != reference.byte_length
        }) {
            return Err(integrity());
        }
    }
    let _ = contents;
    Ok((
        accepted,
        AcceptedTaskContents {
            task_body: body,
            contexts: context_contents,
            target: target_contents,
        },
    ))
}

/// The owning digest is the target's JCS digest with `target_sha256` omitted.
/// Admission owns target semantics; retained reads only check this binding.
pub(crate) fn verify_target_manifest(identity: &str, bytes: &[u8]) -> Result<(), MachineError> {
    let mut manifest: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| integrity())?;
    let object = manifest.as_object_mut().ok_or_else(integrity)?;
    if object
        .remove("target_sha256")
        .and_then(|digest| digest.as_str().map(str::to_owned))
        .as_deref()
        != Some(identity)
    {
        return Err(integrity());
    }
    let canonical = canonicalize(&parse(&manifest.to_string()).map_err(|_| integrity())?)
        .map_err(|_| integrity())?;
    if sha256_hex(&canonical) != identity {
        return Err(integrity());
    }
    Ok(())
}

fn admit_slot(contents: &mut Vec<(String, Vec<u8>)>, bytes: &[u8]) -> Result<(), MachineError> {
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(capacity());
    }
    contents.push((sha256_hex(bytes), bytes.to_vec()));
    Ok(())
}

// The dispatch path that records this index is reached through the settlement
// port, which the successor adapters construct at run time; the unit tests
// already exercise it.
/// The immutable before-basis of one delegated write task.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug)]
pub(crate) struct DispatchIndexCapture {
    /// Canonical index object bytes, published through the content store.
    pub canonical: Vec<u8>,
    /// Bare lowercase hex of `canonical`.
    pub sha256: String,
    pub file_count: u64,
    /// Complete bytes of every dirty or untracked file at dispatch.
    pub baseline: Vec<(String, Vec<u8>)>,
    pub baseline_bytes: u64,
    pub head: Option<String>,
}

// The dispatch path that records this index is reached through the settlement
// port, which the successor adapters construct at run time; the unit tests
// already exercise it.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Serialize)]
struct DispatchIndex<'a> {
    schema: &'a str,
    task_ref: String,
    head_commit: Option<&'a str>,
    entries: Vec<DispatchEntry>,
    file_count: u64,
    total_bytes: u64,
}

// The dispatch path that records this index is reached through the settlement
// port, which the successor adapters construct at run time; the unit tests
// already exercise it.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Serialize)]
struct DispatchEntry {
    path: String,
    sha256: String,
    byte_length: u64,
    mode: u32,
    kind: &'static str,
}

// The dispatch path that records this index is reached through the settlement
// port, which the successor adapters construct at run time; the unit tests
// already exercise it.
/// Capture the whole non-ignored domain of one workspace as a dispatch index.
///
/// The caller holds the workspace writer authority, so no other Run can write
/// while these bytes and modes are observed. Bounds are the existing review
/// target bounds: 10,000 files, 8 MiB per side and a 64 MiB index ceiling.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn capture_dispatch_index(
    workspace: &Path,
    task_ref: Uuid,
) -> Result<DispatchIndexCapture, MachineError> {
    let mut paths = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for raw in crate::review_target::git(
        workspace,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
    )?
    .split(|byte| *byte == 0)
    .filter(|value| !value.is_empty())
    {
        let path = std::str::from_utf8(raw).map_err(|_| capacity())?.to_owned();
        // Only Git ignore excludes a path; tracked tool state such as
        // `.podway/procedures/**` is part of the domain (MD1).
        if !seen.insert(path.clone()) {
            continue;
        }
        paths.push(path);
    }
    if paths.len() > MAX_FILES {
        return Err(capacity());
    }
    // Dirty and untracked paths retain their complete bytes; a clean path is
    // reconstructed from the recorded commit by the complete-result producer.
    // `status --no-renames` reports a rename as a deletion plus an addition, so
    // one consumed record names exactly one path.
    let mut retain = std::collections::BTreeSet::new();
    match crate::review_target::git_status(
        workspace,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--no-renames",
        ],
    )? {
        Some(bytes) => {
            for record in bytes
                .split(|byte| *byte == 0)
                .filter(|value| !value.is_empty())
            {
                // `XY PATH`: every reported status is dirty or untracked.
                if let Some(path) = record
                    .get(3..)
                    .and_then(|raw| std::str::from_utf8(raw).ok())
                {
                    retain.insert(path.to_owned());
                }
            }
        }
        // Not a Git repository (or no worktree): every present path is retained.
        None => retain.extend(paths.iter().cloned()),
    }
    let mut entries = Vec::with_capacity(paths.len());
    let mut baseline = Vec::new();
    let mut total_bytes = 0u64;
    let mut baseline_bytes = 0u64;
    for path in paths {
        let Some((metadata, bytes)) = crate::review_target::read_worktree_file(workspace, &path)?
        else {
            // A tracked path absent from the worktree is a deletion candidate,
            // not a domain entry at dispatch.
            continue;
        };
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(capacity());
        }
        // The 64 MiB ceiling is index-only: captured content is bounded
        // by the per-side bound and by the per-Run baseline aggregate.
        total_bytes = total_bytes.saturating_add(bytes.len() as u64);
        let digest = sha256_hex(&bytes);
        entries.push(DispatchEntry {
            path: path.clone(),
            sha256: digest.clone(),
            byte_length: bytes.len() as u64,
            mode: std::os::unix::fs::MetadataExt::mode(&metadata) & 0o7777,
            kind: "file",
        });
        if retain.contains(&path) {
            baseline_bytes = baseline_bytes.saturating_add(bytes.len() as u64);
            baseline.push((path, bytes));
        }
    }
    if entries.len() > MAX_FILES {
        return Err(capacity());
    }
    let head = crate::review_target::head(workspace)?;
    let index = DispatchIndex {
        schema: DISPATCH_INDEX_SCHEMA,
        task_ref: task_ref.to_string(),
        head_commit: head.as_deref(),
        file_count: entries.len() as u64,
        total_bytes,
        entries,
    };
    let canonical = canonical_json(&index)?.into_bytes();
    if canonical.len() as u64 > MAX_TARGET_BYTES {
        return Err(capacity());
    }
    let sha256 = sha256_hex(&canonical);
    Ok(DispatchIndexCapture {
        canonical,
        sha256,
        file_count: index.file_count,
        baseline,
        baseline_bytes,
        head,
    })
}

fn bytes_ref(bytes: &[u8]) -> BytesRef {
    BytesRef {
        byte_length: bytes.len() as u64,
        sha256: format!("sha256:{}", sha256_hex(bytes)),
    }
}

fn canonical_json(value: &impl Serialize) -> Result<String, MachineError> {
    let text = serde_json::to_string(value).map_err(|_| integrity())?;
    let canonical =
        canonicalize(&parse(&text).map_err(|_| integrity())?).map_err(|_| integrity())?;
    String::from_utf8(canonical).map_err(|_| integrity())
}

fn integrity() -> MachineError {
    MachineError::new(
        "INTERNAL_ERROR",
        "captured delegation content is inconsistent",
        false,
        serde_json::json!({"invariant":"successor_capture"}),
    )
}

fn capacity() -> MachineError {
    MachineError::new(
        "STORAGE_CAPACITY_EXCEEDED",
        "the captured delegation content exceeds its bound",
        false,
        serde_json::json!({"subject":"dispatch_index","required_action":"fix_request"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task_request::{SpecialistTaskRequest, TaskContext};
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::PathBuf;

    fn configuration() -> crate::run::AgentConfigurationSnapshot {
        let reservation: crate::run::CallerStartReservation = serde_json::from_str(include_str!(
            "../docs/protocol/examples/workspace-execution-start-reservation.valid.json"
        ))
        .unwrap();
        let instructions = crate::delegation_identity::InstructionSnapshot::compose(
            crate::instructions::SourceCapture::from_body("common source\n").unwrap(),
            crate::instructions::RoleCapture::from_body("reviewer", "role source\n").unwrap(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let binding = reservation.accepted_intent.global_profile_binding;
        let admitted = crate::delegation_identity::AdmittedConfiguration {
            schema: "dolgorae.admitted-configuration/v1".to_owned(),
            profile_binding_schema: crate::run::ADMITTED_PROFILE_BINDING_SCHEMA.to_owned(),
            profile_binding_digest: format!("sha256:{}", binding.digest().unwrap()),
            model: "gpt-5.6".to_owned(),
            effort: "high".to_owned(),
            purpose_kind: "review".to_owned(),
            purpose_label: None,
            required_capabilities: Vec::new(),
            execution_lane: "dedicated".to_owned(),
            requested_access: "canonical_workspace_write".to_owned(),
            required_assurance: "best_effort_personal_alpha".to_owned(),
            native_subagent_policy: "disabled".to_owned(),
            instruction_identity_digest: instructions.instruction_identity_digest.clone(),
        };
        crate::run::AgentConfigurationSnapshot::from_admitted(
            crate::run::AdmittedConfigurationInput {
                admitted,
                instructions,
                profile: &binding.selected_name,
                profile_binding: &binding,
                role_reference: Some("reviewer"),
            },
        )
        .unwrap()
    }

    fn task(contexts: usize) -> SpecialistTaskRequest {
        SpecialistTaskRequest {
            purpose: "change".to_owned(),
            brief: "Assess the accepted candidate.\n".to_owned(),
            contexts: (0..contexts)
                .map(|index| TaskContext {
                    id: format!("ctx-{index}"),
                    content: format!("context body {index}\n"),
                    provenance: "caller".to_owned(),
                })
                .collect(),
            criteria: Vec::new(),
            expected_output: "structured_review_v3".to_owned(),
        }
    }

    fn workspace() -> PathBuf {
        let root = std::env::temp_dir().join(format!("dolgorae-capture-{}", Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let git = |arguments: &[&str]| {
            assert!(
                std::process::Command::new("/usr/bin/git")
                    .arg("-C")
                    .arg(&root)
                    .args(["-c", "user.email=test@example.invalid"])
                    .args(["-c", "user.name=Dolgorae Test"])
                    .args(arguments)
                    .status()
                    .unwrap()
                    .success()
            );
        };
        std::fs::write(root.join("kept.txt"), b"clean\n").unwrap();
        git(&["init", "-q"]);
        git(&["add", "kept.txt"]);
        git(&["commit", "-q", "-m", "baseline"]);
        root
    }

    #[test]
    fn one_context_ref_per_caller_context_and_the_target_keeps_its_own_slot() {
        let configuration = configuration();
        let target = crate::review::ReviewTargetRequest {
            kind: "workspace".to_owned(),
            revision: None,
        };
        let (identity, manifest) = admitted_manifest(&target);
        let (accepted, contents) = capture_facade_task(
            &configuration,
            &task(3),
            "canonical_workspace_write",
            Some((identity.as_str(), manifest.as_slice())),
        )
        .unwrap();
        // The preserved bytes are the acceptance's own evidence.
        assert_eq!(contents.contexts.len(), 3);
        assert_eq!(contents.contexts[0].0, "ctx-0");
        assert_eq!(contents.target.as_deref(), Some(manifest.as_slice()));
        assert!(
            contents
                .prompt(&accepted, "canonical_workspace_write", 600)
                .unwrap()
                .contains("context body 0")
        );
        assert_eq!(accepted.contexts.len(), 4);
        let identities: Vec<&str> = accepted
            .contexts
            .iter()
            .map(|context| context.identity.as_str())
            .collect();
        assert_eq!(identities, ["ctx-0", "ctx-1", "ctx-2", identity.as_str()]);
        assert_eq!(accepted.contexts[0].kind, "accepted_context");
        assert_eq!(accepted.contexts[3].kind, "review_target");
        assert_eq!(accepted.contexts[3].content, bytes_ref(&manifest));
        // The retained task body is the exact canonical caller task.
        assert_eq!(
            accepted.task_body.sha256,
            format!(
                "sha256:{}",
                sha256_hex(canonical_json(&task(3)).unwrap().as_bytes())
            )
        );
        assert!(
            capture_facade_task(
                &configuration,
                &task(63),
                "canonical_workspace_write",
                Some((identity.as_str(), manifest.as_slice()))
            )
            .is_ok()
        );
        // 64 slots are accepted; a 65th slot is refused before any identity.
        assert!(
            capture_facade_task(&configuration, &task(64), "canonical_workspace_write", None)
                .is_ok()
        );
        assert!(
            capture_facade_task(
                &configuration,
                &task(64),
                "canonical_workspace_write",
                Some((identity.as_str(), manifest.as_slice()))
            )
            .is_err()
        );
    }

    fn admitted_manifest(target: &crate::review::ReviewTargetRequest) -> (String, Vec<u8>) {
        let mut manifest = serde_json::json!({"schema":"dolgorae-review-target/v2","kind":target.kind,"requested_revision":target.revision,
            "workspace_id":"a".repeat(64),"resolved_base":null,"resolved_head":null,"range":null,"assurance":"sampled_mutable",
            "reading_boundary":"checked_workspace","entries":[],"excluded":[],"admission_fingerprint":"b".repeat(64)});
        let identity = sha256_hex(&canonicalize(&parse(&manifest.to_string()).unwrap()).unwrap());
        manifest["target_sha256"] = serde_json::json!(identity);
        (identity, serde_json::to_vec(&manifest).unwrap())
    }

    #[test]
    fn dispatch_checks_the_target_owner_digest_and_exact_content_reference() {
        let selection = crate::review::ReviewTargetRequest {
            kind: "workspace".to_owned(),
            revision: None,
        };
        let (identity, manifest) = admitted_manifest(&selection);
        let (mut accepted, mut contents) = capture_facade_task(
            &configuration(),
            &task(1),
            "read_only",
            Some((&identity, &manifest)),
        )
        .unwrap();
        accepted.contexts.last_mut().unwrap().identity = "c".repeat(64);
        assert!(contents.prompt(&accepted, "read_only", 600).is_err());
        accepted.contexts.last_mut().unwrap().identity = identity;
        accepted.contexts.last_mut().unwrap().content.byte_length += 1;
        assert!(contents.prompt(&accepted, "read_only", 600).is_err());
        let mut substituted: serde_json::Value = serde_json::from_slice(&manifest).unwrap();
        substituted["kind"] = serde_json::json!("dirty");
        let bytes = serde_json::to_vec(&substituted).unwrap();
        accepted.contexts.last_mut().unwrap().content = bytes_ref(&bytes);
        contents.target = Some(bytes);
        assert!(contents.prompt(&accepted, "read_only", 600).is_err());
    }

    #[test]
    fn the_dispatch_index_retains_the_domain_the_head_and_only_the_dirty_baseline() {
        let root = workspace();
        std::fs::write(root.join("kept.txt"), b"changed\n").unwrap();
        std::fs::write(root.join("added.txt"), b"untracked\n").unwrap();
        std::fs::set_permissions(
            root.join("added.txt"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let task_ref = Uuid::now_v7();
        let capture = capture_dispatch_index(&root, task_ref).unwrap();
        assert!(capture.head.is_some());
        assert_eq!(capture.file_count, 2);
        let index: serde_json::Value = serde_json::from_slice(&capture.canonical).unwrap();
        assert_eq!(index["schema"], DISPATCH_INDEX_SCHEMA);
        assert_eq!(index["task_ref"], task_ref.to_string());
        let entries = index["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        let added = entries
            .iter()
            .find(|entry| entry["path"] == "added.txt")
            .unwrap();
        // The recorded mode is the worktree mode, not a normalized Git mode.
        assert_eq!(added["mode"], 0o755);
        assert_eq!(added["kind"], "file");
        // Only the dirty and untracked paths retain their complete bytes.
        let baseline: Vec<&str> = capture
            .baseline
            .iter()
            .map(|(path, _)| path.as_str())
            .collect();
        assert_eq!(baseline, ["added.txt", "kept.txt"]);
        assert_eq!(
            capture.baseline_bytes,
            "changed\n".len() as u64 + "untracked\n".len() as u64
        );
        assert_eq!(capture.sha256, sha256_hex(&capture.canonical));
        std::fs::remove_dir_all(root).unwrap();
    }

    /// One workspace whose baseline commit carries a `*.log` ignore rule (MD1).
    fn ignored_workspace() -> PathBuf {
        let root = workspace();
        std::fs::write(root.join(".gitignore"), b"*.log\n").unwrap();
        git(&root, &["add", ".gitignore"]);
        git(&root, &["commit", "-q", "-m", "ignore"]);
        root
    }

    fn git(root: &Path, arguments: &[&str]) {
        assert!(
            std::process::Command::new("/usr/bin/git")
                .arg("-C")
                .arg(root)
                .args(["-c", "user.email=test@example.invalid"])
                .args(["-c", "user.name=Dolgorae Test"])
                .args(arguments)
                .status()
                .unwrap()
                .success()
        );
    }

    /// One executor change item (MD1).
    fn item<'a>(path: &'a str, kind: &'a str, diff: &'a str) -> ExecutorFileChange<'a> {
        ExecutorFileChange {
            path,
            kind,
            diff,
            move_path: None,
        }
    }

    /// One executor move item: `path` is the source, `move_path` the destination.
    fn moved<'a>(path: &'a str, diff: &'a str, destination: &'a str) -> ExecutorFileChange<'a> {
        ExecutorFileChange {
            path,
            kind: "update",
            diff,
            move_path: Some(destination),
        }
    }

    /// No dispatch-index path belongs to Part A in the unit fixtures.
    fn no_domain() -> std::collections::BTreeSet<String> {
        std::collections::BTreeSet::new()
    }

    /// A move of two domain-side paths is Part A's, so the producer skips it
    /// before any read or diff parsing.
    #[test]
    fn a_move_of_two_domain_side_paths_is_skipped_without_parsing() {
        let root = ignored_workspace();
        std::fs::write(root.join("renamed.txt"), b"tracked move\n").unwrap();
        let mut domain = std::collections::BTreeSet::new();
        domain.insert("README.txt".to_owned());
        domain.insert("renamed.txt".to_owned());
        // The pinned diff may carry a "Moved to:" suffix the applier refuses; a
        // domain-side move never reaches it, so nothing becomes unavailable.
        let changes = ignored_path_changes(
            &root,
            &domain,
            &[moved(
                "README.txt",
                "@@ -1 +1 @@\n-old\n+new\n\nMoved to: renamed.txt\n",
                "renamed.txt",
            )],
        )
        .unwrap();
        assert!(changes.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Only the ignored-side path of a move is checked and produced.
    #[test]
    fn only_the_ignored_side_of_a_move_is_produced() {
        let root = ignored_workspace();
        // A tracked source moving to an ignored destination: the destination is the
        // only side this producer owns, and its bytes need no diff.
        std::fs::write(root.join("moved.log"), b"moved out\n").unwrap();
        let mut domain = std::collections::BTreeSet::new();
        domain.insert("tracked.txt".to_owned());
        let changes = ignored_path_changes(
            &root,
            &domain,
            &[moved(
                "tracked.txt",
                "@@ -1 +1 @@\n-tracked\n+moved out\n\nMoved to: moved.log\n",
                "moved.log",
            )],
        )
        .unwrap();
        let kinds: Vec<(&str, &str)> = changes
            .iter()
            .map(|change| (change.path.as_str(), change.kind.as_str()))
            .collect();
        assert_eq!(kinds, vec![("moved.log", "add")]);
        assert!(changes[0].before.is_none());
        assert_eq!(
            changes[0].after.as_ref().unwrap().sha256,
            format!("sha256:{}", crate::jcs::sha256_hex(b"moved out\n"))
        );
        // An ignored source moving out to a domain-side destination: only the
        // source's delete is this producer's, and its before image still needs the
        // destination's bytes and the diff.
        std::fs::remove_file(root.join("moved.log")).unwrap();
        std::fs::write(root.join("tracked-destination.txt"), b"first\nnew\n").unwrap();
        let mut domain = std::collections::BTreeSet::new();
        domain.insert("tracked-destination.txt".to_owned());
        let changes = ignored_path_changes(
            &root,
            &domain,
            &[moved(
                "old.log",
                "@@ -1,2 +1,2 @@\n first\n-old\n+new\n",
                "tracked-destination.txt",
            )],
        )
        .unwrap();
        let kinds: Vec<(&str, &str)> = changes
            .iter()
            .map(|change| (change.path.as_str(), change.kind.as_str()))
            .collect();
        assert_eq!(kinds, vec![("old.log", "delete")]);
        assert_eq!(
            changes[0].before.as_ref().unwrap().sha256,
            format!("sha256:{}", crate::jcs::sha256_hex(b"first\nold\n"))
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// MD1: only the named ignored-side paths are produced, a diff that does not
    /// verify is refused, and the merge never replaces a domain change.
    #[test]
    fn ignored_path_changes_cover_items_only_and_merge_without_replacing() {
        let root = ignored_workspace();
        // One ignored path edited through an item (update), one added, one deleted.
        std::fs::write(root.join("debug.log"), b"one\ntwo changed\n").unwrap();
        std::fs::write(root.join("new.log"), b"added\n").unwrap();
        // A second ignored path changed only by a "shell command": no item names it.
        std::fs::write(root.join("shell.log"), b"unrelated\n").unwrap();
        let changes = ignored_path_changes(
            &root,
            &no_domain(),
            &[
                item(
                    "debug.log",
                    "update",
                    "@@ -1,2 +1,2 @@\n one\n-two\n+two changed\n",
                ),
                item("new.log", "add", "@@ -0,0 +1 @@\n+added\n"),
                item("gone.log", "delete", "@@ -1 +0,0 @@\n-removed\n"),
            ],
        )
        .unwrap();
        let kinds: Vec<(&str, &str)> = changes
            .iter()
            .map(|change| (change.path.as_str(), change.kind.as_str()))
            .collect();
        // The shell-only path is absent; the item paths are present in order.
        assert_eq!(
            kinds,
            vec![
                ("debug.log", "modify"),
                ("gone.log", "delete"),
                ("new.log", "add")
            ]
        );
        assert!(changes.iter().all(|change| change.before_mode.is_none()));
        assert!(changes.iter().all(|change| change.old_path.is_none()));
        let debug = &changes[0];
        assert_eq!(
            debug.before.as_ref().unwrap().sha256,
            format!("sha256:{}", crate::jcs::sha256_hex(b"one\ntwo\n"))
        );
        assert_eq!(debug.after_mode, Some(0o644));
        // A diff that does not verify against the current bytes is a typed failure.
        assert!(
            ignored_path_changes(
                &root,
                &no_domain(),
                &[item(
                    "debug.log",
                    "update",
                    "@@ -1,2 +1,2 @@\n nope\n-two\n+two changed\n"
                )]
            )
            .is_err()
        );
        // A path the dispatch-index domain owns is Part A's even when an item names
        // it, so the two change sets stay disjoint.
        let mut domain = std::collections::BTreeSet::new();
        domain.insert("debug.log".to_owned());
        assert!(
            ignored_path_changes(
                &root,
                &domain,
                &[item(
                    "debug.log",
                    "update",
                    "@@ -1,2 +1,2 @@\n one\n-two\n+two changed\n"
                )]
            )
            .unwrap()
            .is_empty()
        );
        // The merge concatenates and sorts; a domain change is kept unchanged.
        let domain_change = crate::delegation_identity::ChangeRef {
            kind: "modify".to_owned(),
            path: "b.txt".to_owned(),
            old_path: None,
            before: None,
            after: None,
            before_mode: Some(0o644),
            after_mode: Some(0o600),
        };
        let merged = merge_changes(vec![domain_change.clone()], changes.changes.clone());
        assert_eq!(merged.len(), changes.len() + 1);
        assert_eq!(merged[0].path, "b.txt");
        assert_eq!(merged[0], domain_change);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A pure insertion is applied, so the producer counts it as taking
    /// effect instead of dropping the change as "did not take effect".
    #[test]
    fn a_pure_insertion_counts_as_taking_effect() {
        let root = ignored_workspace();
        std::fs::write(root.join("inserted.log"), b"a\nx\nb\n").unwrap();
        let changes = ignored_path_changes(
            &root,
            &no_domain(),
            &[item("inserted.log", "update", "@@ -1,0 +2 @@\n+x\n")],
        )
        .unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, "modify");
        assert_eq!(
            changes[0].before.as_ref().unwrap().sha256,
            format!("sha256:{}", crate::jcs::sha256_hex(b"a\nb\n"))
        );
        assert_eq!(
            changes[0].after.as_ref().unwrap().sha256,
            format!("sha256:{}", crate::jcs::sha256_hex(b"a\nx\nb\n"))
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Every intermediate before image of the chain is admitted as it is
    /// produced, not only the chain's final cursor.
    #[test]
    fn an_over_bound_intermediate_before_image_is_refused() {
        let root = ignored_workspace();
        // The chain's final before image is 2 bytes; its intermediate one is 6.
        std::fs::write(root.join("chain.log"), b"z\n").unwrap();
        let chain = [
            item("chain.log", "update", "@@ -1 +1 @@\n-x\n+yoyoy\n"),
            item("chain.log", "update", "@@ -1 +1 @@\n-yoyoy\n+z\n"),
        ];
        assert!(
            ignored_path_changes_within(&root, &no_domain(), &chain, 4).is_err(),
            "an intermediate over the side bound is unavailable"
        );
        // Under the plan's 8 MiB side bound the same chain is one change.
        let changes = ignored_path_changes(&root, &no_domain(), &chain).unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(
            changes[0].before.as_ref().unwrap().sha256,
            format!("sha256:{}", crate::jcs::sha256_hex(b"x\n"))
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A change set the closed complete result cannot carry is unavailable.
    #[test]
    fn an_unrepresentable_change_set_is_unavailable() {
        let change = |path: &str| crate::delegation_identity::ChangeRef {
            kind: "modify".to_owned(),
            path: path.to_owned(),
            old_path: None,
            before: None,
            after: None,
            before_mode: None,
            after_mode: None,
        };
        assert!(ensure_representable_changes(&[change("a"), change("b")], 2).is_ok());
        // More changes than the named ceiling.
        assert!(ensure_representable_changes(&[change("a"), change("b"), change("c")], 2).is_err());
        // Paths the closed `ChangeRef` cannot carry.
        for path in [
            "with\nnewline",
            "with\rreturn",
            "with\u{2028}separator",
            "../escape",
            "/absolute",
        ] {
            assert!(
                ensure_representable_changes(&[change(path)], 20).is_err(),
                "{path:?}"
            );
        }
    }

    /// An update that did not take effect contributes nothing, while an update
    /// whose path is gone cannot rebuild a whole before image.
    #[test]
    fn an_update_that_did_not_take_effect_contributes_nothing() {
        let root = ignored_workspace();
        let diff = "@@ -1,2 +1,2 @@\n one\n-two\n+two changed\n";
        // The executor's own before side is still in place: the edit did not happen.
        std::fs::write(root.join("debug.log"), b"one\ntwo\n").unwrap();
        assert!(
            ignored_path_changes(&root, &no_domain(), &[item("debug.log", "update", diff)])
                .unwrap()
                .is_empty()
        );
        // A path that is absent now has no before image to reconstruct.
        std::fs::remove_file(root.join("debug.log")).unwrap();
        assert!(
            ignored_path_changes(&root, &no_domain(), &[item("debug.log", "update", diff)])
                .is_err()
        );
        // Bytes the diff neither reverses nor forwards describe are unavailable.
        std::fs::write(root.join("debug.log"), b"unrelated\n").unwrap();
        assert!(
            ignored_path_changes(&root, &no_domain(), &[item("debug.log", "update", diff)])
                .is_err()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// At the producer level: a delete that did not take effect contributes
    /// nothing, a partial present state is unavailable, and an absent path
    /// rewinds to its whole removed content.
    #[test]
    fn a_delete_that_did_not_take_effect_contributes_nothing() {
        let root = ignored_workspace();
        let diff = "@@ -1,2 +0,0 @@\n-one\n-two\n";
        // Declined: the file is still there with exactly the removed content.
        std::fs::write(root.join("gone.log"), b"one\ntwo\n").unwrap();
        assert!(
            ignored_path_changes(&root, &no_domain(), &[item("gone.log", "delete", diff)])
                .unwrap()
                .is_empty()
        );
        // Present with other bytes is neither removed nor reconstructible.
        std::fs::write(root.join("gone.log"), b"changed\n").unwrap();
        assert!(
            ignored_path_changes(&root, &no_domain(), &[item("gone.log", "delete", diff)]).is_err()
        );
        std::fs::remove_file(root.join("gone.log")).unwrap();
        let changes =
            ignored_path_changes(&root, &no_domain(), &[item("gone.log", "delete", diff)]).unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, "delete");
        assert!(changes[0].after.is_none() && changes[0].after_mode.is_none());
        assert_eq!(
            changes[0].before.as_ref().unwrap().sha256,
            format!("sha256:{}", crate::jcs::sha256_hex(b"one\ntwo\n"))
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// One path's changes are chained newest first into exactly one change.
    #[test]
    fn several_changes_of_one_path_are_chained_into_one_change_ref() {
        let root = ignored_workspace();
        // update then update: the before is the original content.
        std::fs::write(root.join("twice.log"), b"one\ntwo changed twice\n").unwrap();
        // add then update: the result is one add carrying the final bytes.
        std::fs::write(root.join("added-then.log"), b"created\nmore\n").unwrap();
        // update then delete: the result is one delete carrying the original bytes.
        let changes = ignored_path_changes(
            &root,
            &no_domain(),
            &[
                item(
                    "twice.log",
                    "update",
                    "@@ -1,2 +1,2 @@\n one\n-two\n+two changed\n",
                ),
                item(
                    "twice.log",
                    "update",
                    "@@ -1,2 +1,2 @@\n one\n-two changed\n+two changed twice\n",
                ),
                item("added-then.log", "add", "@@ -0,0 +1 @@\n+created\n"),
                item(
                    "added-then.log",
                    "update",
                    "@@ -1 +1,2 @@\n-created\n+created\n+more\n",
                ),
                item(
                    "updated-then-deleted.log",
                    "update",
                    "@@ -1 +1 @@\n-before\n+updated\n",
                ),
                item(
                    "updated-then-deleted.log",
                    "delete",
                    "@@ -1 +0,0 @@\n-updated\n",
                ),
            ],
        )
        .unwrap();
        let kinds: Vec<(&str, &str)> = changes
            .iter()
            .map(|change| (change.path.as_str(), change.kind.as_str()))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("added-then.log", "add"),
                ("twice.log", "modify"),
                ("updated-then-deleted.log", "delete")
            ]
        );
        let by_path = |path: &str| {
            changes
                .iter()
                .find(|change| change.path == path)
                .unwrap()
                .clone()
        };
        // The two updates rewind to the file's original content.
        let twice = by_path("twice.log");
        assert_eq!(
            twice.before.as_ref().unwrap().sha256,
            format!("sha256:{}", crate::jcs::sha256_hex(b"one\ntwo\n"))
        );
        assert_eq!(
            twice.after.as_ref().unwrap().sha256,
            format!(
                "sha256:{}",
                crate::jcs::sha256_hex(b"one\ntwo changed twice\n")
            )
        );
        // The add rewinds to absence, so the final change is the add itself.
        let added = by_path("added-then.log");
        assert!(added.before.is_none());
        assert_eq!(
            added.after.as_ref().unwrap().sha256,
            format!("sha256:{}", crate::jcs::sha256_hex(b"created\nmore\n"))
        );
        // The delete is the newest change, so the update before it still rewinds.
        let deleted = by_path("updated-then-deleted.log");
        assert!(deleted.after.is_none());
        assert_eq!(
            deleted.before.as_ref().unwrap().sha256,
            format!("sha256:{}", crate::jcs::sha256_hex(b"before\n"))
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// An add is only a path's oldest change and a delete only its newest.
    #[test]
    fn an_add_or_a_delete_in_the_wrong_position_is_unavailable() {
        let root = ignored_workspace();
        std::fs::write(root.join("late-add.log"), b"late\n").unwrap();
        // An add after another change of the same path would rewrite history.
        assert!(
            ignored_path_changes(
                &root,
                &no_domain(),
                &[
                    item("late-add.log", "update", "@@ -1 +1 @@\n-before\n+late\n"),
                    item("late-add.log", "add", "@@ -0,0 +1 @@\n+late\n"),
                ]
            )
            .is_err()
        );
        // A delete before a later change of the same path is not its newest change.
        std::fs::write(root.join("early-delete.log"), b"after\n").unwrap();
        assert!(
            ignored_path_changes(
                &root,
                &no_domain(),
                &[
                    item("early-delete.log", "delete", "@@ -1 +0,0 @@\n-before\n"),
                    item(
                        "early-delete.log",
                        "update",
                        "@@ -1 +1 @@\n-before\n+after\n"
                    ),
                ]
            )
            .is_err()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A move is a delete of its source plus an add of its destination, with the
    /// source's before image taken by reverse application against the destination.
    #[test]
    fn a_move_is_a_delete_of_its_source_and_an_add_of_its_destination() {
        let root = ignored_workspace();
        std::fs::write(root.join("new.log"), b"first\nnew\n").unwrap();
        let diff = "@@ -1,2 +1,2 @@\n first\n-old\n+new\n";
        let changes =
            ignored_path_changes(&root, &no_domain(), &[moved("old.log", diff, "new.log")])
                .unwrap();
        let kinds: Vec<(&str, &str)> = changes
            .iter()
            .map(|change| (change.path.as_str(), change.kind.as_str()))
            .collect();
        assert_eq!(kinds, vec![("new.log", "add"), ("old.log", "delete")]);
        assert!(changes.iter().all(|change| change.old_path.is_none()));
        let added = &changes[0];
        assert!(added.before.is_none());
        assert_eq!(added.after_mode, Some(0o644));
        let deleted = &changes[1];
        assert!(deleted.after.is_none() && deleted.after_mode.is_none());
        // The context line is what makes this a reverse application of the
        // destination rather than the removed lines alone.
        assert_eq!(
            deleted.before.as_ref().unwrap().sha256,
            format!("sha256:{}", crate::jcs::sha256_hex(b"first\nold\n"))
        );
        // A source that is still present is not a completed move.
        std::fs::write(root.join("old.log"), b"first\nold\n").unwrap();
        assert!(
            ignored_path_changes(&root, &no_domain(), &[moved("old.log", diff, "new.log")])
                .is_err()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// An unaccepted move diff, or another change touching either side, is
    /// unavailable.
    #[test]
    fn a_move_with_an_unaccepted_diff_or_another_touch_is_unavailable() {
        let root = ignored_workspace();
        std::fs::write(root.join("new.log"), b"first\nnew\n").unwrap();
        let diff = "@@ -1,2 +1,2 @@\n first\n-old\n+new\n";
        // A trailing "Moved to:" line is not part of the accepted diff format.
        assert!(
            ignored_path_changes(
                &root,
                &no_domain(),
                &[moved(
                    "old.log",
                    &format!("{diff}\nMoved to: new.log\n"),
                    "new.log"
                )]
            )
            .is_err()
        );
        // Another change naming the source makes the move's direction ambiguous.
        assert!(
            ignored_path_changes(
                &root,
                &no_domain(),
                &[
                    moved("old.log", diff, "new.log"),
                    item("old.log", "update", "@@ -1 +1 @@\n-x\n+y\n"),
                ]
            )
            .is_err()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// MD1: the reverse unified-diff applier accepts only verified evidence.
    #[test]
    fn the_reverse_applier_reconstructs_verified_before_bytes() {
        // An update: the removed line reconstructs the before, the context matches.
        let after = b"one\ntwo changed\nthree\n";
        let diff = "--- a/ignored.txt\n+++ b/ignored.txt\n@@ -1,3 +1,3 @@\n one\n-two\n+two changed\n three\n";
        assert_eq!(
            reverse_unified_diff(Some(after), diff).unwrap(),
            Some(b"one\ntwo\nthree\n".to_vec())
        );
        // An empty-after file reconstructs from the removed lines alone.
        let diff = "--- a/ignored.txt\n+++ b/ignored.txt\n@@ -1 +0,0 @@\n-gone\n";
        assert_eq!(
            reverse_unified_diff(Some(b""), diff).unwrap(),
            Some(b"gone\n".to_vec())
        );
        // The absent-after branch keeps the removed lines; the whole-file delete
        // reconstruction is `deleted_file_before`.
        assert_eq!(
            reverse_unified_diff(None, diff).unwrap(),
            Some(b"gone\n".to_vec())
        );
        assert_eq!(deleted_file_before(diff).unwrap(), b"gone\n");
        // An add has no before image at all (the caller emits before = null).
        let add = "--- /dev/null\n+++ b/ignored.txt\n@@ -0,0 +1 @@\n+new\n";
        assert_eq!(
            reverse_unified_diff(Some(b"new\n"), add).unwrap(),
            Some(Vec::new())
        );
        // Rejected: non-unified body, missing headers, no hunk, offset outside the
        // file, and a context that does not match the after bytes.
        for (after, diff) in [
            (Some(&b"one\n"[..]), "not a unified diff\n"),
            (Some(&b"one\n"[..]), "+++ b/x\n@@ -1 +1 @@\n one\n"),
            (Some(&b"one\n"[..]), "--- a/x\n+++ b/x\n one\n"),
            (Some(&b"one\n"[..]), "--- a/x\n+++ b/x\n@@ -1 +1 @@\n two\n"),
            (Some(&b"one\n"[..]), "--- a/x\n+++ b/x\n@@ -9 +9 @@\n one\n"),
        ] {
            assert!(reverse_unified_diff(after, diff).is_err(), "{diff}");
        }
        // A body line that is neither context, added, removed nor a hunk marker.
        assert!(
            reverse_unified_diff(Some(b"one\n"), "--- a/x\n+++ b/x\n@@ -1 +1 @@\n!bogus\n")
                .is_err()
        );
    }

    /// The `---`/`+++` header pair is optional, and nothing else may precede
    /// the first hunk.
    #[test]
    fn the_applier_accepts_hunk_only_diffs_and_refuses_anything_before_the_first_hunk() {
        // The real update form is hunk-only (the pinned runtime emits no headers).
        let after = b"one\ntwo changed\n";
        let hunk = "@@ -1,2 +1,2 @@\n one\n-two\n+two changed\n";
        assert_eq!(
            reverse_unified_diff(Some(after), hunk).unwrap(),
            Some(b"one\ntwo\n".to_vec())
        );
        // The same diff with the optional header pair is accepted too.
        assert_eq!(
            reverse_unified_diff(Some(after), &format!("--- a/x\n+++ b/x\n{hunk}")).unwrap(),
            Some(b"one\ntwo\n".to_vec())
        );
        // Junk before the first hunk is unavailable, with or without headers.
        for rejected in [
            "junk\n@@ -1,2 +1,2 @@\n one\n-two\n+two changed\n",
            "--- a/x\n+++ b/x\njunk\n@@ -1,2 +1,2 @@\n one\n-two\n+two changed\n",
            "+++ b/x\n@@ -1,2 +1,2 @@\n one\n-two\n+two changed\n",
            "--- a/x\n@@ -1,2 +1,2 @@\n one\n-two\n+two changed\n",
            "--- a/x\n+++ b/x\n",
        ] {
            assert!(
                reverse_unified_diff(Some(after), rejected).is_err(),
                "{rejected}"
            );
        }
    }

    /// The no-newline marker strips the newline of the line before it instead
    /// of being skipped.
    #[test]
    fn the_applier_applies_the_no_newline_marker_instead_of_skipping_it() {
        // The before side had no final newline: the marker follows its `-` line.
        let before_without_newline =
            "@@ -1,2 +1,2 @@\n one\n-two\n\\ No newline at end of file\n+two\n";
        assert_eq!(
            reverse_unified_diff(Some(b"one\ntwo\n"), before_without_newline).unwrap(),
            Some(b"one\ntwo".to_vec())
        );
        // The after side has no final newline: the marker follows its `+` line.
        let after_without_newline =
            "@@ -1,2 +1,2 @@\n one\n-two\n+two changed\n\\ No newline at end of file\n";
        assert_eq!(
            reverse_unified_diff(Some(b"one\ntwo changed"), after_without_newline).unwrap(),
            Some(b"one\ntwo\n".to_vec())
        );
        // A marker with no preceding body line carries nothing to strip.
        assert!(
            reverse_unified_diff(
                Some(b"one\n"),
                "@@ -1 +1 @@\n\\ No newline at end of file\n one\n"
            )
            .is_err()
        );
        // Every line of the diff text is newline-terminated, so an
        // unterminated final line is refused instead of silently dropping the
        // final newline of a before image.
        assert!(reverse_unified_diff(None, "@@ -1,1 +0,0 @@\n-one").is_err());
        assert!(deleted_file_before("@@ -1,1 +0,0 @@\n-one").is_err());
    }

    /// A zero-count range names the line before the position, on both sides.
    #[test]
    fn crlf_diff_round_trip_preserves_exact_bytes() {
        let before = b"first\r\nbefore\r\nlast\r\n";
        let after = b"first\r\nafter\r\nlast\r\n";
        let diff = "@@ -1,3 +1,3 @@\n first\r\n-before\r\n+after\r\n last\r\n";
        assert_eq!(
            reverse_unified_diff(Some(after), diff).unwrap().unwrap(),
            before
        );
        assert!(forward_applies(before, diff).unwrap());
        assert!(!forward_applies(b"first\nbefore\nlast\n", diff).unwrap());
    }

    #[test]
    fn zero_count_hunk_ranges_name_the_position_before_the_line() {
        // A mid-file pure insertion: the old side is empty (`-1,0`), so the
        // insertion point is index 1, not index 0.
        let after = b"a\nx\nb\n";
        let diff = "@@ -1,0 +2 @@\n+x\n";
        assert_eq!(
            reverse_unified_diff(Some(after), diff).unwrap(),
            Some(b"a\nb\n".to_vec())
        );
        assert!(forward_applies(b"a\nb\n", diff).unwrap());
        // A mid-file pure deletion: the new side is empty (`+1,0`), so the
        // deletion point is index 1 of the after file.
        let after = b"a\nb\n";
        let diff = "@@ -2 +1,0 @@\n-x\n";
        assert_eq!(
            reverse_unified_diff(Some(after), diff).unwrap(),
            Some(b"a\nx\nb\n".to_vec())
        );
        assert!(forward_applies(b"a\nx\nb\n", diff).unwrap());
        // A top-of-file insertion (`-0,0`) starts at index 0 on both sides.
        let diff = "@@ -0,0 +1 @@\n+x\n";
        assert_eq!(
            reverse_unified_diff(Some(b"x\na\n"), diff).unwrap(),
            Some(b"a\n".to_vec())
        );
        assert!(forward_applies(b"a\n", diff).unwrap());
        // A range whose end overflows usize is unavailable, not a wrapped index.
        assert!(
            reverse_unified_diff(
                Some(b"a\nb\n"),
                "@@ -18446744073709551615,2 +1,2 @@\n a\n b\n"
            )
            .is_err()
        );
    }

    /// Declared counts and hunk order are exact.
    #[test]
    fn the_applier_requires_exact_hunk_counts_and_ordered_hunks() {
        // A body holding fewer lines than its header declares is unavailable.
        assert!(reverse_unified_diff(Some(b"one\n"), "@@ -1,2 +1,2 @@\n one\n").is_err());
        // So is a body holding more.
        assert!(reverse_unified_diff(Some(b"one\n"), "@@ -1 +1 @@\n one\n+another\n").is_err());
        // Two ordered hunks are accepted and reconstruct the original file.
        let before = b"one\ntwo\nthree\nfour\n";
        let after = b"one\ntwo changed\nthree\nfour changed\n";
        let diff = "@@ -1,2 +1,2 @@\n one\n-two\n+two changed\n@@ -3,2 +3,2 @@\n three\n-four\n+four changed\n";
        assert_eq!(
            reverse_unified_diff(Some(after), diff).unwrap(),
            Some(before.to_vec())
        );
        assert!(forward_applies(before, diff).unwrap());
        // Hunks that overlap on either side are unavailable.
        let overlapping = "@@ -1,2 +1,2 @@\n one\n-two\n+two changed\n@@ -2,2 +2,2 @@\n two\n-three\n+three changed\n";
        assert!(reverse_unified_diff(Some(after), overlapping).is_err());
    }

    /// Only a whole-file delete reconstructs a before image.
    #[test]
    fn a_whole_file_delete_reconstructs_and_a_partial_delete_is_refused() {
        let diff = "@@ -1,2 +0,0 @@\n-one\n-two\n";
        assert_eq!(deleted_file_before(diff).unwrap(), b"one\ntwo\n");
        // The optional header pair does not change the reconstruction.
        assert_eq!(
            deleted_file_before(&format!("--- a/gone.log\n+++ b/gone.log\n{diff}")).unwrap(),
            b"one\ntwo\n"
        );
        // A context line is not a whole-file delete.
        assert!(deleted_file_before("@@ -1,2 +0,1 @@\n one\n-two\n").is_err());
        // Nor is a delete that starts at line 5, or one split over two hunks.
        assert!(deleted_file_before("@@ -5,1 +0,0 @@\n-gone\n").is_err());
        assert!(
            deleted_file_before("@@ -1,1 +0,0 @@\n-one\n@@ -3,1 +0,0 @@\n-two\n").is_err(),
            "a partial delete is never a full before image"
        );
    }

    /// The forward check recognizes the item's own before side, which is how a
    /// change that did not take effect contributes nothing.
    #[test]
    fn the_forward_check_recognizes_an_item_that_did_not_take_effect() {
        let before = b"one\ntwo\n";
        let update = "@@ -1,2 +1,2 @@\n one\n-two\n+two changed\n";
        assert!(forward_applies(before, update).unwrap());
        assert!(!forward_applies(b"one\n", update).unwrap());
        let delete = "@@ -1,2 +0,0 @@\n-one\n-two\n";
        assert!(forward_applies(before, delete).unwrap());
        // A diff the applier refuses is unavailable, never "not applicable".
        assert!(forward_applies(before, "not a diff\n").is_err());
    }

    /// The retained-content aggregate spans every file, so several changes that
    /// are each within the bound but together exceed it are unavailable.
    #[test]
    fn a_retained_total_over_the_aggregate_is_unavailable() {
        let root = workspace();
        std::fs::write(root.join("second.txt"), b"base\n").unwrap();
        git(&root, &["add", "second.txt"]);
        git(&root, &["commit", "-q", "-m", "second"]);
        let task_ref = Uuid::now_v7();
        let index = capture_dispatch_index(&root, task_ref).unwrap();
        let facts = parse_dispatch_index(&index.canonical, task_ref).unwrap();
        let baseline = index.baseline.clone();
        // Two modified files: each side is 6-7 bytes, so each change is 12-13 bytes.
        std::fs::write(root.join("kept.txt"), b"edited\n").unwrap();
        std::fs::write(root.join("second.txt"), b"edited\n").unwrap();
        // Every change fits one bound of 16 bytes, but their total does not.
        let wide = complete_result_changes_within(&root, &facts, &baseline, 16);
        assert_eq!(
            wide.unwrap_err().code,
            "COMPLETE_RESULT_UNAVAILABLE",
            "the aggregate spans files"
        );
        // Under the plan's own bound the same two changes are produced.
        let produced = complete_result_changes(&root, &facts, &baseline).unwrap();
        assert_eq!(produced.changes.len(), 2);
        assert_eq!(produced.retained.len(), 4);
        // The merged total is checked once more before the settlement retains it.
        assert!(ensure_retained_within(&produced.retained, 25).is_ok());
        assert_eq!(
            ensure_retained_within(&produced.retained, 24)
                .unwrap_err()
                .code,
            "COMPLETE_RESULT_UNAVAILABLE"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The retained index must be the one this task recorded.
    #[test]
    fn a_dispatch_index_of_another_shape_is_unavailable() {
        let root = workspace();
        let task_ref = Uuid::now_v7();
        let capture = capture_dispatch_index(&root, task_ref).unwrap();
        assert!(parse_dispatch_index(&capture.canonical, task_ref).is_ok());
        // Another task's index is not this task's before-basis.
        assert_eq!(
            parse_dispatch_index(&capture.canonical, Uuid::now_v7())
                .unwrap_err()
                .code,
            "COMPLETE_RESULT_UNAVAILABLE"
        );
        // A different schema, an unknown field and a wrong declared count are
        // refused as well, never parsed into a different index.
        let wire: serde_json::Value = serde_json::from_slice(&capture.canonical).unwrap();
        let original = wire.clone();
        for mutate in [
            |value: &mut serde_json::Value| {
                value["schema"] = serde_json::json!("dolgorae.dispatch-index/v2")
            },
            |value: &mut serde_json::Value| value["unknown_field"] = serde_json::json!(1),
            |value: &mut serde_json::Value| value["file_count"] = serde_json::json!(7),
        ] {
            let mut changed = original.clone();
            mutate(&mut changed);
            let text = serde_json::to_string(&changed).unwrap();
            // A serde-level refusal is the content condition too, never an
            // INTERNAL_ERROR that retries forever.
            assert_eq!(
                parse_dispatch_index(text.as_bytes(), task_ref)
                    .unwrap_err()
                    .code,
                "COMPLETE_RESULT_UNAVAILABLE",
                "{changed}"
            );
        }
        // Corrupt bytes are the same content condition.
        assert_eq!(
            parse_dispatch_index(b"{not json", task_ref)
                .unwrap_err()
                .code,
            "COMPLETE_RESULT_UNAVAILABLE"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The producer keeps the exact bytes of both sides, bounded by an
    /// explicit aggregate rather than an unbounded buffer.
    #[test]
    fn the_producer_retains_both_sides_within_an_explicit_aggregate() {
        let root = workspace();
        let task_ref = Uuid::now_v7();
        let index = capture_dispatch_index(&root, task_ref).unwrap();
        let facts = parse_dispatch_index(&index.canonical, task_ref).unwrap();
        let baseline = index.baseline.clone();
        std::fs::write(root.join("kept.txt"), b"changed\n").unwrap();
        let produced = complete_result_changes(&root, &facts, &baseline).unwrap();
        assert_eq!(produced.changes.len(), 1);
        let sides: Vec<(&str, &str, &[u8])> = produced
            .retained
            .iter()
            .map(|side| (side.path.as_str(), side.side, side.bytes.as_slice()))
            .collect();
        assert_eq!(
            sides,
            vec![
                ("kept.txt", "before", &b"clean\n"[..]),
                ("kept.txt", "after", &b"changed\n"[..]),
            ]
        );
        // The aggregate is what bounds the memory: a set over it is unavailable.
        let mut bounded = RetainedSides::new(4);
        assert!(bounded.push("a.txt", "before", b"one\n").is_ok());
        assert_eq!(
            bounded.push("a.txt", "after", b"two\n").unwrap_err().code,
            "COMPLETE_RESULT_UNAVAILABLE"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A file created and committed during the task is an add, although
    /// `git status` against the current HEAD calls it clean.
    #[test]
    fn a_file_committed_during_the_task_is_an_add() {
        let root = workspace();
        let task_ref = Uuid::now_v7();
        let index = capture_dispatch_index(&root, task_ref).unwrap();
        let facts = parse_dispatch_index(&index.canonical, task_ref).unwrap();
        let baseline = index.baseline.clone();
        // The executor creates the file and commits it, so the worktree matches
        // the current HEAD and the dispatch index does not have it.
        std::fs::write(root.join("committed.txt"), b"committed during the task\n").unwrap();
        git(&root, &["add", "committed.txt"]);
        git(&root, &["commit", "-q", "-m", "later"]);
        assert_eq!(
            crate::review_target::git_status(&root, &["status", "--porcelain=v1"])
                .unwrap()
                .unwrap_or_default(),
            b""
        );
        let changes = complete_result_changes(&root, &facts, &baseline).unwrap();
        let added = changes
            .iter()
            .find(|change| change.path == "committed.txt")
            .expect("the committed file is an add");
        assert_eq!(added.kind, "add");
        assert!(added.before.is_none());
        assert_eq!(
            added.after.as_ref().unwrap().sha256,
            format!(
                "sha256:{}",
                crate::jcs::sha256_hex(b"committed during the task\n")
            )
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A failed listing propagates instead of publishing an empty change set.
    #[test]
    fn a_failed_listing_propagates() {
        let root = workspace();
        let task_ref = Uuid::now_v7();
        let index = capture_dispatch_index(&root, task_ref).unwrap();
        let facts = parse_dispatch_index(&index.canonical, task_ref).unwrap();
        let baseline = index.baseline.clone();
        std::fs::rename(root.join(".git"), root.join(".git-away")).unwrap();
        let error = complete_result_changes(&root, &facts, &baseline).unwrap_err();
        assert_eq!(error.code, "REVIEW_TARGET_GIT_FAILED");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A non-UTF-8 listed path is the typed unavailable result, never
    /// skipped. This volume refuses such names, so the parser is the proof.
    #[test]
    fn a_non_utf8_listed_path_is_unavailable() {
        let listing = b"ok.txt\0bad\xff.txt\0";
        assert_eq!(
            parse_domain_listing(listing).unwrap_err().code,
            "COMPLETE_RESULT_UNAVAILABLE"
        );
        assert_eq!(
            parse_domain_listing(b"ok.txt\0nested/also.txt\0")
                .unwrap()
                .len(),
            2
        );
    }

    /// Part A: the complete-result producer over tracked and untracked paths.
    #[test]
    fn the_producer_reports_modified_added_deleted_and_mode_only_changes_in_order() {
        let root = workspace();
        let task_ref = Uuid::now_v7();
        let index = capture_dispatch_index(&root, task_ref).unwrap();
        let facts = parse_dispatch_index(&index.canonical, task_ref).unwrap();
        let baseline = index.baseline.clone();
        // A tracked file is modified, another is deleted, an untracked file is
        // added and a clean file becomes executable (mode-only).
        std::fs::write(root.join("kept.txt"), b"changed\n").unwrap();
        std::fs::write(root.join("added.txt"), b"new file\n").unwrap();
        std::fs::set_permissions(
            root.join("kept.txt"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let changes = complete_result_changes(&root, &facts, &baseline).unwrap();
        // Only the paths that actually moved appear, sorted by path.
        let paths: Vec<&str> = changes.iter().map(|change| change.path.as_str()).collect();
        let mut sorted = paths.clone();
        sorted.sort_unstable();
        assert_eq!(paths, sorted);
        let by_path = |path: &str| changes.iter().find(|change| change.path == path).cloned();
        let kept = by_path("kept.txt").unwrap();
        assert_eq!(kept.kind, "modify");
        // The before side is the verified dispatch-index bytes.
        assert_eq!(
            kept.before.as_ref().unwrap().sha256,
            format!("sha256:{}", crate::jcs::sha256_hex(b"clean\n"))
        );
        assert_eq!(
            kept.after.as_ref().unwrap().sha256,
            format!("sha256:{}", crate::jcs::sha256_hex(b"changed\n"))
        );
        assert_eq!(kept.before_mode, Some(0o644));
        assert_eq!(kept.after_mode, Some(0o755));
        let added = by_path("added.txt").unwrap();
        assert_eq!(added.kind, "add");
        assert!(added.before.is_none());
        assert!(added.before_mode.is_none());
        // Every change reference is a valid closed object.
        for change in changes.iter() {
            assert!(matches!(
                change.kind.as_str(),
                "add" | "delete" | "modify" | "rename"
            ));
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Part A: delete, 0644 -> 0600 mode-only, and ordering across kinds.
    #[test]
    fn the_producer_reports_deletes_mode_only_changes_and_orders_kinds() {
        let root = workspace();
        // Add a second tracked file so a delete can be observed alongside an add.
        std::fs::write(root.join("removed.txt"), b"clean\n").unwrap();
        assert!(
            std::process::Command::new("/usr/bin/git")
                .arg("-C")
                .arg(&root)
                .args(["-c", "user.email=test@example.invalid"])
                .args(["-c", "user.name=Dolgorae Test"])
                .args(["add", "removed.txt"])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            std::process::Command::new("/usr/bin/git")
                .arg("-C")
                .arg(&root)
                .args(["-c", "user.email=test@example.invalid"])
                .args(["-c", "user.name=Dolgorae Test"])
                .args(["commit", "-q", "-m", "second"])
                .status()
                .unwrap()
                .success()
        );
        let task_ref = Uuid::now_v7();
        let index = capture_dispatch_index(&root, task_ref).unwrap();
        let facts = parse_dispatch_index(&index.canonical, task_ref).unwrap();
        let baseline = index.baseline.clone();
        std::fs::remove_file(root.join("removed.txt")).unwrap();
        std::fs::write(root.join("added.txt"), b"added\n").unwrap();
        std::fs::set_permissions(
            root.join("kept.txt"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let changes = complete_result_changes(&root, &facts, &baseline).unwrap();
        let kinds: Vec<(&str, &str)> = changes
            .iter()
            .map(|change| (change.path.as_str(), change.kind.as_str()))
            .collect();
        // Sorted by path across kinds, never a rename.
        assert_eq!(
            kinds,
            vec![
                ("added.txt", "add"),
                ("kept.txt", "modify"),
                ("removed.txt", "delete")
            ]
        );
        assert!(changes.iter().all(|change| change.old_path.is_none()));
        let removed = changes
            .iter()
            .find(|change| change.path == "removed.txt")
            .unwrap();
        assert!(removed.after.is_none() && removed.after_mode.is_none());
        assert_eq!(
            removed.before.as_ref().unwrap().sha256,
            format!("sha256:{}", crate::jcs::sha256_hex(b"clean\n"))
        );
        let before = changes
            .retained
            .iter()
            .filter(|side| side.side == "before")
            .collect::<Vec<_>>();
        assert_eq!(before.len(), 2);
        assert!(before.iter().all(|side| side.bytes == b"clean\n"));
        // A 0644 -> 0600 change is content-identical: only the modes differ.
        let kept = changes
            .iter()
            .find(|change| change.path == "kept.txt")
            .unwrap();
        assert_eq!(kept.before.as_ref().unwrap(), kept.after.as_ref().unwrap());
        assert_eq!(kept.before_mode, Some(0o644));
        assert_eq!(kept.after_mode, Some(0o600));
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Part A: a rename is emitted as delete + add, never as a rename.
    #[test]
    fn a_rename_is_emitted_as_delete_and_add() {
        let root = workspace();
        let task_ref = Uuid::now_v7();
        let index = capture_dispatch_index(&root, task_ref).unwrap();
        let facts = parse_dispatch_index(&index.canonical, task_ref).unwrap();
        let baseline = index.baseline.clone();
        std::fs::rename(root.join("kept.txt"), root.join("moved.txt")).unwrap();
        let changes = complete_result_changes(&root, &facts, &baseline).unwrap();
        let kinds: Vec<(&str, &str)> = changes
            .iter()
            .map(|change| (change.path.as_str(), change.kind.as_str()))
            .collect();
        assert_eq!(kinds, vec![("kept.txt", "delete"), ("moved.txt", "add")]);
        assert!(changes.iter().all(|change| change.old_path.is_none()));
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Part A: a before image that cannot be established is a typed refusal, never
    /// fabricated bytes.
    #[test]
    fn an_unverifiable_before_image_is_refused() {
        let root = workspace();
        let task_ref = Uuid::now_v7();
        let index = capture_dispatch_index(&root, task_ref).unwrap();
        let mut facts = parse_dispatch_index(&index.canonical, task_ref).unwrap();
        let baseline = index.baseline.clone();
        // The index recorded a digest that the recorded commit cannot reproduce.
        for entry in &mut facts.entries {
            if entry.path == "kept.txt" {
                entry.sha256 = "0".repeat(64);
            }
        }
        std::fs::write(root.join("kept.txt"), b"changed\n").unwrap();
        assert!(complete_result_changes(&root, &facts, &baseline).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Part A: a side over the 8 MiB bound is a typed refusal.
    #[test]
    fn an_oversized_change_side_is_refused() {
        let root = workspace();
        let task_ref = Uuid::now_v7();
        let index = capture_dispatch_index(&root, task_ref).unwrap();
        let facts = parse_dispatch_index(&index.canonical, task_ref).unwrap();
        let baseline = index.baseline.clone();
        std::fs::write(
            root.join("kept.txt"),
            vec![b'x'; (MAX_FILE_BYTES + 1) as usize],
        )
        .unwrap();
        assert!(complete_result_changes(&root, &facts, &baseline).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_oversized_domain_or_side_is_refused_by_the_existing_bounds() {
        let root = workspace();
        // A file beyond the per-side bound refuses the whole index; the
        // existing bounded worktree reader is the enforcement point.
        std::fs::write(
            root.join("huge.bin"),
            vec![0u8; (MAX_FILE_BYTES + 1) as usize],
        )
        .unwrap();
        assert_eq!(
            capture_dispatch_index(&root, Uuid::now_v7())
                .unwrap_err()
                .code,
            "REVIEW_TARGET_LIMIT_EXCEEDED"
        );
        std::fs::remove_file(root.join("huge.bin")).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
