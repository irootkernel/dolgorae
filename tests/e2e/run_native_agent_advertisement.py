#!/usr/bin/env python3
"""Observe isolated native tool metadata; no agents, nested tools or consumer calls."""
from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from run_native_host_invocation_fixture import host_environment, run_host

OPT_IN = "DOLGORAE_RUN_NATIVE_AGENT_ADVERTISEMENT"
BODY = 4 * 1024 * 1024
PINS = {
    "codex": "d8990e3ee7acfbc26f91265b12a1a83f0fee40d0e0f8f1f40a37d4ec21f499cf",
    "claude": "ffb64331bc0ee1b69efb7c2c428bd3ab516bc31b75bf60886b356c97c1c25ffd",
}
TEXT = "Native advertisement observed. No agent, nested tool or consumer call was made."


def private(path: Path, value) -> None:
    body = json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode()
    with os.fdopen(os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600), "wb") as output:
        output.write(body)


def named(value):
    if isinstance(value, dict):
        if isinstance(value.get("name"), str):
            yield value
        for child in value.values():
            yield from named(child)
    elif isinstance(value, list):
        for child in value:
            yield from named(child)


def observe(args, host: str) -> dict:
    executable = getattr(args, host).resolve(strict=True)
    if hashlib.sha256(executable.read_bytes()).hexdigest() != PINS[host]:
        raise ValueError("selected native image changed")
    if host=="codex" and args.codex_metadata:
        helper=executable.with_name("codex-code-mode-host")
        if not helper.is_file() or hashlib.sha256(helper.read_bytes()).hexdigest()!="416721e33af8822041dc650793512b3e58d43e0be4f3a5c6187eb4dfaad3148a":
            raise ValueError("selected official code-mode companion changed or missing")
    root = Path(tempfile.mkdtemp(prefix=host[0] + "-", dir=args.scratch)).resolve()
    root.chmod(0o700)
    for name in ("home", "tmp", "config", "cache", "workspace", "codex", "claude"):
        (root / name).mkdir(mode=0o700)
    state = {"posts": 0, "model_posts": 0, "count_token_posts": 0, "failure": None, "advertisement": None, "deferred_metadata": None}
    lock = threading.Lock()

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_POST(self):
            try:
                self.connection.settimeout(5)
                if self.headers.get("Authorization") not in (None, "Bearer sk-ant-fixture-not-a-credential") or self.headers.get("x-api-key") not in (None, "sk-ant-fixture-not-a-credential"):
                    raise ValueError("authentication outside explicit fixture policy")
                if self.headers.get("Transfer-Encoding") is not None:
                    raise ValueError("streaming native request refused")
                length = int(self.headers.get("Content-Length", "0"))
                if not 0 < length <= BODY:
                    raise ValueError("native request outside bound")
                body = self.rfile.read(length)
                if len(body) != length:
                    raise ValueError("incomplete native request")
                encoding = self.headers.get("Content-Encoding", "identity")
                if encoding == "gzip":
                    with gzip.GzipFile(fileobj=io.BytesIO(body)) as stream:
                        body = stream.read(BODY + 1)
                elif encoding != "identity":
                    raise ValueError("unsupported native request encoding")
                if len(body) > BODY:
                    raise ValueError("decoded native request outside bound")
                data = json.loads(body)
                endpoint=self.path.split("?",1)[0]
                with lock:
                    state["posts"] += 1
                    if state["posts"] > 2:
                        raise ValueError("two-request observation budget exceeded")
                    private(root / f"http-{state['posts']}.json", data)
                    private(root / f"http-{state['posts']}-request.json", {"raw_path":self.path,"endpoint":endpoint,"content_encoding":encoding,"decoded_bytes":len(body)})
                    if endpoint == "/v1/messages/count_tokens":
                        state["count_token_posts"] += 1
                        answer = b'{"input_tokens":1}'
                        self.send_response(200)
                        self.send_header("Content-Type", "application/json")
                        self.send_header("Content-Length", str(len(answer)))
                        self.end_headers()
                        self.wfile.write(answer)
                        return
                    if endpoint != ("/v1/responses" if host == "codex" else "/v1/messages"):
                        raise ValueError("unexpected native endpoint")
                    state["model_posts"] += 1
                    limit=2 if host=="codex" and args.codex_metadata else 1
                    if state["model_posts"] > limit:
                        raise ValueError("native model request budget exceeded")
                    if host=="codex" and args.codex_metadata and state["model_posts"]==2:
                        returned=[v for v in data.get("input",[]) if v.get("type")=="custom_tool_call_output" and v.get("call_id")=="advertisement-metadata"]
                        if len(returned)!=1:
                            raise ValueError("metadata return correlation missing")
                        blocks=returned[0].get("output")
                        if not isinstance(blocks,list) or len(blocks)!=2 or any(v.get("type")!="input_text" for v in blocks) or not blocks[0].get("text","").startswith("Script completed\n"):
                            raise ValueError("metadata return not a completed code-mode result")
                        result=json.loads(blocks[1]["text"])
                        if result.get("schema")!="dolgorae.native-agent-tool-metadata/v1" or not isinstance(result.get("tools"),list) or len(result["tools"])>64:
                            raise ValueError("metadata return outside selected filter")
                        state["deferred_metadata"]=result
                        private(root/"deferred-tool-metadata.json",result)
                    advertisement = {"tools": data.get("tools", []), "additional_tools": [v for v in data.get("input", []) if v.get("type") == "additional_tools"]}
                    if state["advertisement"] is None:
                        state["advertisement"] = advertisement
                        private(root / "tool-advertisement.json", advertisement)
                    private(root/f"model-{state['model_posts']}-tools.json",advertisement)
                if host == "codex":
                    item={"type":"message","role":"assistant","id":"msg_advertisement","content":[{"type":"output_text","text":TEXT}]}
                    if args.codex_metadata and state["model_posts"]==1:
                        definitions=[tool for message in data.get("input",[]) if message.get("type")=="additional_tools" for namespace in message.get("tools",[]) if namespace.get("name")=="functions" for tool in namespace.get("tools",[]) if tool.get("type")=="custom" and tool.get("name")=="exec"]
                        if len(definitions)!=1 or "ALL_TOOLS" not in definitions[0].get("description",""):
                            raise ValueError("advertised code-mode metadata surface missing")
                        code='const hits = ALL_TOOLS.filter(t => /agent|spawn|fork|send_input|resume/i.test(t.name)); if (hits.length > 64) throw new Error("metadata filter exceeds bound"); text({schema:"dolgorae.native-agent-tool-metadata/v1",tools:hits});'
                        item={"type":"custom_tool_call","name":"exec","namespace":"functions","call_id":"advertisement-metadata","input":code}
                    events = [{"type": "response.created", "response": {"id": "advertisement-only"}},
                        {"type": "response.output_item.done", "item": item},
                        {"type": "response.completed", "response": {"id": "advertisement-only", "usage": {"input_tokens": 0, "input_tokens_details": None, "output_tokens": 0, "output_tokens_details": None, "total_tokens": 0}}}]
                else:
                    events = [{"type": "message_start", "message": {"id": "advertisement-only", "type": "message", "role": "assistant", "model": data.get("model"), "content": [], "stop_reason": None, "stop_sequence": None, "usage": {"input_tokens": 1, "output_tokens": 0}}},
                        {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}},
                        {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": TEXT}},
                        {"type": "content_block_stop", "index": 0},
                        {"type": "message_delta", "delta": {"stop_reason": "end_turn", "stop_sequence": None}, "usage": {"output_tokens": 1}},
                        {"type": "message_stop"}]
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Connection", "close")
                self.end_headers()
                for event in events:
                    wire = json.dumps(event, separators=(",", ":")).encode()
                    self.wfile.write(b"event: " + event["type"].encode() + b"\ndata: " + wire + b"\n\n")
                self.wfile.flush()
            except (OSError, ValueError) as error:
                state["failure"] = str(error)
                private(root/f"handler-failure-{time.monotonic_ns()}.json",{"raw_path":self.path,"endpoint":self.path.split("?",1)[0],"failure_type":type(error).__name__,"failure":str(error)})
                self.send_error(400, "bounded advertisement observation refused")

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.daemon_threads = True
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    base = f"http://127.0.0.1:{server.server_port}"
    environment = host_environment(root)
    prompt = "Inspect only the in-process ALL_TOOLS metadata, without calling any nested tool, child agent, external program or I/O. Then return the fixed advertisement-probe completion." if host=="codex" and args.codex_metadata else "Return the fixed advertisement-probe completion. Do not invoke any tool or delegate work."
    if host == "codex":
        config = (f'model = {json.dumps(args.codex_model)}\nmodel_provider = "advertisement_probe"\ncli_auth_credentials_store = "file"\n'
                  '[features]\nmulti_agent = true\nplugins = false\nshell_snapshot = false\n'
                  '[model_providers.advertisement_probe]\nname = "Isolated advertisement observation"\n'
                  f'base_url = "{base}/v1"\nwire_api = "responses"\nrequires_openai_auth = false\nsupports_websockets = false\nrequest_max_retries = 0\nstream_max_retries = 0\n')
        (root / "codex/config.toml").write_text(config)
        (root / "codex/config.toml").chmod(0o600)
        argv = [str(executable), "--no-daemon", "-a", "never", "exec", "--skip-git-repo-check", "--sandbox", "read-only", "--json", "--cd", str(root / "workspace"), prompt]
    else:
        environment.update(ANTHROPIC_BASE_URL=base, ANTHROPIC_API_KEY="sk-ant-fixture-not-a-credential")
        argv = [str(executable), "--restricted", "--setting-sources", "", "--tools", "Agent", "--permission-mode", "dontAsk", "--permission-prompts", "none", "--model", args.claude_model, "-p", "--output-format", "stream-json", "--verbose", prompt]
    private(root / "launch.json", {"argv": argv, "environment": environment, "image_sha256": PINS[host], "host_seconds": 30, "post_limit": 2,"model_post_limit":2 if host=="codex" and args.codex_metadata else 1})
    failure = None
    try:
        output = run_host(argv, root, environment, "advertisement", time.monotonic() + 30)
        if state["failure"] or state["model_posts"] != (2 if host=="codex" and args.codex_metadata else 1) or TEXT.encode() not in output.read_bytes():
            raise ValueError("native advertisement observation incomplete")
    except Exception as error:
        failure = type(error).__name__ + ": " + str(error)
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=3)
    definitions = list(named(state["advertisement"]))
    selected = [v for v in definitions if v["name"].rsplit(".", 1)[-1] in ("spawn_agent", "wait", "wait_agent", "send_input", "close_agent", "Agent", "Task")]
    result = {"schema": "dolgorae.native-agent-advertisement/v1", "host": host, "observations_complete": failure is None and not thread.is_alive(),
        "qualification_executed": False, "child_spawned": False, "consumer_acceptance": False, "owned_root": str(root),
        "image_sha256": PINS[host], "posts": state["posts"], "model_posts": state["model_posts"], "count_token_posts": state["count_token_posts"],
        "tool_names": sorted({v["name"] for v in definitions}), "selected_tool_definitions": selected,
        "deferred_metadata":state["deferred_metadata"],"metadata_tool_invoked":state["deferred_metadata"] is not None,
        "failure": failure,"handler_failure":state["failure"],
        "cleanup_complete": not thread.is_alive()}
    private(root / "observation.json", result)
    return result


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--codex", type=Path, required=True)
    parser.add_argument("--claude", type=Path, required=True)
    parser.add_argument("--codex-model", required=True)
    parser.add_argument("--claude-model", required=True)
    parser.add_argument("--scratch", type=Path, required=True)
    parser.add_argument("--codex-metadata",action="store_true")
    args = parser.parse_args()
    if os.environ.get(OPT_IN) != "1":
        raise ValueError("explicit native advertisement observation opt-in required")
    args.scratch = args.scratch.resolve(strict=True)
    rows = [observe(args, host) for host in PINS]
    print(json.dumps({"qualification_executed": False, "observations": rows}, sort_keys=True))
    return 0 if all(v["observations_complete"] and v["cleanup_complete"] for v in rows) else 1


if __name__ == "__main__":
    raise SystemExit(main())
