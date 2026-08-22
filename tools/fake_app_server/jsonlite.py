"""A strict JSON reader written independently of the Rust ingest path.

ADR-014 requires the shared fake to be independent of the production parser so a
conformance test cannot certify the same mistake twice.  The standard library's
``json`` module shares the production defect this project rejects — it keeps the
last of a set of duplicate members and discards the source lexeme of a number —
so this fixture does not use it for inbound protocol text either.

The reader below rejects duplicate object members outright and keeps every
number as its original lexeme, so a scenario can assert on exactly the bytes the
client sent.
"""

from __future__ import annotations

import re

from typing import Any

__all__ = ["JsonError", "Number", "loads", "dumps"]

_NEEDS_ESCAPE = re.compile(r'["\\\x00-\x1f]')
_WHITESPACE = " \t\n\r"
_ESCAPES = {
    '"': '"',
    "\\": "\\",
    "/": "/",
    "b": "\b",
    "f": "\f",
    "n": "\n",
    "r": "\r",
    "t": "\t",
}


class JsonError(ValueError):
    """Raised for any input this reader refuses; the fixture never guesses."""


class Number:
    """A JSON number kept as its source lexeme.

    Equality and ordering go through the numeric value, so scenarios stay
    readable, while ``lexeme`` preserves what actually arrived on the wire.
    """

    __slots__ = ("lexeme",)

    def __init__(self, lexeme: str) -> None:
        self.lexeme = lexeme

    @property
    def value(self) -> float | int:
        if any(character in self.lexeme for character in ".eE"):
            return float(self.lexeme)
        return int(self.lexeme)

    def __repr__(self) -> str:
        return f"Number({self.lexeme!r})"

    def __eq__(self, other: object) -> bool:
        if isinstance(other, Number):
            return self.value == other.value
        if isinstance(other, (int, float)):
            return self.value == other
        return NotImplemented

    def __hash__(self) -> int:
        return hash(self.value)

    def __index__(self) -> int:
        value = self.value
        if not isinstance(value, int):
            raise JsonError(f"number {self.lexeme!r} is not an integer")
        return value

    def __int__(self) -> int:
        return self.__index__()


