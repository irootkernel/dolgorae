# Dolgorae Configuration

Read this reference only when creating, inspecting, removing, or replacing a
global Codex Profile. Configuration is user-global and machine-local under the
fixed Dolgorae home. Use the public CLI rather than editing registry files directly.

## Create a global Codex Profile

1. Inspect existing global names:

   ```sh
   dolgorae profile list
   ```

2. Obtain the intended profile name, canonical absolute `CODEX_HOME`, direct
   native Codex executable, allowed global arguments, and explicit non-secret
   environment. `argv[0]` must be an absolute executable path whose basename
   and canonical target are both `codex`. The target must be a regular native
   executable with an executable bit, not a script, setuid image, or setgid
   image. Do not infer an account home or authentication profile from the
   current shell.
3. Require `PATH`, `LANG`, and `LC_ALL`. Every `PATH` component must be an
   existing absolute directory; empty, relative, `.`, and duplicate components
   are invalid. Do not store credentials or secrets in profile environment.
   `CODEX_HOME`, `HOME`, `USER`, `LOGNAME`, `SHELL`, `TMPDIR`, and every
   `DOLGORAE_*` name are reserved for Dolgorae.
4. Add the profile with its complete launch contract:

   ```sh
   dolgorae profile add <name> \
     --codex-home <absolute-codex-home> \
     --native-subagents enabled \
     --env PATH=<absolute-search-path> \
     --env LANG=<locale> \
     --env LC_ALL=<locale> \
     -- <absolute-native-codex-executable> [allowed-global-arguments...]
   ```

   The arguments after `--` belong to the Codex launch command. V1 permits only
   `--profile <name>`, repeatable `--enable <feature>`, repeatable `--disable
   <feature>`, and flag-only `--strict-config`. Do not use aliases,
   `--flag=value`, or any other option, and omit global arguments unless the
   requested profile requires them. The `multi_agent` feature is reserved to
   Dolgorae and must not appear in raw profile arguments. Do not copy shell
   wrappers, aliases, virtual-environment state, or incidental environment into
   the profile.
5. Read the returned profile identity, then run the offline check:

   ```sh
   dolgorae profile show <name>
   dolgorae profile doctor <name>
   ```

   Inspect `data.compatibility` and every diagnostic even when the envelope is
   successful. Add `--launch-probe` only with explicit authorization for a live
   Codex compatibility probe. The probe may temporarily start a Profile Server;
   `--leave-running` is a separate explicit decision.

Profile names are create-exclusive. `PROFILE_ALREADY_EXISTS` is not permission
to replace one. Replacement requires an explicit remove and a later add, after
checking current profile membership and runtime impact. Removing a definition
does not rewrite immutable snapshots already stored by existing Runs.

## Specialist Policy availability

The Specialist Policy Registry is owned by planned roadmap task `TASK-024` and
is not implemented in this release. The CLI parses future `specialist policy`
grammar, but every such operation reaches the unavailable-command path. Parsed
grammar is not runtime capability evidence.

If a Specialist Policy operation is requested, inspect current capabilities and
the owning roadmap task, report that the workflow is unavailable, and stop. Do
not invoke `add`, `list`, `show`, `validate`, or `remove`, and do not edit a
registry or policy snapshot directly.

## Configuration boundaries

- `profile add` and `profile remove` are mutations and require explicit intent.
- Bare `profile doctor` and profile list/show are read-only. A launch probe is
  not read-only.
- Do not expose `CODEX_HOME` contents, authentication state, environment values,
  executable digests, or diagnostic records beyond what the user needs.
- After a mutation, repeat the corresponding list/show operation and report the
  exact Profile identity that changed.
