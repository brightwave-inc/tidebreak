#!/usr/bin/env python3
"""Qualify installed harness MCP image delivery without paid inference.

Run on macOS with sandbox-exec. The harness gets isolated configuration and
only loopback networking. The provider fixture requests one MCP capture, then
checks that the next model request includes the exact PNG as image content.
Raw requests and child output stay under the requested artifact directory.
This verifies transport; it does not qualify native input or model reasoning.
"""

import argparse
import base64
import hashlib
import http.server
import json
import os
from pathlib import Path
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import threading
import time
import zlib

TOOL = "capture_fixture"
SERVER = "tidebreak_probe"
PROMPT = "Call the capture_fixture MCP tool once, inspect its image, then reply PROBE_COMPLETE."


def png_bytes():
    """Create a deterministic 32 by 32 RGB checkerboard PNG."""

    def chunk(kind, payload):
        return (
            struct.pack(">I", len(payload))
            + kind
            + payload
            + struct.pack(">I", zlib.crc32(kind + payload) & 0xFFFFFFFF)
        )

    rows = bytearray()
    for y in range(32):
        rows.append(0)
        for x in range(32):
            rows.extend((240, 70, 40) if (x // 8 + y // 8) % 2 else (20, 120, 230))
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", 32, 32, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(bytes(rows)))
        + chunk(b"IEND", b"")
    )


PNG = png_bytes()
PNG_B64 = base64.b64encode(PNG).decode()
PNG_SHA = hashlib.sha256(PNG).hexdigest()


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def mcp_main(log_path):
    with log_path.open("a", buffering=1) as log:
        for line in sys.stdin:
            request = json.loads(line)
            method = request.get("method")
            log.write(
                json.dumps({"method": method, "params": request.get("params")}) + "\n"
            )
            if "id" not in request:
                continue
            if method == "initialize":
                result = {
                    "protocolVersion": request["params"]["protocolVersion"],
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": SERVER, "version": "1.0.0"},
                }
            elif method == "tools/list":
                result = {
                    "tools": [
                        {
                            "name": TOOL,
                            "description": "Return the harmless checkerboard screenshot fixture as a PNG image.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {},
                                "additionalProperties": False,
                            },
                            "annotations": {
                                "readOnlyHint": True,
                                "destructiveHint": False,
                                "openWorldHint": False,
                            },
                        }
                    ]
                }
            elif (
                method == "tools/call" and request.get("params", {}).get("name") == TOOL
            ):
                result = {
                    "content": [
                        {
                            "type": "text",
                            "text": "Checkerboard fixture: 32 by 32 pixels.",
                        },
                        {"type": "image", "mimeType": "image/png", "data": PNG_B64},
                    ],
                    "isError": False,
                }
                log.write(json.dumps({"returned_image_sha256": PNG_SHA}) + "\n")
            elif method in (
                "ping",
                "resources/list",
                "prompts/list",
                "resources/templates/list",
            ):
                result = {
                    "resources/list": {"resources": []},
                    "prompts/list": {"prompts": []},
                    "resources/templates/list": {"resourceTemplates": []},
                }.get(method, {})
            else:
                print(
                    json.dumps(
                        {
                            "jsonrpc": "2.0",
                            "id": request["id"],
                            "error": {
                                "code": -32601,
                                "message": "Fixture method unavailable",
                            },
                        }
                    ),
                    flush=True,
                )
                continue
            print(
                json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}),
                flush=True,
            )


def find_tool(tools):
    for tool in tools:
        if tool.get("type") == "namespace":
            nested = find_tool(tool.get("tools", []))
            if nested:
                return tool["name"] + "." + nested
        item = tool.get("function", tool)
        if TOOL in item.get("name", ""):
            return item["name"]
    return None


