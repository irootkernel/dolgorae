#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PYTHON_BIN="${PYTHON_BIN:-python3}"
if [[ "$PYTHON_BIN" == */* && "$PYTHON_BIN" != /* ]]; then
  PYTHON_BIN="$ROOT/$PYTHON_BIN"
fi
DOLGORAE_BIN="${DOLGORAE_BIN:-$ROOT/target/debug/dolgorae}"

cd "$ROOT"

"$PYTHON_BIN" - <<'PY'
try:
    import jsonschema  # noqa: F401
    import referencing  # noqa: F401
except ImportError as exc:
    raise SystemExit(
        "Missing Python dependencies. Create .venv and install "
        "tools/validation/requirements.txt."
    ) from exc
PY

if ! command -v buf >/dev/null 2>&1; then
  echo "Missing validation dependency: buf 1.66.1 is required." >&2
  exit 1
fi
if [[ "$(buf --version)" != "1.66.1" ]]; then
  echo "Validation dependency mismatch: expected buf 1.66.1, got $(buf --version)." >&2
  exit 1
fi

cargo fmt --all --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
cargo build --locked --bin dolgorae

buf lint docs/protocol
buf build docs/protocol >/dev/null

"$PYTHON_BIN" tools/validators/validate_json_schemas.py
"$PYTHON_BIN" tools/validators/validate_schema_examples.py
"$PYTHON_BIN" tools/validators/validate_markdown.py
"$PYTHON_BIN" tests/e2e/test_machine_cli.py --binary "$DOLGORAE_BIN"
"$PYTHON_BIN" tests/e2e/test_workspace_cli.py --binary "$DOLGORAE_BIN"

git diff --check
echo "Dolgorae checks passed."
