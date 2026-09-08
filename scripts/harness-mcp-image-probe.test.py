#!/usr/bin/env python3
"""Regression checks for the installed-harness image qualification probe."""

import base64
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location(
    "harness_mcp_image_probe", Path(__file__).with_name("harness-mcp-image-probe.py")
)
PROBE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PROBE)


class ImageEvidenceTests(unittest.TestCase):
    def test_text_containing_base64_is_not_image_delivery(self):
        body = {"input": [{"type": "function_call_output", "output": PROBE.PNG_B64}]}
        self.assertEqual(PROBE.image_evidence(body), [])
        body["input"][0]["output"] = json.dumps(
            {"type": "image", "mimeType": "image/png", "data": PROBE.PNG_B64}
        )
        self.assertEqual(PROBE.image_evidence(body), [])

    def test_responses_and_messages_preserve_the_same_png(self):
        blocks = [
            {
                "type": "input_image",
                "image_url": "data:image/png;base64," + PROBE.PNG_B64,
            },
            {
                "type": "image_url",
                "image_url": {"url": "data:image/png;base64," + PROBE.PNG_B64},
            },
            {
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/png",
                    "data": PROBE.PNG_B64,
                },
            },
        ]
        for block in blocks:
            with self.subTest(block=block["type"]):
                evidence = PROBE.image_evidence({"messages": [{"content": [block]}]})
                self.assertEqual(len(evidence), 1)
                self.assertEqual(evidence[0]["sha256"], PROBE.PNG_SHA)
                self.assertEqual(evidence[0]["bytes"], len(PROBE.PNG))

    def test_wrong_image_and_remote_url_do_not_prove_fixture_pixels(self):
        self.assertEqual(
            PROBE.image_evidence(
                {"type": "input_image", "image_url": "https://example.com/a.png"}
            ),
            [],
        )
        evidence = PROBE.image_evidence(
            {
                "type": "input_image",
                "image_url": "data:image/png;base64,"
                + base64.b64encode(b"wrong image").decode(),
            }
        )
        self.assertNotEqual(evidence[0]["sha256"], PROBE.PNG_SHA)

    def test_deferred_namespace_is_preserved_in_the_call(self):
        name = PROBE.find_tool(
            [
                {
                    "type": "namespace",
                    "name": "mcp__tidebreak_probe",
                    "tools": [{"name": PROBE.TOOL}],
                }
            ]
        )
        events = PROBE.response_events(name, False, "fixture")
        item = events[-1]["response"]["output"][0]
        self.assertEqual(item["namespace"], "mcp__tidebreak_probe")
        self.assertEqual(item["name"], PROBE.TOOL)

    def test_mcp_fixture_discovery_and_image_result(self):
        with tempfile.TemporaryDirectory() as temporary:
            log = Path(temporary) / "mcp.jsonl"
            requests = [
                {
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {"protocolVersion": "2025-06-18"},
                },
                {"jsonrpc": "2.0", "method": "notifications/initialized"},
                {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
                {
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "tools/call",
                    "params": {"name": PROBE.TOOL, "arguments": {}},
                },
            ]
            child = subprocess.run(
                [sys.executable, str(Path(PROBE.__file__)), "--mcp", str(log)],
                input="".join(json.dumps(request) + "\n" for request in requests),
                text=True,
                capture_output=True,
                timeout=5,
                check=True,
            )
            replies = [json.loads(line) for line in child.stdout.splitlines()]
            self.assertEqual(len(replies), 3)
            self.assertEqual(replies[1]["result"]["tools"][0]["name"], PROBE.TOOL)
            image = replies[2]["result"]["content"][1]
            self.assertEqual(image["mimeType"], "image/png")
            self.assertEqual(base64.b64decode(image["data"]), PROBE.PNG)
            self.assertTrue(
                any(
                    json.loads(line).get("returned_image_sha256") == PROBE.PNG_SHA
                    for line in log.read_text().splitlines()
                )
            )

    def test_child_environment_drops_inherited_credentials(self):
        with tempfile.TemporaryDirectory() as temporary:
            env = PROBE.isolated_environment(Path(temporary))
            self.assertNotIn("OPENAI_API_KEY", env)
            self.assertNotIn("ANTHROPIC_AUTH_TOKEN", env)
            self.assertNotIn("GITHUB_TOKEN", env)
            self.assertTrue(Path(env["HOME"]).is_relative_to(temporary))
            self.assertTrue(Path(env["CODEX_HOME"]).is_relative_to(temporary))


if __name__ == "__main__":
    unittest.main()
