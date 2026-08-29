# EPIC-012: Development Aquarium Producer

Roadmap Epic: [`EPIC-012`](../roadmap/README.md#epic-012-development-aquarium-producer)

This temporary dossier owns implementation detail while EPIC-012 is active.
The canonical roadmap remains the sole authority for identity, dependency,
status, and lifecycle.

## Goal and boundary

Produce Dolgorae as one exact non-MCP executable generation for Aquarium's
isolated development channel. Dolgorae owns descriptor and build bytes;
Aquarium TASK-024 owns enrollment, publication, resolution, leasing, and
isolated runtime configuration. TASK-016 consumes the resulting exact
generation evidence but does not own producer implementation.

No work in this Epic pushes, releases, installs, authenticates, changes the
stable Codex home, or mutates the Aquarium repository.

TASK-035 may change only producer documentation, Make targets, producer support
code, and its tests. Runtime Rust sources, Cargo dependency declarations, and
`Cargo.lock` stay byte-identical to completed TASK-015. The built artifact must
also return the same runtime capability digest recorded by TASK-015. If any of
those inputs must change, TASK-035 stops until TASK-015 acceptance is rerun and
the roadmap authority is revised explicitly.

## Producer contract

`make aquarium-dev-describe` emits exactly one compact UTF-8 JSON object followed
by one newline and no other stdout bytes. Its closed field set is `schema`,
`project_id`, `next_version`, `artifact_kind`, and `artifact_path`; their values
are `aquarium-dev-producer-description/v1`, `dolgorae`, `v0.1.0`, `executable`,
and `bin/dolgorae`. This follows the canonical Aquarium development-channel
contract admitted by Aquarium TASK-024.

`make aquarium-dev-build AQUARIUM_DEV_OUTPUT=<absolute-empty-directory>`:

- physically canonicalizes the repository and output root, rejects a symlink as
  the supplied root itself, and accepts only an existing absolute empty
  canonical directory outside this repository; platform alias ancestors such
  as macOS `/var` are resolved before the retained root descriptor is opened;
- requires a clean local `main` and derives identity from its exact `HEAD`;
- exports the exact HEAD tree into a producer-owned staging subtree, verifies
  its tree identity against HEAD, and never compiles mutable checkout bytes;
- opens and retains no-follow directory descriptors for the output root,
  records its device and inode, performs relative operations through those
  descriptors, and revalidates physical path and identity before publication;
- directs both `CARGO_HOME` and Cargo's release target directory into that
  producer-owned subtree and runs `cargo build --locked --release --bin
  dolgorae` from the exported snapshot with a 30-minute deadline in a dedicated
  process group;
- copies the built executable to `bin/dolgorae` and writes `manifest.json`;
- emits no other retained entry in the staging root;
- binds schema, project ID, source Git SHA, development version
  `v0.1.0-dev.<sha12>`, artifact kind/path, and lowercase artifact SHA-256; and
- leaves the repository, index, refs, stable environment, and user credentials
  unchanged.

The manifest is also compact UTF-8 JSON plus one newline with exactly `schema`,
`project_id`, `git_sha`, `development_version`, `artifact_kind`,
`artifact_path`, and `sha256`. Values are respectively
`aquarium-dev-artifact-manifest/v1`, `dolgorae`, the full lowercase 40-hex HEAD,
`v0.1.0-dev.<first-12-git-sha>`, `executable`, `bin/dolgorae`, and `sha256:` plus
the lowercase digest of the exact executable bytes. The `next_version` comes
from the canonical Cargo package version. Its development form is a local
channel-generation identity only, may reuse the current 0.1.0 package version,
and makes no release-publication or SemVer ordering claim.

All invalid preconditions fail before compilation with exit 2. Build failure or
timeout exits 1 with one bounded stderr diagnostic. Timeout and handled signals
send TERM to the dedicated build process group, reap it after a bounded grace
interval, then send KILL to and reap every survivor. Cleanup traps are
idempotent under repeated interruption and remove the producer-owned temporary
subtree and any partial `bin` or manifest only after no descendant can write. The
completed executable and `bin` directory are fsynced before manifest
publication. The manifest is written by same-directory temporary file, fsynced,
and renamed last, then the output root is fsynced; any write, fsync, identity
revalidation, or rename failure cleans the partial result. Its durable presence
therefore marks the only complete producer result. An unhandled
interruption leaves an invalid non-empty staging root that the caller must
discard rather than retry. After compilation, any missing, non-regular,
symlinked, or non-executable artifact, identity mismatch, checksum failure, or
unexpected retained entry fails closed.

## Verification and handoff

Focused black-box tests exercise the exact descriptor bytes and every
precondition without using the user's real home. Producer internals accept an
injected subprocess runner, deadline, and publication-phase callback only in
unit tests; production commands expose none of these controls. Tests use those
controls and a fake Cargo process group to drive timeout, root replacement, and
repeated interruption at deterministic barriers without wall-clock sleeps.
Cold and warm successful builds
prove the result is independent of pre-existing user Cargo cache, and prove
exact inventory, manifest bytes and values, checksum, executable mode, source
snapshot identity, timeout cleanup, repeated interruption at each publication
phase, no surviving descendant process, and repository non-mutation. The
complete Dolgorae gate and independent review must pass before TASK-035 moves to
complete.

The independent review uses the repository's already configured Mulgae path
through `$use-mulgae`, reviews the exact TASK-035 diff, and neither launches the
new generation nor depends on Aquarium TASK-024. This keeps the review gate
independent of the downstream activation it is preparing.

The task commit SHA is the only producer handoff to Aquarium TASK-024. Aquarium
must rebuild that exact clean local-main commit into its own fresh staging root,
validate the returned manifest, publish one immutable generation, and bind
launch guards to the resolved Git SHA, development version, and artifact
checksum. A mutable `target/release/dolgorae`, moving `current` selector, or
uncommitted build is never activation evidence.

## Closeout

Promote the durable producer contract to the repository's existing development
authority, remove this dossier and its TODO index entry, replace the roadmap's
Detailed SOT link with Canonical Outcomes, and complete EPIC-012 in one approved
closeout commit. Do not archive the dossier.
