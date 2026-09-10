# Dolgorae Configuration

Read this reference only when creating, inspecting, removing, or replacing a
global Codex Profile or Specialist Policy. Configuration is machine-local under
the fixed Dolgorae home. Use the public CLI rather than editing registry files
directly.

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

## Configure a Specialist Policy

1. Inspect the selected global Profiles and the explicit common or project Role
   sources. A source defines character and instructions only; the policy input
   separately supplies Profile, model, lane, access, approval, reuse, and
   lifecycle controls. Never infer scope when the same name exists in both
   locations.
2. Start each selected Profile Server through an independently authorized
   lifecycle operation. Then treat `validate` as read-only and run it against
   the intended canonical workspace:

   ```sh
   dolgorae specialist policy validate \
     --workspace <path> \
     --file <policy-input.json>
   ```

   The result is the complete immutable installed-policy candidate. Validation
   observes the already-running server and never starts, repairs, or records a
   Profile binding. It does not authorize later installation and cannot be
   reused if a source, policy input, or Profile binding changes.
3. Add only with explicit authorization to install the project-authored
   instructions:

   ```sh
   dolgorae specialist policy add <name> \
     --workspace <path> \
     --file <policy-input.json>
   dolgorae specialist policy show <name> --workspace <path>
   ```

   The requested name must equal `policy_name`. Installation is
   create-exclusive; an existing name is never replaced implicitly.
4. `list` and `show` are read-only. `remove` is a distinct authorized mutation.
   Removing an entry affects future launches only and never rewrites an existing
   Orchestrated Session snapshot. Replacement requires an authorized remove and
   a separately validated add.

Never edit common or project Role files as part of a policy operation unless
the user separately authorized that source change. Never edit the protected
installed registry or orchestration state directly.

## Configuration boundaries

- `profile add`, `profile remove`, `specialist policy add`, and `specialist
  policy remove` are mutations and require explicit intent.
- Bare `profile doctor`, profile list/show, and policy validate/list/show are
  read-only. Policy validation requires an already-running compatible Profile
  Server; a launch probe is a separate lifecycle operation and is not read-only.
- Do not expose `CODEX_HOME` contents, authentication state, environment values,
  executable digests, or diagnostic records beyond what the user needs.
- After a mutation, repeat the corresponding list/show operation and report the
  exact Profile identity that changed.
