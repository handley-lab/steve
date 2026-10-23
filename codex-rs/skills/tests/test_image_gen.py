"""Local-only destination regressions for the bundled image API script.

Requires the openai SDK; run with unittest discovery like test_skill_installer.py.
"""

import asyncio
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import unittest
from unittest.mock import patch


SCRIPT = (
    Path(__file__).resolve().parents[1]
    / "src/assets/samples/imagegen/scripts/image_gen.py"
)
spec = importlib.util.spec_from_file_location("image_gen", SCRIPT)
image_gen = importlib.util.module_from_spec(spec)
spec.loader.exec_module(image_gen)


class ImageClientDestinationTests(unittest.TestCase):
    def client(self, asynchronous):
        return (
            image_gen._create_async_client()
            if asynchronous
            else image_gen._create_client()
        )

    def close(self, client, asynchronous):
        if asynchronous:
            asyncio.run(client.close())
        else:
            client.close()

    def server(self, redirect=None):
        requests = []

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                requests.append({
                    "path": self.path,
                    "headers": dict(self.headers),
                    "body": json.loads(self.rfile.read(int(self.headers["Content-Length"]))),
                })
                print("image API request: " + json.dumps(requests[-1]), flush=True)
                if redirect:
                    self.send_response(307)
                    self.send_header("Location", redirect)
                    self.end_headers()
                else:
                    body = json.dumps({"created": 1, "data": [{"b64_json": "c3ludGhldGlj"}]}).encode()
                    self.send_response(200)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)

            def log_message(self, *args):
                pass

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        return f"http://127.0.0.1:{server.server_port}", requests

    def generate(self, asynchronous):
        if asynchronous:
            async def run():
                async with self.client(True) as client:
                    return await client.images.generate(model="gpt-image-2", prompt="synthetic image prompt")
            return asyncio.run(run())
        with self.client(False) as client:
            return client.images.generate(model="gpt-image-2", prompt="synthetic image prompt")

    def test_missing_or_blank_endpoint_rejected(self):
        for asynchronous in [False, True]:
            for endpoint in [None, "", " \t"]:
                with self.subTest(asynchronous=asynchronous, endpoint=endpoint):
                    environment = {"OPENAI_API_KEY": "synthetic-image-key"}
                    if endpoint is not None:
                        environment["OPENAI_BASE_URL"] = endpoint
                    with patch.dict(os.environ, environment, clear=True):
                        with self.assertRaises(SystemExit):
                            client = self.client(asynchronous)
                            self.close(client, asynchronous)

    def test_explicit_openai_endpoint_remains_valid(self):
        for asynchronous in [False, True]:
            with self.subTest(asynchronous=asynchronous):
                with patch.dict(os.environ, {
                    "OPENAI_API_KEY": "synthetic-image-key",
                    "OPENAI_BASE_URL": "https://api.openai.com/v1",
                }, clear=True):
                    client = self.client(asynchronous)
                    try:
                        self.assertEqual(str(client.base_url), "https://api.openai.com/v1/")
                    finally:
                        self.close(client, asynchronous)

    def test_configured_local_delivery(self):
        for asynchronous in [False, True]:
            with self.subTest(asynchronous=asynchronous):
                base, requests = self.server()
                with patch.dict(os.environ, {
                    "OPENAI_API_KEY": "synthetic-image-key",
                    "OPENAI_BASE_URL": f"{base}/v1",
                }, clear=True):
                    response = self.generate(asynchronous)
                self.assertEqual(response.data[0].b64_json, "c3ludGhldGlj")
                self.assertEqual(len(requests), 1)
                self.assertEqual(requests[0]["path"], "/v1/images/generations")
                headers = {name.lower(): value for name, value in requests[0]["headers"].items()}
                self.assertEqual(headers["authorization"], "Bearer synthetic-image-key")
                self.assertEqual(requests[0]["body"]["prompt"], "synthetic image prompt")

    def test_cli_uses_configured_local_model(self):
        base, requests = self.server()
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "image.png"
            result = subprocess.run(
                [sys.executable, "-B", str(SCRIPT), "generate", "--prompt", "synthetic image prompt",
                 "--model", "local-image", "--out", str(output), "--no-augment"],
                env={"OPENAI_API_KEY": "synthetic-image-key", "OPENAI_BASE_URL": f"{base}/v1"},
                capture_output=True, text=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(output.read_bytes(), b"synthetic")
        self.assertEqual(len(requests), 1)
        self.assertEqual(requests[0]["body"]["model"], "local-image")

    def test_missing_model_does_not_send_a_request(self):
        base, requests = self.server()
        result = subprocess.run(
            [sys.executable, "-B", str(SCRIPT), "generate", "--prompt", "synthetic image prompt"],
            env={"OPENAI_API_KEY": "synthetic-image-key", "OPENAI_BASE_URL": f"{base}/v1"},
            capture_output=True, text=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--model", result.stderr)
        self.assertEqual(requests, [])

    def test_dry_run_needs_neither_token_nor_endpoint(self):
        result = subprocess.run(
            [sys.executable, "-B", str(SCRIPT), "generate", "--prompt", "synthetic image prompt",
             "--model", "local-image", "--out", "/tmp/dry-run.png", "--dry-run", "--no-augment"],
            env={}, capture_output=True, text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["prompt"], "synthetic image prompt")

    def test_redirect_does_not_deliver_prompt_to_another_origin(self):
        for asynchronous in [False, True]:
            with self.subTest(asynchronous=asynchronous):
                recipient, received = self.server()
                base, sent = self.server(f"{recipient}/unconfigured")
                with patch.dict(os.environ, {
                    "OPENAI_API_KEY": "synthetic-image-key",
                    "OPENAI_BASE_URL": f"{base}/v1",
                }, clear=True):
                    import openai
                    error = None
                    try:
                        self.generate(asynchronous)
                    except openai.APIStatusError as caught:
                        error = caught
                print(f"asynchronous={asynchronous}, configured_requests={len(sent)}, redirected_requests={len(received)}", flush=True)
                self.assertEqual(len(sent), 1)
                self.assertEqual(received, [])
                self.assertIsNotNone(error)
                self.assertEqual(error.status_code, 307)


if __name__ == "__main__":
    unittest.main()
