//! Noncreating Profile cleanup projections; never lifecycle admission proof.
use super::*;
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Clone, Debug, Eq, PartialEq)]
struct Identity {
    device: u64,
    inode: u64,
    mode: u32,
    uid: u32,
    links: u64,
    length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
fn identity(path: &Path) -> Result<Option<Identity>, MachineError> {
    let metadata = match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(path_error(path, e)),
        Ok(metadata) => metadata,
    };
    if metadata.is_dir() {
        verify_secure_directory(path, DarwinSystem.current_uid())?;
    } else {
        crate::workspace::verify_secure_file(path, DarwinSystem.current_uid())?;
    }
    Ok(Some(Identity {
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode(),
        uid: metadata.uid(),
        links: metadata.nlink(),
        length: metadata.len(),
        modified: (metadata.mtime(), metadata.mtime_nsec()),
        changed: (metadata.ctime(), metadata.ctime_nsec()),
    }))
}
type Sources = Vec<(PathBuf, Option<Identity>)>;
fn capture(paths: impl IntoIterator<Item = PathBuf>) -> Result<Sources, MachineError> {
    paths
        .into_iter()
        .map(|path| identity(&path).map(|value| (path, value)))
        .collect()
}
#[derive(Clone)]
struct Capture {
    root: PathBuf,
    sources: Sources,
    index: Arc<GlobalMembershipIndex>,
}
static CACHE: OnceLock<Mutex<Vec<Capture>>> = OnceLock::new();

impl GlobalMembershipStore {
    pub(crate) fn released_for_display(
        &self,
        workspace: &str,
        run_id: Uuid,
    ) -> Result<bool, MachineError> {
        let root = self.home_root.join("profiles").join(&self.server_key);
        for directory in [&self.home_root, &self.home_root.join("profiles"), &root] {
            verify_secure_directory(directory, DarwinSystem.current_uid())?;
        }
        let lock_path = root.join("server.lock");
        let lock = open_secure_file_if_present(&lock_path, DarwinSystem.current_uid())?
            .ok_or_else(|| {
                membership_incomplete(
                    &self.profile,
                    &self.server_key,
                    "membership observation lock is missing",
                )
            })?;
        DarwinSystem
            .lock_shared_nonblocking(&lock)
            .map_err(|e| path_error(&lock_path, e))?;
        let cache = CACHE.get_or_init(|| Mutex::new(Vec::new()));
        let prior = cache
            .lock()
            .ok()
            .and_then(|entries| entries.iter().find(|entry| entry.root == root).cloned());
        let index = if let Some(prior) = prior
            && capture(prior.sources.iter().map(|(path, _)| path.clone()))? == prior.sources
        {
            prior.index
        } else {
            let mut paths = vec![
                self.home_root.clone(),
                self.home_root.join("profiles"),
                root.clone(),
                root.join("server.lock"),
                root.join("members.json"),
                root.join("state.json"),
                root.join("membership-selection.json"),
                root.join("membership-generations"),
                root.join("membership.jsonl"),
            ];
            if path_present(&root.join("membership-selection.json"))? {
                let selected: MembershipManifest = membership_control(
                    &root.join("membership-selection.json"),
                    &self.profile,
                    &self.server_key,
                )?;
                let generation = root
                    .join("membership-generations")
                    .join(selected.operation_id.to_string());
                paths.push(generation.clone());
                for name in [
                    "manifest.json",
                    "checkpoint.json",
                    "activation.json",
                    "append.jsonl",
                ] {
                    paths.push(generation.join(name));
                }
            }
            let before = capture(paths)?;
            // Parsing a large checkpoint must not hold up an ordinary writer.
            // All selected sources are fenced before releasing this short pin.
            drop(lock);
            let pin = pin_membership(&root, &self.profile, &self.server_key)?;
            let records = replay_membership(&pin, &self.profile, &self.server_key)?;
            let derived = derive_index(&self.server_key, &pin.journal, &records)?;
            let persisted = open_secure_file_if_present(
                &root.join("members.json"),
                DarwinSystem.current_uid(),
            )?
            .ok_or_else(|| {
                membership_incomplete(
                    &self.profile,
                    &self.server_key,
                    "membership projection is missing",
                )
            })?;
            let persisted: GlobalMembershipIndex =
                serde_json::from_reader(persisted).map_err(|_| {
                    membership_incomplete(
                        &self.profile,
                        &self.server_key,
                        "membership projection is invalid",
                    )
                })?;
            if persisted.schema_version != derived.schema_version
                || persisted.server_key != derived.server_key
                || persisted.revision != derived.revision
                || persisted.members != derived.members
            {
                return Err(membership_incomplete(
                    &self.profile,
                    &self.server_key,
                    "membership projection disagrees with selected records",
                ));
            }
            self.verify_consistency(&root, &persisted)?;
            if capture(before.iter().map(|(path, _)| path.clone()))? != before {
                return Err(membership_incomplete(
                    &self.profile,
                    &self.server_key,
                    "membership observation changed",
                ));
            }
            let index = Arc::new(persisted);
            if let Ok(mut entries) = cache.lock() {
                entries.retain(|entry| entry.root != root);
                if entries.len() >= 8 {
                    entries.remove(0);
                }
                entries.push(Capture {
                    root,
                    sources: before,
                    index: Arc::clone(&index),
                });
            }
            index
        };
        Ok(index
            .members
            .get(&format!("{workspace}:{run_id}"))
            .is_some_and(|member| member.disposition == MembershipDisposition::Released))
    }
}
