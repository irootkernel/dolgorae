"""Manifest-validated declarative scenarios for the fake app-server.

A scenario says what the fixture answers, never how the client should behave, so
one file drives both the happy path and the adversarial orderings.  The manifest
is checked before the socket is bound: a scenario that does not match the schema
is a fixture bug, and the fixture refuses to start rather than silently
answering nothing.
"""

from __future__ import annotations

import pathlib
from typing import Any

import jsonlite

MANIFEST_PATH = pathlib.Path(__file__).with_name("manifest.json")


class ScenarioError(Exception):
    """A scenario file that does not satisfy the manifest."""


def _manifest() -> dict[str, Any]:
    return jsonlite.loads(MANIFEST_PATH.read_text(encoding="utf-8"))


def _fail(path: str, reason: str) -> None:
    raise ScenarioError(f"{path}: {reason}")


def _bound(schema: dict[str, Any], name: str, default: int | None = None) -> int | None:
    """Read a numeric schema constraint, which this reader parses as a lexeme."""
    if name not in schema:
        return default
    return int(schema[name])


def _validate(value: Any, schema: dict[str, Any], root: dict[str, Any], path: str) -> None:
    if "$ref" in schema:
        target = root
        for part in schema["$ref"].removeprefix("#/").split("/"):
            target = target[part]
        _validate(value, target, root, path)
        return
    if "const" in schema and value != schema["const"]:
        _fail(path, f"expected {schema['const']!r}")
    if "enum" in schema and value not in schema["enum"]:
        _fail(path, f"expected one of {schema['enum']!r}")
    kind = schema.get("type")
    if kind == "object":
        if not isinstance(value, dict):
            _fail(path, "expected an object")
        for required in schema.get("required", []):
            if required not in value:
                _fail(path, f"missing required member {required!r}")
        properties = schema.get("properties", {})
        if schema.get("additionalProperties") is False:
            for name in value:
                if name not in properties:
                    _fail(path, f"unknown member {name!r}")
        for name, member in value.items():
            if name in properties:
                _validate(member, properties[name], root, f"{path}.{name}")
    elif kind == "array":
        if not isinstance(value, list):
            _fail(path, "expected an array")
        minimum = _bound(schema, "minItems", 0) or 0
        if len(value) < minimum:
            _fail(path, f"expected at least {minimum} items")
        for index, element in enumerate(value):
            if "items" in schema:
                _validate(element, schema["items"], root, f"{path}[{index}]")
    elif kind == "string":
        if not isinstance(value, str):
            _fail(path, "expected a string")
        if len(value) < (_bound(schema, "minLength", 0) or 0):
            _fail(path, "string is too short")
        maximum = _bound(schema, "maxLength")
        if maximum is not None and len(value) > maximum:
            _fail(path, "string is too long")
    elif kind == "integer":
        if not isinstance(value, jsonlite.Number):
            _fail(path, "expected an integer")
        try:
            number = int(value)
        except jsonlite.JsonError as error:
            _fail(path, str(error))
        minimum = _bound(schema, "minimum")
        if minimum is not None and number < minimum:
            _fail(path, f"expected at least {minimum}")
    elif kind == "boolean" and not isinstance(value, bool):
        _fail(path, "expected a boolean")


class Scenario:
    """One validated scenario, addressable by inbound method name."""

    def __init__(self, document: dict[str, Any], overrides: dict[str, str]) -> None:
        self.name: str = document["name"]
        self.fragment_bytes = document.get("fragment_bytes")
        if self.fragment_bytes is not None:
            self.fragment_bytes = int(self.fragment_bytes)
        self.bindings = dict(overrides)
        self.bindings.setdefault("codex_home", document.get("codex_home", "/tmp/codex-home"))
        self.steps: list[dict[str, Any]] = document["steps"]
        self.counts: dict[str, int] = {}

    @classmethod
    def load(cls, path: pathlib.Path, overrides: dict[str, str] | None = None) -> "Scenario":
        document = jsonlite.loads(path.read_text(encoding="utf-8"))
        manifest = _manifest()
        _validate(document, manifest, manifest, path.name)
        return cls(document, overrides or {})

    def bind(self, name: str, value: str) -> None:
        self.bindings[name] = value

    def match(self, method: str) -> dict[str, Any] | None:
        """Select the step for this call, honouring per-method occurrence."""
        seen = self.counts.get(method, 0) + 1
        self.counts[method] = seen
        fallback = None
        for step in self.steps:
            if step["method"] != method:
                continue
            occurrence = step.get("occurrence")
            if occurrence is None:
                if fallback is None:
                    fallback = step
                continue
            if int(occurrence) == seen:
                return step
        return fallback

    def substitute(self, value: Any) -> Any:
        """Replace `${name}` placeholders with the scenario's live bindings."""
        if isinstance(value, str):
            for name, binding in self.bindings.items():
                value = value.replace("${" + name + "}", binding)
            return value
        if isinstance(value, list):
            return [self.substitute(element) for element in value]
        if isinstance(value, dict):
            return {name: self.substitute(member) for name, member in value.items()}
        return value


def generate_thread_read(specification: dict[str, Any], bindings: dict[str, str]) -> dict[str, Any]:
    """Build a `thread/read` result far larger than any single message bound.

    The wanted Turn is placed last, behind decoy Turns, so a reader that keeps
    the whole reply in memory is distinguishable from one that streams and
    discards.
    """
    decoys = int(specification.get("decoy_turns", 0) or 0)
    filler = int(specification.get("decoy_filler_bytes", 0) or 0)
    target_bytes = int(specification.get("target_text_bytes", 0) or 0)
    thread_id = bindings.get("thread_id", "thread-1")
    turn_id = bindings.get("turn_id", "turn-1")
    turns: list[dict[str, Any]] = []
    for index in range(decoys):
        turns.append(
            {
                "id": f"decoy-turn-{index}",
                "status": "completed",
                "items": [
                    {
                        "type": "agentMessage",
                        "phase": "final_answer",
                        "threadId": thread_id,
                        "turnId": f"decoy-turn-{index}",
                        "status": "completed",
                        "text": "d" * filler,
                    }
                ],
            }
        )
    turns.append(
        {
            "id": turn_id,
            "status": "completed",
            "items": [
                {
                    "type": "agentMessage",
                    "phase": "final_answer",
                    "threadId": thread_id,
                    "turnId": turn_id,
                    "status": "completed",
                    "text": "t" * target_bytes,
                }
            ],
        }
    )
    if specification.get("nest_under_thread", True):
        return {"thread": {"id": thread_id, "turns": turns}}
    return {"turns": turns}