def image_evidence(value, path="request"):
    """Recognize image blocks, never a base64 string inside plain tool text."""
    found = []
    if isinstance(value, list):
        for index, item in enumerate(value):
            found.extend(image_evidence(item, f"{path}[{index}]"))
    elif isinstance(value, dict):
        encoded = None
        if value.get("type") in ("input_image", "image_url"):
            url = value.get("image_url")
            if isinstance(url, dict):
                url = url.get("url")
            if isinstance(url, str) and url.startswith("data:image/png;base64,"):
                encoded = url.split(",", 1)[1]
        elif value.get("type") == "image" and isinstance(value.get("source"), dict):
            source = value["source"]
            if (
                source.get("type") == "base64"
                and source.get("media_type") == "image/png"
            ):
                encoded = source.get("data")
        if encoded:
            raw = base64.b64decode(encoded, validate=True)
            found.append(
                {
                    "path": path,
                    "bytes": len(raw),
                    "sha256": hashlib.sha256(raw).hexdigest(),
                }
            )
        for key, item in value.items():
            found.extend(image_evidence(item, f"{path}.{key}"))
    return found


def response_events(name, final, model, search=False):
    response_id = "resp_fixture_final" if final else "resp_fixture_capture"
    if search:
        item = {
            "id": "ts_fixture",
            "type": "tool_search_call",
            "call_id": "search_fixture",
            "execution": "client",
            "status": "completed",
            "arguments": {"query": TOOL, "limit": 1},
        }
    elif final:
        item = {
            "id": "msg_fixture",
            "type": "message",
            "role": "assistant",
            "status": "completed",
            "content": [
                {"type": "output_text", "text": "PROBE_COMPLETE", "annotations": []}
            ],
        }
    else:
        item = {
            "id": "fc_fixture",
            "type": "function_call",
            "call_id": "call_fixture",
            "name": name.split(".")[-1],
            "arguments": "{}",
            "status": "completed",
        }
        if "." in name:
            item["namespace"] = name.rsplit(".", 1)[0]
    response = {
        "id": response_id,
        "object": "response",
        "created_at": int(time.time()),
        "status": "in_progress",
        "model": model,
        "output": [],
    }
    events = [
        {"type": "response.created", "response": response.copy()},
        {
            "type": "response.output_item.added",
            "output_index": 0,
            "item": dict(
                item,
                status="in_progress",
                **(
                    {}
                    if search
                    else ({"arguments": ""} if not final else {"content": []})
                ),
            ),
        },
    ]
    if search:
        pass
    elif final:
        events.extend(
            [
                {
                    "type": "response.content_part.added",
                    "item_id": item["id"],
                    "output_index": 0,
                    "content_index": 0,
                    "part": {"type": "output_text", "text": "", "annotations": []},
                },
                {
                    "type": "response.output_text.delta",
                    "item_id": item["id"],
                    "output_index": 0,
                    "content_index": 0,
                    "delta": "PROBE_COMPLETE",
                },
                {
                    "type": "response.output_text.done",
                    "item_id": item["id"],
                    "output_index": 0,
                    "content_index": 0,
                    "text": "PROBE_COMPLETE",
                },
                {
                    "type": "response.content_part.done",
                    "item_id": item["id"],
                    "output_index": 0,
                    "content_index": 0,
                    "part": item["content"][0],
                },
            ]
        )
    else:
        events.extend(
            [
                {
                    "type": "response.function_call_arguments.delta",
                    "item_id": item["id"],
                    "output_index": 0,
                    "delta": "{}",
                },
                {
                    "type": "response.function_call_arguments.done",
                    "item_id": item["id"],
                    "output_index": 0,
                    "arguments": "{}",
                },
            ]
        )
    response.update(
        status="completed",
        output=[item],
        usage={
            "input_tokens": 10,
            "output_tokens": 5,
            "total_tokens": 15,
            "input_tokens_details": {"cached_tokens": 0},
            "output_tokens_details": {"reasoning_tokens": 0},
        },
    )
    events.extend(
        [
            {"type": "response.output_item.done", "output_index": 0, "item": item},
            {"type": "response.completed", "response": response},
        ]
    )
    return [dict(event, sequence_number=i) for i, event in enumerate(events)]


