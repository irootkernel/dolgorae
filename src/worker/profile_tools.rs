use super::{
    DedicatedServerIdentity, StartupOwnedChild, server_census_matches, startup_ancestries_match,
    startup_ancestry,
};
use crate::darwin::{DarwinSystem, PipeEndpoint, ProcessCensusEntry};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub(super) struct ToolProcesses {
    groups_by_pid: BTreeMap<u32, u32>,
}

impl ToolProcesses {
    pub(super) fn sample(
        server: &DedicatedServerIdentity,
        census: &[ProcessCensusEntry],
        retained: Option<&[StartupOwnedChild]>,
    ) -> Self {
        let system = DarwinSystem;
        let Some(leader) = census
            .iter()
            .find(|entry| server_census_matches(entry, server))
        else {
            return Self::default();
        };
        let mut roots = census
            .iter()
            .filter(|entry| {
                entry.parent_pid == server.pid
                    && entry.pid == entry.process_group_id
                    && entry.session_id == server.session_id
                    && entry.uid == server.uid
                    && entry.birth.is_some()
                    && !entry.zombie
            })
            .peekable();
        if roots.peek().is_none() {
            return Self::default();
        }
        let Some(pipes) = system.pipe_endpoints(server.pid) else {
            return Self::default();
        };
        let groups: BTreeSet<_> = roots
            .filter(|root| {
                let descriptors = || {
                    Some([
                        system.pipe_endpoint(root.pid, 0)?,
                        system.pipe_endpoint(root.pid, 1)?,
                        system.pipe_endpoint(root.pid, 2)?,
                    ])
                };
                descriptors().is_some_and(|first| {
                    standard_transport(first, &pipes)
                        && descriptors() == Some(first)
                        && live_identity_matches(root)
                })
            })
            .map(|root| root.pid)
            .collect();
        if groups.is_empty() || !live_identity_matches(leader) {
            return Self::default();
        }
        let groups_by_pid = census
            .iter()
            .filter(|entry| !entry.zombie && groups.contains(&entry.process_group_id))
            .filter_map(|entry| {
                let ancestry = startup_ancestry(census, entry, server)?;
                let root = &ancestry[ancestry.len() - 2];
                if root.pid != entry.process_group_id
                    || ancestry[..ancestry.len() - 1].iter().any(|member| {
                        member.process_group_id != root.pid
                            || member.session_id != server.session_id
                    })
                    || retained.is_some_and(|children| {
                        !children
                            .iter()
                            .any(|child| startup_ancestries_match(&child.ancestry, &ancestry))
                    })
                {
                    return None;
                }
                Some((entry.pid, root.pid))
            })
            .collect();
        Self { groups_by_pid }
    }

    pub(super) fn contains(&self, pid: u32) -> bool {
        self.groups_by_pid.contains_key(&pid)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.groups_by_pid.is_empty()
    }

    pub(super) fn startup_ps(
        &self,
        process: &ProcessCensusEntry,
        server: &DedicatedServerIdentity,
    ) -> bool {
        self.has_tool_parent(process, server) && DarwinSystem.startup_setuid_ps(process, server.uid)
    }

    fn has_tool_parent(
        &self,
        process: &ProcessCensusEntry,
        server: &DedicatedServerIdentity,
    ) -> bool {
        self.groups_by_pid.get(&process.parent_pid) == Some(&process.process_group_id)
            && process.session_id == server.session_id
    }
}

pub(super) struct PolicySample<'a> {
    pub retained: &'a [StartupOwnedChild],
    pub dirty: bool,
}

impl PolicySample<'_> {
    pub fn defer_member(
        &mut self,
        tools: &ToolProcesses,
        process: &ProcessCensusEntry,
        server: &DedicatedServerIdentity,
    ) -> bool {
        let retained = self
            .retained
            .iter()
            .find(|child| child.identity.pid == process.pid);
        let transient = match retained {
            None => {
                (process.uid == server.uid && process.birth.is_some())
                    || tools.startup_ps(process, server)
            }
            Some(child) => {
                let prior = &child.identity;
                prior.parent_pid == process.parent_pid
                    && prior.process_group_id == process.process_group_id
                    && prior.session_id == process.session_id
                    && tools.startup_ps(process, server)
            }
        };
        let defer = transient && tools.has_tool_parent(process, server);
        self.dirty |= defer;
        defer
    }

    pub fn defer_membership_change(
        &mut self,
        tools: &ToolProcesses,
        server: &DedicatedServerIdentity,
        group: u32,
        memberships: (&[u32], &[u32]),
        census: &[ProcessCensusEntry],
    ) -> bool {
        if !tools.contains(group) {
            return false;
        }
        let (listed, enumerated) = memberships;
        let only_transients = listed
            .iter()
            .chain(enumerated)
            .filter(|pid| {
                listed.binary_search(pid).is_ok() != enumerated.binary_search(pid).is_ok()
            })
            .all(|pid| {
                let observed = census
                    .iter()
                    .find(|entry| entry.pid == *pid)
                    .copied()
                    .map_or_else(|| DarwinSystem.process_census_identity(*pid), Ok);
                match observed {
                    Ok(entry) => {
                        entry.process_group_id == group && self.defer_member(tools, &entry, server)
                    }
                    Err(error) => error.raw_os_error() == Some(libc::ESRCH),
                }
            });
        self.dirty |= only_transients;
        only_transients
    }

    pub fn defer_server_group_membership_change(
        &mut self,
        tools: &ToolProcesses,
        server: &DedicatedServerIdentity,
        memberships: (&[u32], &[u32]),
        census: &[ProcessCensusEntry],
    ) -> bool {
        let (listed, enumerated) = memberships;
        let only_transients = listed
            .iter()
            .chain(enumerated)
            .filter(|pid| {
                listed.binary_search(pid).is_ok() != enumerated.binary_search(pid).is_ok()
            })
            .all(|pid| {
                if *pid == server.pid {
                    return false;
                }
                let observed = census
                    .iter()
                    .find(|entry| entry.pid == *pid)
                    .copied()
                    .map_or_else(|| DarwinSystem.process_census_identity(*pid), Ok);
                match observed {
                    Ok(entry) => {
                        entry.process_group_id == server.process_group_id
                            && entry.session_id == server.session_id
                            && (self.defer_member(tools, &entry, server)
                                || (entry.parent_pid == server.pid
                                    && DarwinSystem.startup_setuid_ps(&entry, server.uid)))
                    }
                    Err(error) => error.raw_os_error() == Some(libc::ESRCH) && !tools.is_empty(),
                }
            });
        self.dirty |= only_transients;
        only_transients
    }
}

