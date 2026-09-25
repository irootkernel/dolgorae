"""The fake Codex app-server: a WebSocket-over-Unix-socket subprocess.

ADR-014 makes this fixture an independent process with an independent parser so
conformance evidence is not a Rust module agreeing with itself.  It answers
JSON-RPC over one Unix socket, driven entirely by a manifest-validated scenario,
and holds no product logic of its own.
"""

from __future__ import annotations

import os
import pathlib
import socket
import stat
import threading
from typing import Any

import jsonlite
import scenario as scenario_module
import wsframe


class FakeAppServer:
    def __init__(
        self,
        socket_path: pathlib.Path,
        scenario: scenario_module.Scenario,
        ready_fd: int | None = None,
        transcript: pathlib.Path | None = None,
    ) -> None:
        self.socket_path = socket_path
        self.scenario = scenario
        self.ready_fd = ready_fd
        # Every client message, one JSON line each, for a case that has to
        # assert about what the client did *not* send.
        self.transcript = transcript
        self.listener: socket.socket | None = None
        # Emissions a step held back until the client answers a server request
        # this fixture has outstanding.  A real app-server does not finish a
        # Turn while it is still waiting on an approval, and a fixture that did
        # would certify a client that answers too late.
        self.deferred: list[dict[str, Any]] = []

    def bind(self) -> None:
        parent = self.socket_path.parent
        parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        if self.socket_path.exists():
            if not stat.S_ISSOCK(self.socket_path.lstat().st_mode):
                raise RuntimeError(f"{self.socket_path} exists and is not a socket")
            self.socket_path.unlink()
        listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        listener.bind(str(self.socket_path))
        os.chmod(self.socket_path, 0o600)
        listener.listen(8)
        self.listener = listener
        if self.ready_fd is not None:
            os.write(self.ready_fd, b"ready\n")
            os.close(self.ready_fd)
            self.ready_fd = None

    def serve_forever(self) -> None:
        if self.listener is None:
            self.bind()
        assert self.listener is not None
        while True:
            connection, _ = self.listener.accept()
            if self.scenario.concurrent_connections:
                threading.Thread(
                    target=self.serve_isolated_connection,
                    args=(connection,),
                    daemon=True,
                ).start()
                continue
            try:
                self.serve_connection(connection)
            except (wsframe.ProtocolError, jsonlite.JsonError, ConnectionError, OSError):
                pass
            finally:
                connection.close()

    def serve_isolated_connection(self, connection: socket.socket) -> None:
        worker = FakeAppServer(
            self.socket_path,
            self.scenario.fresh_connection(),
            transcript=self.transcript,
        )
        try:
            worker.serve_connection(connection)
        except (wsframe.ProtocolError, jsonlite.JsonError, ConnectionError, OSError):
            pass
        finally:
            connection.close()

    def serve_connection(self, connection: socket.socket) -> None:
        headers = wsframe.read_handshake(connection)
        wsframe.complete_handshake(connection, headers)
        while True:
            opcode, payload = wsframe.read_message(connection)
            if opcode == wsframe.CLOSE:
                wsframe.send_close(connection)
                return
            if opcode != wsframe.TEXT:
                raise wsframe.ProtocolError("binary messages are unsupported")
            text = payload.decode("utf-8")
            message = jsonlite.loads(text)
            if not isinstance(message, dict):
                raise wsframe.ProtocolError("a JSON-RPC message must be an object")
            self.record(text)
            if "method" not in message:
                self.remember_tool_result(message)
                # A client reply to one of this fixture's own requests.  Any
                # emission the scenario held back for it is released now, in
                # the order the step declared.
                if not self.release_deferred(connection):
                    return
                continue
            if not self.dispatch(connection, message):
                return

    def record(self, text: str) -> None:
        """Append one client message verbatim to the transcript, if enabled."""
        if self.transcript is None:
            return
        with self.transcript.open("a", encoding="utf-8") as sink:
            sink.write(text.replace("\n", " ") + "\n")

    def dispatch(self, connection: socket.socket, message: dict[str, Any]) -> bool:
        method = message["method"]
        if not isinstance(method, str):
            raise wsframe.ProtocolError("method must be a string")
        request_id = message.get("id")
        self.remember(method, message.get("params"))
        step = self.scenario.match(method)
        if step is None:
            if request_id is not None:
                self.send(
                    connection,
                    {
                        "id": request_id,
                        "error": {"code": -32601, "message": f"no scenario step for {method}"},
                    },
                )
            return True
        respond = step.get("respond")
        if request_id is not None and respond is not None and not respond.get("silent", False):
            self.send(connection, self.build_response(request_id, respond))
        emissions = list(step.get("emit", []))
        while emissions:
            emission = emissions.pop(0)
            if emission.get("await_reply", False):
                # This emission, and everything the step declared after it,
                # waits for the client to answer the request just sent.
                self.deferred.append(emission)
                self.deferred.extend(emissions)
                return True
            if not self.emit(connection, emission):
                return False
        return True

    def release_deferred(self, connection: socket.socket) -> bool:
        """Send deferred emissions through the next reply boundary."""
        pending, self.deferred = self.deferred, []
        if pending and not self.emit(connection, pending.pop(0)):
            return False
        while pending:
            emission = pending.pop(0)
            if emission.get("await_reply", False):
                self.deferred.append(emission)
                self.deferred.extend(pending)
                return True
            if not self.emit(connection, emission):
                return False
        return True

    def remember_tool_result(self, message: dict[str, Any]) -> None:
        result = message.get("result")
        if not isinstance(result, dict) or result.get("success") is not True:
            return
        items = result.get("contentItems")
        if not isinstance(items, list) or not items:
            return
        first = items[0]
        if not isinstance(first, dict) or not isinstance(first.get("text"), str):
            return
        try:
            payload = jsonlite.loads(first["text"])
        except jsonlite.JsonError:
            return
        if not isinstance(payload, dict):
            return
        if payload.get("operation") == "list_specialists_result":
            specialists = payload.get("specialists")
            if isinstance(specialists, list) and len(specialists) == 1:
                run_id = specialists[0].get("run_id")
                if isinstance(run_id, str):
                    self.scenario.bind("specialist_run_id", run_id)
        elif payload.get("operation") == "assign_specialist_task_result":
            task_id = payload.get("task_id")
            if isinstance(task_id, str):
                self.scenario.bind("specialist_task_id", task_id)

    def build_response(self, request_id: Any, respond: dict[str, Any]) -> dict[str, Any]:
        if "error" in respond:
            error = self.scenario.substitute(respond["error"])
            return {"id": request_id, "error": error}
        if "generate" in respond:
            generated = scenario_module.generate_thread_read(
                respond["generate"], self.scenario.bindings
            )
            return {"id": request_id, "result": generated}
        result = self.scenario.substitute(respond.get("result", {}))
        self.bind_answered_identities(result)
        return {"id": request_id, "result": result}

    def bind_answered_identities(self, result: Any) -> None:
        """Remember the Thread and Turn identities this fixture just handed out.

        Later steps refer to them by name, so a scenario never has to repeat an
        identifier it already declared once.
        """
        if not isinstance(result, dict):
            return
        for name, member in (("thread_id", "thread"), ("turn_id", "turn")):
            value = result.get(member)
            if isinstance(value, dict) and isinstance(value.get("id"), str):
                self.scenario.bind(name, value["id"])

    def emit(self, connection: socket.socket, emission: dict[str, Any]) -> bool:
        kind = emission["kind"]
        if kind == "close":
            wsframe.send_close(connection, int(emission.get("code", 1000)))
            return False
        body: dict[str, Any] = {
            "method": self.scenario.substitute(emission["method"]),
            "params": self.scenario.substitute(emission.get("params", {})),
        }
        if kind == "request":
            body = {"id": int(emission["id"]), **body}
        self.send(connection, body)
        return True

    def remember(self, method: str, params: Any) -> None:
        """Bind identifiers the scenario refers to by name."""
        if not isinstance(params, dict):
            return
        thread_id = params.get("threadId")
        if isinstance(thread_id, str):
            self.scenario.bind("client_thread_id", thread_id)
            self.scenario.bind("thread_id", thread_id)
        if method == "turn/start":
            self.scenario.bind("turn_index", str(self.scenario.counts.get("turn/start", 0) + 1))

    def send(self, connection: socket.socket, body: dict[str, Any]) -> None:
        wsframe.send_text(
            connection, jsonlite.dumps(body), fragment_bytes=self.scenario.fragment_bytes
        )
