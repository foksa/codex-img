"""Offline tests for the Python fallback (skills/codex-img/scripts/codex_img.py)."""
import base64
import contextlib
import http.server
import importlib.util
import io
import json
import os
import tempfile
import threading
import time
import unittest
import unittest.mock

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("codex_img", os.path.join(HERE, "..", "skills", "codex-img", "scripts", "codex_img.py"))
ci = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ci)

PNG = b"\x89PNG\r\n\x1a\n" + b"\0" * 16


def jwt(payload):
    part = lambda v: base64.urlsafe_b64encode(json.dumps(v).encode()).decode().rstrip("=")
    return f"{part({'alg': 'none'})}.{part(payload)}.sig"


def write_auth(directory, exp_in=3600):
    token = jwt({"exp": int(time.time()) + exp_in, ci.JWT_CLAIM_PATH: {"chatgpt_account_id": "acct_claim"}})
    with open(os.path.join(directory, "auth.json"), "w") as f:
        json.dump({"tokens": {"access_token": token, "account_id": "acct_123"}}, f)


class Server(http.server.BaseHTTPRequestHandler):
    responses = []
    requests = []

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["content-length"])))
        Server.requests.append((self.path, {k.lower(): v for k, v in self.headers.items()}, body))
        status, payload = Server.responses.pop(0)
        data = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        if status == 429:
            self.send_header("retry-after", "0")
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *args):
        pass


class Test(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = self.tmp.name
        os.environ["CODEX_HOME"] = self.dir
        write_auth(self.dir)
        self.httpd = http.server.HTTPServer(("127.0.0.1", 0), Server)
        threading.Thread(target=self.httpd.serve_forever, daemon=True).start()
        ci.BASE_URL = f"http://127.0.0.1:{self.httpd.server_port}"
        Server.responses, Server.requests = [], []

    def tearDown(self):
        self.httpd.shutdown()
        self.httpd.server_close()
        self.tmp.cleanup()

    def run_cli(self, *args):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = ci.main(list(args))
        return code, out.getvalue(), err.getvalue()

    def ok(self, **extra):
        return 200, dict({"data": [{"b64_json": base64.b64encode(PNG).decode(), "generation_id": "gen_abc"}],
                          "size": "1024x1024", "quality": "medium", "usage": {"total_tokens": 5, "junk": "x"}}, **extra)

    def test_generates_png_with_json(self):
        Server.responses = [self.ok()]
        out = os.path.join(self.dir, "a.png")
        code, stdout, _ = self.run_cli("a fox", "-o", out, "-s", "1536x1024", "--json", "--quiet")
        self.assertEqual(code, 0)
        info = json.loads(stdout)
        self.assertEqual((info["path"], info["size"], info["usage"]), (out, "1024x1024", {"total_tokens": 5}))
        path, headers, body = Server.requests[0]
        self.assertEqual(path, "/images/generations")
        self.assertEqual(headers["chatgpt-account-id"], "acct_123")
        self.assertEqual(body, {"prompt": "a fox", "model": "gpt-image-2", "size": "1536x1024"})
        with open(out, "rb") as f:
            self.assertEqual(f.read(), PNG)
        # never overwrites, and finds out before spending a request
        Server.responses = [self.ok()]
        code, _, stderr = self.run_cli("a fox", "-o", out, "--quiet")
        self.assertEqual(code, 1)
        self.assertIn("already exists", stderr)
        self.assertEqual(len(Server.requests), 1)

    def test_edit_and_count(self):
        ref = os.path.join(self.dir, "ref.png")
        with open(ref, "wb") as f:
            f.write(PNG)
        Server.responses = [self.ok(), self.ok()]
        code, stdout, _ = self.run_cli("night", "-i", ref, "-n", "2", "-o", os.path.join(self.dir, "out/x.png"), "--quiet")
        self.assertEqual(code, 0)
        self.assertEqual(sorted(os.path.basename(p) for p in stdout.split()), ["x-1.png", "x-2.png"])
        self.assertEqual(Server.requests[0][0], "/images/edits")
        self.assertTrue(Server.requests[0][2]["images"][0]["image_url"].startswith("data:image/png;base64,"))

    def test_exit_codes(self):
        Server.responses = [(429, {"error": {"code": "usage_limit_reached"}})]
        self.assertEqual(self.run_cli("x", "-o", os.path.join(self.dir, "q.png"), "--quiet")[0], 3)
        Server.responses = [(400, {"error": {"code": "moderation_blocked"}})]
        self.assertEqual(self.run_cli("x", "-o", os.path.join(self.dir, "m.png"), "--quiet")[0], 4)
        Server.responses = [(401, {})]
        self.assertEqual(self.run_cli("x", "-o", os.path.join(self.dir, "u.png"), "--quiet")[0], 2)
        Server.responses = [(503, {}), self.ok()]
        self.assertEqual(self.run_cli("x", "-o", os.path.join(self.dir, "r.png"), "--quiet")[0], 0)

    def test_unsupported_options_fail_before_any_request(self):
        for args in (["convert", "a.png", "-o", "a.webp"], ["sheet", "a.png", "-o", "s.png"], ["batch", "art.json"], ["tile", "sky.png", "-o", "t.png"], ["x", "-f", "jpeg"], ["x", "-c", "64"], ["x", "--output-quality", "80"], ["x", "--via-responses"], ["x", "--trim=4"], ["x", "--resize", "400x"], ["x", "-o", "a.jpg"], ["x", "-s", "big"], []):
            self.assertEqual(self.run_cli(*args)[0], 64, args)
        self.assertEqual(Server.requests, [])

    def test_failed_write_leaves_no_file(self):
        real_open = open

        class Full:
            def __init__(self, f):
                self.f = f

            def __enter__(self):
                return self

            def __exit__(self, *args):
                self.f.close()

            def write(self, data):
                self.f.write(data[:3])
                raise OSError(28, "No space left on device")

        def full_disk(path, mode="r", *args, **kwargs):
            f = real_open(path, mode, *args, **kwargs)
            return Full(f) if "x" in mode else f

        folder = os.path.join(self.dir, "out")
        with unittest.mock.patch("builtins.open", full_disk):
            with self.assertRaises(ci.Fail) as failed:
                ci.save(PNG, os.path.join(folder, "a.png"))
        self.assertIn("No space left", str(failed.exception))
        self.assertEqual(os.listdir(folder), [])
        ci.save(PNG, os.path.join(folder, "a.png"))
        self.assertEqual(os.listdir(folder), ["a.png"])

    def test_auth(self):
        code, stdout, _ = self.run_cli("status", "--json")
        self.assertEqual((code, json.loads(stdout)["accountId"]), (0, "acct_123"))
        write_auth(self.dir, exp_in=10)
        code, _, stderr = self.run_cli("status")
        self.assertEqual(code, 2)
        self.assertIn("Open Codex", stderr)
        os.remove(os.path.join(self.dir, "auth.json"))
        self.assertEqual(self.run_cli("x")[0], 2)


if __name__ == "__main__":
    unittest.main()