def anthropic_events(name, final, model):
    message = {
        "id": "msg_fixture",
        "type": "message",
        "role": "assistant",
        "content": [],
        "model": model,
        "stop_reason": None,
        "stop_sequence": None,
        "usage": {"input_tokens": 10, "output_tokens": 0},
    }
    content = (
        {"type": "text", "text": ""}
        if final
        else {"type": "tool_use", "id": "toolu_fixture", "name": name, "input": {}}
    )
    delta = (
        {"type": "text_delta", "text": "PROBE_COMPLETE"}
        if final
        else {"type": "input_json_delta", "partial_json": "{}"}
    )
    return [
        {"type": "message_start", "message": message},
        {"type": "content_block_start", "index": 0, "content_block": content},
        {"type": "content_block_delta", "index": 0, "delta": delta},
        {"type": "content_block_stop", "index": 0},
        {
            "type": "message_delta",
            "delta": {
                "stop_reason": "end_turn" if final else "tool_use",
                "stop_sequence": None,
            },
            "usage": {"output_tokens": 5},
        },
        {"type": "message_stop"},
    ]


class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def json_reply(self, status, body):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        self.json_reply(
            404, {"error": "Only the local scripted model endpoint is available"}
        )

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        raw = self.rfile.read(length)
        if self.headers.get("Content-Encoding") == "zstd":
            import compression.zstd

            raw = compression.zstd.decompress(raw)
        body = json.loads(raw)
        state = self.server.probe
        with state["lock"]:
            number = len(state["requests"]) + 1
            write_json(
                state["directory"] / f"request-{number}.json",
                {"path": self.path, "body": body},
            )
            available = body.get("tools", []) + [
                tool
                for entry in body.get("input", [])
                if isinstance(entry, dict) and entry.get("type") == "tool_search_output"
                for tool in entry.get("tools", [])
            ]
            entry = {
                "path": self.path,
                "tool": find_tool(available),
                "images": image_evidence(body),
            }
            state["requests"].append(entry)
            tool = entry["tool"]
            search = (
                tool is None
                and not state.get("searched")
                and any(item.get("type") == "tool_search" for item in available)
            )
            if search:
                state["searched"] = True
            final = not search and (
                bool(entry["images"]) or state["called"] or tool is None
            )
            if tool and not final:
                state["called"] = True
        if self.path.split("?", 1)[0] not in ("/v1/responses", "/v1/messages"):
            self.json_reply(400, {"error": "Unexpected model protocol"})
            return
        events = (
            anthropic_events(tool, final, body.get("model"))
            if "/messages" in self.path
            else response_events(tool, final, body.get("model"), search=search)
        )
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "close")
        self.end_headers()
        for event in events:
            self.wfile.write(
                f"event: {event['type']}\ndata: {json.dumps(event)}\n\n".encode()
            )
        self.wfile.flush()


def isolated_environment(directory):
    node = shutil.which("node")
    runtime_path = str(Path(node).parent) + ":" if node else ""
    env = {
        "PATH": runtime_path
        + "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
        "LANG": "en_US.UTF-8",
    }
    for name, folder in {
        "HOME": "home",
        "CODEX_HOME": "codex",
        "CLAUDE_CONFIG_DIR": "claude",
        "XDG_CONFIG_HOME": "config",
        "XDG_DATA_HOME": "data",
        "XDG_CACHE_HOME": "cache",
        "XDG_STATE_HOME": "state",
        "XDG_RUNTIME_DIR": "runtime",
        "TMPDIR": "tmp",
    }.items():
        path = directory / folder
        path.mkdir(mode=0o700)
        env[name] = str(path)
    return env


