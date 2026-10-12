#!/usr/bin/env python3
"""Bind H1's portable artifacts after generation and verification."""

import hashlib
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PROTOCOL = ROOT / "docs/protocol"


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def inventory():
    paths = {PROTOCOL / name for name in [
        "dolgorae-capabilities-v2.json", "dolgorae-grpc-client-policy-v2.json",
        "dolgorae-grpc-error-mapping-v2.json", "dolgorae-gul-consumer-v2.json",
        "dolgorae-rpc-mutation-policy-v2.json",
    ]}
    paths.update(PROTOCOL.glob("*v2.fixtures.json"))
    paths.update(PROTOCOL.glob("dolgorae-public-v2.descriptor.*"))
    paths.update(PROTOCOL.glob("*v5.schema.json"))
    paths.update(PROTOCOL.glob("*v014*.json"))
    paths.add(PROTOCOL / "dolgorae-successor-state-v1.schema.json")
    paths.update(p for p in (PROTOCOL / "generated/gul-consumer-v2/go").rglob("*") if p.is_file())
    paths.add(PROTOCOL / "dolgorae/public/v2/dolgorae.proto")
    paths.update(p for p in (PROTOCOL / "examples").glob("*.json") if "v014" in p.name or "v5" in p.name)
    paths.update(ROOT / p for p in [
        "docs/specs/dolgorae-v014-successor-requirements.md",
        "docs/specs/dolgorae-v014-method-and-digest-contract.md",
        "docs/specs/dolgorae-consumer-acceptance-v014.md",
        "docs/specs/dolgorae-v014-contract-coordination.md",
        "docs/specs/dolgorae-v014-host-contract-qualification.md",
        "docs/specs/dolgorae-v014-h1-package.md",
        "examples/host_invocation_contract.rs",
        "tests/e2e/run_native_host_invocation_fixture.py",
        "tests/e2e/host_invocation_fault_proxy.py",
        "tests/successor_contract.rs",
        "src/jcs.rs",
        "Cargo.toml",
        "Cargo.lock",
        "Makefile",
        "tools/validation/requirements.txt",
        "tools/validators/validate_schema_examples.py",
        "tools/validators/generate_public_v2.py",
        "tools/validators/generate_h1_policies.py",
        "tools/validators/generate_h1_lock.py",
        "tools/validators/validate_h1_contract.py",
        "tools/validators/package_h1.py",
    ])
    # Include every checked schema reached by the selected successor shapes.
    registry = {}
    for path in PROTOCOL.glob("*.json"):
        for schema_id in re.findall(r'"\$id":\s*"(https://dolgorae.local/[^"#]+)', path.read_text()):
            registry[schema_id] = path
    pending = list(paths)
    while pending:
        path = pending.pop()
        if path.suffix != ".json" or not path.is_file():
            continue
        text = path.read_text()
        for ref in re.findall(r'"\$ref":\s*"(https://dolgorae.local/[^"#]+)', text):
            dependency = registry[ref]
            if dependency not in paths:
                paths.add(dependency)
                pending.append(dependency)
    return sorted(paths)


def main():
    coordination = (ROOT / "docs/specs/dolgorae-v014-contract-coordination.md").read_text()
    revisions = dict(re.findall(r"\| (Aquarium|Podway|Gul) \| `([a-f0-9]{40})`", coordination))
    sources = [{"consumer": owner, "revision": revisions[owner], "path": path, "sha256": digest}
               for owner, path, digest in re.findall(r"\| (Aquarium|Podway|Gul): `([^`]+)` \| `([a-f0-9]{64})`", coordination)]
    assert len(sources) == 11
    frozen = json.loads((PROTOCOL / "dolgorae-gul-consumer-v1.lock.json").read_text())
    frozen_artifacts = [{"path": "docs/protocol/" + item["path"], "sha256": item["sha256"]}
                        for item in frozen["artifacts"]]
    frozen_artifacts.append({"path": "docs/protocol/dolgorae-gul-consumer-v1.lock.json", "sha256": sha(PROTOCOL / "dolgorae-gul-consumer-v1.lock.json")})
    for item in frozen_artifacts:
        assert sha(ROOT / item["path"]) == item["sha256"], item["path"]
    artifacts = []
    for path in inventory():
        generation = "H1-contract-input"
        if path.suffix == ".json":
            value = json.loads(path.read_text())
            generation = value.get("$id", value.get("contract_id", value.get("package", generation)))
        artifacts.append({"path": str(path.relative_to(ROOT)), "sha256": sha(path), "generation": generation})
    state = json.loads((PROTOCOL / "dolgorae-successor-state-v1.schema.json").read_text())
    value = {
        "contract_id": "dolgorae.h1-contract-lock/v1", "schema_version": 1,
        "producer": {"epic": "EPIC-018", "task": "TASK-068", "requirements_commit": "063d96c97f633cc6cbb611319c19da5a4fbacb82"},
        "package_generation": "H1/public-v2/gul-consumer-v2/machine-v5/facade-v5",
        "identity": "raw SHA-256 of this lock plus every enumerated artifact; containing completion commit supplies Git revision",
        "toolchain": {"rust": "1.97.1", "buf": "1.69.0", "go": "1.26.6", "protoc": "36.2", "protoc_gen_go": "1.36.11", "protoc_gen_go_grpc": "1.6.1"},
        "generation": {"public": "python3 tools/validators/generate_public_v2.py --write", "policies": "python3 tools/validators/generate_h1_policies.py", "lock": "python3 tools/validators/generate_h1_lock.py", "verify": "make PYTHON_BIN=.venv/bin/python test"},
        "persisted_generations": state["x-generationSelection"],
        "artifacts": artifacts, "frozen_v1_artifacts": frozen_artifacts,
        "selected_external_sources": sources,
        "external_selection": {"authority": "Master-approved source and responsibility decision", "consumer_source_change_identified": False, "prepared_podway_is_released_support": False,
            "provider_implementation_owners": ["TASK-076", "TASK-077", "TASK-086"], "independent_final_acceptance": "TASK-088", "release_binding": "TASK-089"},
        "native_feasibility": {"qualification": "docs/specs/dolgorae-v014-host-contract-qualification.md", "scope": "account-free actual native host contract prototype, not production facade/consumer acceptance", "hosts": [
            {"product": "Codex CLI", "version": "0.160.0", "sha256": "112fae7a5a1223e673c8a1791d32338f37df8b527ff1159bb8adac6c4dbf1b4b"},
            {"product": "Claude Code", "version": "2.1.295", "sha256": "0116ee2e0a513900b633d9951367f18747686478e2b462805b8c31609f047f70"}],
            "tested_prototype_sha256": "d95cee443673088a75e57ba4ef67c0c245a435bc065ad0fb4473325d2f94399c", "historical_0_149_mcp_unavailable": "unchanged"},
        "later_handoffs": {"H2": {"producer": "Gul E15 including E15-T21 and whole-Epic closeout", "consumer": "TASK-069/EPIC-019", "status": "not_started"},
            "H3": {"producer": "TASK-087/EPIC-021", "consumer": "Gul E16", "status": "future_runtime_qualification"},
            "H4": {"producer": "Gul E16 including E16-T16 and whole-Epic closeout", "consumer": "TASK-088/089", "status": "future_final_campaign"}},
        "runtime_availability": "unavailable_until_explicit_negotiated_implementation", "release_acceptance": False}
    (PROTOCOL / "dolgorae-gul-consumer-v2.lock.json").write_text(json.dumps(value, indent=2) + "\n")


if __name__ == "__main__":
    main()