class _Reader:
    def __init__(self, text: str) -> None:
        self.text = text
        self.index = 0
        self.depth = 0

    def error(self, reason: str) -> JsonError:
        return JsonError(f"{reason} at byte {self.index}")

    def skip_whitespace(self) -> None:
        while self.index < len(self.text) and self.text[self.index] in _WHITESPACE:
            self.index += 1

    def peek(self) -> str:
        if self.index >= len(self.text):
            raise self.error("unexpected end of input")
        return self.text[self.index]

    def take(self, expected: str) -> None:
        if not self.text.startswith(expected, self.index):
            raise self.error(f"expected {expected!r}")
        self.index += len(expected)

    def read_value(self) -> Any:
        self.skip_whitespace()
        character = self.peek()
        if character == "{":
            return self.read_object()
        if character == "[":
            return self.read_array()
        if character == '"':
            return self.read_string()
        if character == "t":
            self.take("true")
            return True
        if character == "f":
            self.take("false")
            return False
        if character == "n":
            self.take("null")
            return None
        return self.read_number()

    def enter(self) -> None:
        self.depth += 1
        if self.depth > 128:
            raise self.error("nesting is too deep")

    def read_object(self) -> dict[str, Any]:
        self.enter()
        self.take("{")
        members: dict[str, Any] = {}
        self.skip_whitespace()
        if self.peek() == "}":
            self.index += 1
            self.depth -= 1
            return members
        while True:
            self.skip_whitespace()
            name = self.read_string()
            if name in members:
                raise self.error(f"duplicate member {name!r}")
            self.skip_whitespace()
            self.take(":")
            members[name] = self.read_value()
            self.skip_whitespace()
            character = self.peek()
            self.index += 1
            if character == "}":
                self.depth -= 1
                return members
            if character != ",":
                raise self.error("expected ',' or '}'")

    def read_array(self) -> list[Any]:
        self.enter()
        self.take("[")
        elements: list[Any] = []
        self.skip_whitespace()
        if self.peek() == "]":
            self.index += 1
            self.depth -= 1
            return elements
        while True:
            elements.append(self.read_value())
            self.skip_whitespace()
            character = self.peek()
            self.index += 1
            if character == "]":
                self.depth -= 1
                return elements
            if character != ",":
                raise self.error("expected ',' or ']'")

    def read_string(self) -> str:
        self.take('"')
        parts: list[str] = []
        while True:
            character = self.peek()
            self.index += 1
            if character == '"':
                return "".join(parts)
            if character == "\\":
                escape = self.peek()
                self.index += 1
                if escape == "u":
                    digits = self.text[self.index : self.index + 4]
                    if len(digits) != 4 or any(
                        digit not in "0123456789abcdefABCDEF" for digit in digits
                    ):
                        raise self.error("invalid \\u escape")
                    self.index += 4
                    parts.append(chr(int(digits, 16)))
                    continue
                if escape not in _ESCAPES:
                    raise self.error(f"invalid escape {escape!r}")
                parts.append(_ESCAPES[escape])
                continue
            if ord(character) < 0x20:
                raise self.error("unescaped control character in string")
            parts.append(character)

    def read_number(self) -> Number:
        start = self.index
        if self.peek() == "-":
            self.index += 1
        digits = self._read_digits()
        if digits == 0:
            raise self.error("number has no integer digits")
        if self.text.startswith("0", start if self.text[start] != "-" else start + 1) and digits > 1:
            raise self.error("number has a leading zero")
        if self.index < len(self.text) and self.text[self.index] == ".":
            self.index += 1
            if self._read_digits() == 0:
                raise self.error("number has no fraction digits")
        if self.index < len(self.text) and self.text[self.index] in "eE":
            self.index += 1
            if self.index < len(self.text) and self.text[self.index] in "+-":
                self.index += 1
            if self._read_digits() == 0:
                raise self.error("number has no exponent digits")
        return Number(self.text[start : self.index])

    def _read_digits(self) -> int:
        start = self.index
        while self.index < len(self.text) and self.text[self.index].isdigit():
            self.index += 1
        return self.index - start


def loads(text: str) -> Any:
    """Parse one complete JSON document, refusing duplicates and trailing data."""
    reader = _Reader(text)
    value = reader.read_value()
    reader.skip_whitespace()
    if reader.index != len(text):
        raise reader.error("trailing bytes after the document")
    return value


def dumps(value: Any) -> str:
    """Serialise without whitespace, preserving `Number` lexemes verbatim."""
    if value is None:
        return "null"
    if value is True:
        return "true"
    if value is False:
        return "false"
    if isinstance(value, Number):
        return value.lexeme
    if isinstance(value, str):
        return _quote(value)
    if isinstance(value, int):
        return str(value)
    if isinstance(value, float):
        if value != value or value in (float("inf"), float("-inf")):
            raise JsonError("non-finite numbers are not JSON")
        return repr(value)
    if isinstance(value, list):
        return "[" + ",".join(dumps(element) for element in value) + "]"
    if isinstance(value, dict):
        members = []
        for name, member in value.items():
            if not isinstance(name, str):
                raise JsonError("object member names must be strings")
            members.append(f"{_quote(name)}:{dumps(member)}")
        return "{" + ",".join(members) + "}"
    raise JsonError(f"unsupported value {type(value).__name__}")


def _quote(text: str) -> str:
    if not _NEEDS_ESCAPE.search(text):
        return '"' + text + '"'
    parts = ['"']
    for character in text:
        if character == '"':
            parts.append('\\"')
        elif character == "\\":
            parts.append("\\\\")
        elif character in "\b\f\n\r\t":
            parts.append({"\b": "\\b", "\f": "\\f", "\n": "\\n", "\r": "\\r", "\t": "\\t"}[character])
        elif ord(character) < 0x20:
            parts.append(f"\\u{ord(character):04x}")
        else:
            parts.append(character)
    parts.append('"')
    return "".join(parts)