def harness_command(kind, binary, directory, base, env):
    mcp = [
        sys.executable,
        str(Path(__file__).resolve()),
        "--mcp",
        str(directory / "mcp.jsonl"),
    ]
    if kind == "codex":
        settings = {
            "model_provider": '"fixture"',
            "model": '"gpt-5.5"',
            "model_providers.fixture.name": '"Fixture"',
            "model_providers.fixture.base_url": json.dumps(base + "/v1"),
            "model_providers.fixture.env_key": '"TB_FIXTURE_KEY"',
            "model_providers.fixture.wire_api": '"responses"',
            "model_providers.fixture.supports_websockets": "false",
            "model_providers.fixture.request_max_retries": "0",
            "approval_policy": '"never"',
            "check_for_update_on_startup": "false",
            "analytics.enabled": "false",
            "feedback.enabled": "false",
            "otel.metrics_exporter": '"none"',
            "features.apps": "false",
            "features.hooks": "false",
            "web_search": '"disabled"',
            "mcp_optional_startup_grace_ms": "0",
            f"mcp_servers.{SERVER}.command": json.dumps(mcp[0]),
            f"mcp_servers.{SERVER}.args": json.dumps(mcp[1:]),
        }
        env["TB_FIXTURE_KEY"] = "local-fixture-no-inference"
        config = [
            arg for key, value in settings.items() for arg in ("-c", f"{key}={value}")
        ]
        return [
            binary,
            "exec",
            "--json",
            "--ephemeral",
            "--ignore-user-config",
            "--ignore-rules",
            "--skip-git-repo-check",
            "-s",
            "read-only",
            *config,
            PROMPT,
        ]
    if kind == "claude":
        env.update(
            ANTHROPIC_API_KEY="local-fixture-no-inference",
            ANTHROPIC_BASE_URL=base,
            CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC="1",
            DISABLE_AUTOUPDATER="1",
            DISABLE_TELEMETRY="1",
            DISABLE_ERROR_REPORTING="1",
            ENABLE_TOOL_SEARCH="false",
        )
        config = {
            "mcpServers": {
                SERVER: {"type": "stdio", "command": mcp[0], "args": mcp[1:]}
            }
        }
        return [
            binary,
            "--bare",
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--model",
            "claude-sonnet-4-5",
            "--mcp-config",
            json.dumps(config),
            "--strict-mcp-config",
            "--setting-sources",
            "",
            "--tools",
            "",
            "--allowedTools",
            f"mcp__{SERVER}__{TOOL}",
            "--permission-mode",
            "dontAsk",
            "--no-session-persistence",
            "--no-chrome",
            "--disable-slash-commands",
            "--system-prompt",
            "Use the fixture tool, then finish.",
            PROMPT,
        ]
    for suffix in (
        "AUTOUPDATE",
        "MODELS_FETCH",
        "LSP_DOWNLOAD",
        "DEFAULT_PLUGINS",
        "PROJECT_CONFIG",
        "CLAUDE_CODE",
        "EXTERNAL_SKILLS",
        "SHARE",
    ):
        env["OPENCODE_DISABLE_" + suffix] = "1"
    env["OPENCODE_CONFIG_CONTENT"] = json.dumps(
        {
            "$schema": "https://opencode.ai/config.json",
            "model": "openai/gpt-5.2",
            "autoupdate": False,
            "enabled_providers": ["openai"],
            "provider": {
                "openai": {
                    "options": {
                        "baseURL": base + "/v1",
                        "apiKey": "local-fixture-no-inference",
                    }
                }
            },
            "mcp": {SERVER: {"type": "local", "command": mcp, "enabled": True}},
            "permission": {"*": "deny", SERVER + "_*": "allow"},
        }
    )
    return [
        binary,
        "run",
        "--pure",
        "--format",
        "json",
        "--print-logs",
        "--log-level",
        "DEBUG",
        PROMPT,
    ]


