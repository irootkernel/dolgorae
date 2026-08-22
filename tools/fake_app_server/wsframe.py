"""Server-side RFC 6455 framing for the fake app-server.

Written directly against the specification rather than a library so the fixture
can produce the shapes a real server produces *and* the shapes a careless one
would: unmasked server frames, fragmented messages, interleaved pings, and
replies too large for any single frame.
"""

from __future__ import annotations

import base64
import hashlib
import socket

GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
MAX_HANDSHAKE_BYTES = 16 * 1024

TEXT = 0x1
BINARY = 0x2
CLOSE = 0x8
PING = 0x9
PONG = 0xA


class ProtocolError(Exception):
    """The client violated the framing contract; the fixture fails loudly."""


def accept_key(key: str) -> str:
    return base64.b64encode(hashlib.sha1((key + GUID).encode("ascii")).digest()).decode("ascii")


def read_handshake(connection: socket.socket) -> dict[str, str]:
    buffer = bytearray()
    while not buffer.endswith(b"\r\n\r\n"):
        if len(buffer) > MAX_HANDSHAKE_BYTES:
            raise ProtocolError("HTTP upgrade request exceeded its bound")
        chunk = connection.recv(1)
        if not chunk:
            raise ProtocolError("client closed during the HTTP upgrade")
        buffer.extend(chunk)
    lines = buffer.decode("latin-1").split("\r\n")
    if not lines[0].startswith("GET "):
        raise ProtocolError(f"unexpected request line: {lines[0]!r}")
    headers: dict[str, str] = {}
    for line in lines[1:]:
        if not line:
            continue
        name, _, value = line.partition(":")
        headers[name.strip().lower()] = value.strip()
    for required in ("upgrade", "connection", "sec-websocket-key", "sec-websocket-version"):
        if required not in headers:
            raise ProtocolError(f"upgrade request is missing {required}")
    if headers["upgrade"].lower() != "websocket":
        raise ProtocolError("upgrade header is not websocket")
    if headers["sec-websocket-version"] != "13":
        raise ProtocolError("only WebSocket version 13 is supported")
    return headers


def complete_handshake(connection: socket.socket, headers: dict[str, str]) -> None:
    response = (
        "HTTP/1.1 101 Switching Protocols\r\n"
        "Upgrade: websocket\r\n"
        "Connection: Upgrade\r\n"
        f"Sec-WebSocket-Accept: {accept_key(headers['sec-websocket-key'])}\r\n\r\n"
    )
    connection.sendall(response.encode("ascii"))


def _recv_exact(connection: socket.socket, count: int) -> bytes:
    buffer = bytearray()
    while len(buffer) < count:
        chunk = connection.recv(count - len(buffer))
        if not chunk:
            raise ProtocolError("client closed mid-frame")
        buffer.extend(chunk)
    return bytes(buffer)


def read_message(connection: socket.socket) -> tuple[int, bytes]:
    """Read one whole message, answering pings and honouring fragmentation."""
    payload = bytearray()
    message_opcode: int | None = None
    while True:
        prefix = _recv_exact(connection, 2)
        final = bool(prefix[0] & 0x80)
        if prefix[0] & 0x70:
            raise ProtocolError("reserved bits are set")
        opcode = prefix[0] & 0x0F
        masked = bool(prefix[1] & 0x80)
        if not masked:
            raise ProtocolError("client frames must be masked")
        length = prefix[1] & 0x7F
        if length == 126:
            length = int.from_bytes(_recv_exact(connection, 2), "big")
        elif length == 127:
            length = int.from_bytes(_recv_exact(connection, 8), "big")
        mask = _recv_exact(connection, 4)
        raw = bytearray(_recv_exact(connection, length))
        for index in range(len(raw)):
            raw[index] ^= mask[index % 4]
        if opcode & 0x08:
            if not final:
                raise ProtocolError("control frames cannot be fragmented")
            if opcode == PING:
                send_frame(connection, PONG, bytes(raw))
                continue
            if opcode == PONG:
                continue
            return CLOSE, bytes(raw)
        if opcode == 0x0:
            if message_opcode is None:
                raise ProtocolError("continuation without an open message")
        else:
            if message_opcode is not None:
                raise ProtocolError("new data frame inside an open message")
            message_opcode = opcode
        payload.extend(raw)
        if final:
            return message_opcode, bytes(payload)


def send_frame(connection: socket.socket, opcode: int, payload: bytes, final: bool = True) -> None:
    header = bytearray([(0x80 if final else 0) | opcode])
    length = len(payload)
    if length < 126:
        header.append(length)
    elif length < 65536:
        header.append(126)
        header.extend(length.to_bytes(2, "big"))
    else:
        header.append(127)
        header.extend(length.to_bytes(8, "big"))
    connection.sendall(bytes(header) + payload)


def send_text(connection: socket.socket, text: str, fragment_bytes: int | None = None) -> None:
    """Send one text message, optionally split across continuation frames."""
    payload = text.encode("utf-8")
    if fragment_bytes is None or fragment_bytes >= len(payload) or not payload:
        send_frame(connection, TEXT, payload)
        return
    chunks = [payload[index : index + fragment_bytes] for index in range(0, len(payload), fragment_bytes)]
    for position, chunk in enumerate(chunks):
        opcode = TEXT if position == 0 else 0x0
        send_frame(connection, opcode, chunk, final=position == len(chunks) - 1)


def send_close(connection: socket.socket, code: int = 1000) -> None:
    send_frame(connection, CLOSE, code.to_bytes(2, "big"))