fn standard_transport(stdio: [PipeEndpoint; 3], server_pipes: &[PipeEndpoint]) -> bool {
    stdio[0].reads
        && !stdio[1].reads
        && !stdio[2].reads
        && stdio.iter().enumerate().all(|(index, endpoint)| {
            stdio[..index]
                .iter()
                .all(|other| endpoint.handle != other.handle)
                && server_pipes.iter().any(|peer| endpoint.connects(*peer))
        })
}

fn live_identity_matches(entry: &ProcessCensusEntry) -> bool {
    DarwinSystem
        .bsd_process_identity(entry.pid)
        .is_ok_and(|live| {
            live.pid == entry.pid
                && live.uid == entry.uid
                && live.parent_pid == entry.parent_pid
                && live.process_group_id == entry.process_group_id
                && live.session_id == entry.session_id
                && Some((live.start_tvsec, live.start_tvusec)) == entry.birth
                && !live.zombie
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn darwin_reports_both_ends_of_child_stdio() {
        let mut child = Command::new("/bin/cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let result = (|| {
            let system = DarwinSystem;
            let stdio = [
                system.pipe_endpoint(child.id(), 0)?,
                system.pipe_endpoint(child.id(), 1)?,
                system.pipe_endpoint(child.id(), 2)?,
            ];
            let pipes = system.pipe_endpoints(std::process::id())?;
            Some(standard_transport(stdio, &pipes))
        })();
        child.stdin.take();
        assert!(child.wait().unwrap().success());
        assert_eq!(result, Some(true));
    }

    #[test]
    fn standard_transport_requires_distinct_reciprocal_directional_pipes() {
        let input = PipeEndpoint {
            handle: 1,
            peer: 2,
            reads: true,
        };
        let output = PipeEndpoint {
            handle: 3,
            peer: 4,
            reads: false,
        };
        let error = PipeEndpoint {
            handle: 5,
            peer: 6,
            reads: false,
        };
        let mirror = |end: PipeEndpoint| PipeEndpoint {
            handle: end.peer,
            peer: end.handle,
            reads: !end.reads,
        };
        let mut server = [mirror(error), mirror(input), mirror(output)];
        assert!(standard_transport([input, output, error], &server));
        assert!(!standard_transport([input, output, output], &server));
        server[1].reads = true;
        assert!(!standard_transport([input, output, error], &server));
        server[1].reads = false;
        server[1].peer = 8;
        assert!(!standard_transport([input, output, error], &server));
    }

    #[test]
    fn server_group_membership_deferral_rejects_leader_drift_and_unverified_members() {
        let server = DedicatedServerIdentity {
            pid: 200,
            uid: 501,
            process_group_id: 200,
            session_id: 200,
            start_tvsec: 10,
            start_tvusec: 1,
            executable_path: std::path::PathBuf::from("/usr/bin/true"),
            executable_device: 1,
            executable_inode: 1,
            executable_sha256: "a".repeat(64),
        };
        let tools = ToolProcesses::default();
        let mut sample = PolicySample {
            retained: &[],
            dirty: false,
        };
        // Leader membership drift must never be deferred.
        assert!(!sample.defer_server_group_membership_change(&tools, &server, (&[200], &[]), &[]));
        assert!(!sample.dirty);

        // Ordinary non-tool child in server group must fail closed.
        let stray = ProcessCensusEntry {
            pid: 201,
            parent_pid: 200,
            uid: 501,
            process_group_id: 200,
            session_id: 200,
            birth: Some((10, 2)),
            zombie: false,
        };
        assert!(!sample.defer_server_group_membership_change(
            &tools,
            &server,
            (&[200], &[200, 201]),
            &[stray],
        ));
        assert!(!sample.dirty);
    }
}