def qualify(kind, binary, output, timeout):
    directory = Path(tempfile.mkdtemp(prefix=kind + "-", dir=output))
    env = isolated_environment(directory)
    work = directory / "work"
    work.mkdir()
    profile = directory / "loopback.sb"
    profile.write_text(
        "(version 1)\n(allow default)\n(deny network-outbound)\n"
        '(allow network-outbound (remote ip "localhost:*"))\n'
    )
    sandbox = ["/usr/bin/sandbox-exec", "-f", str(profile)]
    check = subprocess.run(
        sandbox
        + [
            sys.executable,
            "-c",
            "import socket; s=socket.socket(); "
            "s.settimeout(2); print(s.connect_ex(('192.0.2.1',443)))",
        ],
        cwd=work,
        env=env,
        capture_output=True,
        text=True,
        timeout=5,
    )
    if check.returncode or check.stdout.strip() != "1":
        raise RuntimeError("Loopback network confinement did not return EPERM")
    version = subprocess.run(
        sandbox + [binary, "--version"],
        cwd=work,
        env=env,
        capture_output=True,
        text=True,
        timeout=10,
    )
    if version.returncode:
        raise RuntimeError("Harness version probe failed: " + version.stderr)
    state = {
        "directory": directory,
        "requests": [],
        "called": False,
        "lock": threading.Lock(),
    }
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    server.probe = state
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    command = harness_command(
        kind, binary, directory, f"http://127.0.0.1:{server.server_port}", env
    )
    write_json(
        directory / "launch.json",
        {"command": command, "isolated_environment_names": sorted(env)},
    )
    timed_out = False
    with (
        (directory / "stdout.jsonl").open("wb") as stdout,
        (directory / "stderr.log").open("wb") as stderr,
    ):
        child = subprocess.Popen(
            sandbox + command,
            cwd=work,
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=stdout,
            stderr=stderr,
            start_new_session=True,
        )
        try:
            code = child.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
            os.killpg(child.pid, signal.SIGTERM)
            try:
                code = child.wait(timeout=3)
            except subprocess.TimeoutExpired:
                os.killpg(child.pid, signal.SIGKILL)
                code = child.wait(timeout=3)
    server.shutdown()
    server.server_close()
    log_path = directory / "mcp.jsonl"
    mcp = (
        [json.loads(line) for line in log_path.read_text().splitlines()]
        if log_path.exists()
        else []
    )
    discovered = any(item.get("method") == "tools/list" for item in mcp)
    invoked = any(
        item.get("method") == "tools/call"
        and item.get("params", {}).get("name") == TOOL
        for item in mcp
    )
    returned = any(item.get("returned_image_sha256") == PNG_SHA for item in mcp)
    image_requests = [
        dict(image, request_index=i + 1)
        for i, req in enumerate(state["requests"])
        for image in req["images"]
        if image["sha256"] == PNG_SHA
    ]
    passed = code == 0 and discovered and invoked and returned and bool(image_requests)
    report = {
        "harness": kind,
        "version": version.stdout.strip(),
        "status": "passed" if passed else "failed",
        "scope": "installed_harness_mcp_image_transport",
        "outbound_network": "loopback_only_verified",
        "paid_inference": False,
        "child_exit": code,
        "timed_out": timed_out,
        "mcp_discovered": discovered,
        "mcp_invoked": invoked,
        "mcp_returned_image": returned,
        "fixture_png_bytes": len(PNG),
        "fixture_png_sha256": PNG_SHA,
        "provider_requests": state["requests"],
        "matching_images": image_requests,
        "remaining_gates": [
            "actual_tidebreak_bridge",
            "native_or_browser_capture",
            "model_reasoning",
        ],
    }
    write_json(directory / "report.json", report)
    print(json.dumps({"report": str(directory / "report.json"), **report}), flush=True)
    return passed


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mcp", type=Path, help=argparse.SUPPRESS)
    parser.add_argument(
        "--harness", choices=("codex", "claude", "opencode", "all"), default="all"
    )
    parser.add_argument(
        "--output-dir", type=Path, default=Path(".context/harness-image-probe")
    )
    parser.add_argument("--binary", help="Override the executable for a single harness")
    parser.add_argument("--timeout", type=int, default=60)
    args = parser.parse_args()
    if args.mcp:
        mcp_main(args.mcp)
        return
    if sys.platform != "darwin" or not Path("/usr/bin/sandbox-exec").exists():
        parser.error(
            "This probe requires macOS sandbox-exec to enforce loopback-only networking"
        )
    if args.binary and args.harness == "all":
        parser.error("Use --binary with a single harness")
    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=True, mode=0o700)
    names = (
        ("codex", "claude", "opencode") if args.harness == "all" else (args.harness,)
    )
    results = []
    for name in names:
        binary = args.binary or shutil.which(name)
        if not binary:
            parser.error(f"Install {name} or supply --binary")
        results.append(
            qualify(name, str(Path(binary).absolute()), output, args.timeout)
        )
    sys.exit(0 if all(results) else 1)


if __name__ == "__main__":
    main()
