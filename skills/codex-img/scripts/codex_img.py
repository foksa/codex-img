#!/usr/bin/env python3
"""Fallback for the codex-img binary: the direct image route only, Python 3.9+ stdlib only.

Same flags, exit codes and --json output as the binary, minus what needs image
libraries or the Responses route: output is always PNG (no JPEG/WebP conversion,
no --via-responses). Convert or resize afterwards with other tools if needed.

Reads the ChatGPT login that `codex login` keeps in $CODEX_HOME/auth.json and
never refreshes it (refresh tokens rotate; refreshing here could log codex out).
"""
import base64
import contextlib
import json
import os
import random
import sys
import threading
import time
import urllib.error
import urllib.request
import uuid

VERSION = "0.6.3-py"
BASE_URL = "https://chatgpt.com/backend-api/codex"
IMAGE_MODEL = "gpt-image-2"
JWT_CLAIM_PATH = "https://api.openai.com/auth"
EXPIRY_MARGIN_SECS = 60
REQUEST_TIMEOUT = 5 * 60
MAX_RETRIES = 3
MAX_RETRY_DELAY = 30.0
MAX_RESPONSE_BYTES = 100 * 1024 * 1024
MAX_ERROR_BYTES = 16 * 1024
MAX_PROMPT_CHARS = 32000
MAX_EDIT_IMAGES = 5
MAX_IMAGE_BYTES = 32 * 1024 * 1024
MAX_INPUT_IMAGE_BYTES = 20 * 1024 * 1024
MAX_TOTAL_INPUT_BYTES = 50 * 1024 * 1024
QUOTA_CODES = {
    "insufficient_quota", "quota_exceeded", "usage_limit_reached", "usage_limit_exceeded",
    "billing_hard_limit_reached", "billing_not_active", "organization_usage_limit_exceeded",
    "workspace_member_usage_limit_reached",
}
RENEW_HINT = ("Open Codex (run `codex` or the Codex app) once so it renews your login, "
              "or run `codex login` to sign in again, then retry.")

OTHER, AUTH, QUOTA, MODERATION, USAGE = 1, 2, 3, 4, 64

HELP = f"""codex_img.py {VERSION} - fallback for codex-img (direct route, PNG only)

Usage:
  codex_img.py [options] "<prompt>"
  echo "<prompt>" | codex_img.py [options] -
  codex_img.py status [--json]   Check the Codex login offline (uses no quota)

Options:
  -o, --output <path>       Output file or directory (default: current directory)
  -i, --image <path>        Reference image to edit/compose (repeatable, max {MAX_EDIT_IMAGES})
  -s, --size <WxH>          Shape hint, e.g. 1536x1024, 1024x1536, auto
  -q, --quality <q>         low | medium | high | auto
  -b, --background <bg>     transparent | opaque | auto
  -n, --count <n>           Number of images, generated in parallel (default: 1)
      --json                Print one JSON object per image to stdout
      --quiet               No progress on stderr
  -h, --help                Show help
  -v, --version             Show version

Output is always PNG. Not supported here: -f/--format, -c/--colors, --dither,
--output-quality, --lossless, --trim, --resize, --fit, --hard-alpha, --no-enlarge, --no-bleed,
--via-responses, --model.
Exit codes: 0 ok, 1 error, 2 auth, 3 quota, 4 moderation, 64 usage."""


class Fail(Exception):
    def __init__(self, code, message):
        super().__init__(message)
        self.code = code


# --- Arguments ---

def is_size(value):
    if value == "auto":
        return True
    w, sep, h = value.partition("x")
    ok = lambda p: 2 <= len(p) <= 5 and p.isdigit() and p.isascii() and not p.startswith("0")
    return bool(sep) and ok(w) and ok(h)


def parse(args):
    # The binary's subcommands: refuse them rather than generate an image from "sheet a.png".
    if args[:1] and args[0] in ("convert", "sheet", "batch", "tile"):
        raise Fail(USAGE, f"`{args[0]}` needs the codex-img binary; the Python fallback only generates and edits images. "
                          f"Install the binary, or quote a prompt that starts with \"{args[0]}\".")
    if args[:1] == ["status"] and all(a == "--json" for a in args[1:]):
        return {"command": "status", "json": len(args) > 1}
    names = {
        "-o": "output", "--output": "output", "-i": "image", "--image": "image",
        "-s": "size", "--size": "size", "-q": "quality", "--quality": "quality",
        "-b": "background", "--background": "background", "-n": "count", "--count": "count",
    }
    unsupported = {"-f", "--format", "--via-responses", "-m", "--model", "-c", "--colors", "--dither",
                   "--output-quality", "--lossless", "--trim", "--resize", "--fit", "--hard-alpha", "--no-enlarge", "--no-bleed"}
    flags = {"--json": "json", "--quiet": "quiet", "-h": "help", "--help": "help", "-v": "version", "--version": "version"}
    values, seen, positionals = {"image": []}, set(), []
    it = iter(args)
    for arg in it:
        if arg == "--":
            positionals.extend(it)
            break
        if arg == "-" or not arg.startswith("-"):
            positionals.append(arg)
            continue
        name, inline = arg, None
        if arg.startswith("--") and "=" in arg:
            name, inline = arg.split("=", 1)
        if name in unsupported:
            raise Fail(USAGE, f"{name} is not supported by the Python fallback (PNG only, direct route). "
                              "Install the codex-img binary, or convert, trim or resize the PNG afterwards.")
        if name in flags:
            seen.add(flags[name])
            continue
        key = names.get(name)
        if key is None:
            raise Fail(USAGE, f"Unknown option: {arg}")
        if inline is None:
            inline = next(it, None)
            if inline is None:
                raise Fail(USAGE, f"{name} needs a value.")
        if key == "image":
            values["image"].append(inline)
        else:
            values[key] = inline
    if "help" in seen:
        return {"command": "help"}
    if "version" in seen:
        return {"command": "version"}
    if not positionals:
        raise Fail(USAGE, "Missing prompt.")
    output = values.get("output")
    if output and not output.endswith("/") and not os.path.isdir(output):
        ext = os.path.splitext(output)[1].lower()
        if ext and ext != ".png":
            raise Fail(USAGE, f"The Python fallback only writes PNG; use a .png output path and convert "
                              f"afterwards if you need {ext}.")
    size = values.get("size")
    if size is not None and not is_size(size):
        raise Fail(USAGE, "--size must be WIDTHxHEIGHT or auto.")
    for key, allowed in (("quality", ("low", "medium", "high", "auto")), ("background", ("transparent", "opaque", "auto"))):
        if values.get(key) is not None and values[key] not in allowed:
            raise Fail(USAGE, f"--{key} must be one of: {', '.join(allowed)}")
    count = values.get("count", "1")
    if not (count.isdigit() and 1 <= int(count) <= 10):
        raise Fail(USAGE, "--count must be an integer from 1 to 10.")
    if len(values["image"]) > MAX_EDIT_IMAGES:
        raise Fail(USAGE, f"At most {MAX_EDIT_IMAGES} --image references are supported.")
    return {
        "command": "run", "prompt": " ".join(positionals), "output": output, "images": values["image"],
        "size": size, "quality": values.get("quality"), "background": values.get("background"),
        "count": int(count), "json": "json" in seen, "quiet": "quiet" in seen,
    }


# --- Auth ---

def auth_path():
    home = os.environ.get("CODEX_HOME") or os.path.join(os.path.expanduser("~"), ".codex")
    return os.path.join(home, "auth.json")


def jwt_payload(token):
    parts = token.split(".")
    if len(parts) != 3 or not parts[1]:
        raise Fail(AUTH, "Codex access token is not a JWT. Run `codex login` again.")
    try:
        part = parts[1].rstrip("=")
        return json.loads(base64.urlsafe_b64decode(part + "=" * (-len(part) % 4)))
    except Exception:
        raise Fail(AUTH, "Failed to decode Codex access token. Run `codex login` again.")


def load_credentials(path=None, now=None):
    path = path or auth_path()
    try:
        with open(path, encoding="utf-8") as f:
            raw = f.read()
    except OSError:
        raise Fail(AUTH, f"No Codex login found at {path}. Run `codex login` and sign in with ChatGPT.")
    try:
        auth = json.loads(raw)
    except ValueError:
        raise Fail(AUTH, f"Codex auth file {path} is not valid JSON. Run `codex login` again.")
    tokens = auth.get("tokens") if isinstance(auth, dict) else None
    tokens = tokens if isinstance(tokens, dict) else {}
    token = tokens.get("access_token")
    if not isinstance(token, str) or not token:
        hint = (" Codex is logged in with an API key; image generation here needs a ChatGPT login."
                if isinstance(auth, dict) and isinstance(auth.get("OPENAI_API_KEY"), str) else "")
        raise Fail(AUTH, f"Codex login has no ChatGPT access token.{hint} Run `codex login` and sign in with ChatGPT.")
    payload = jwt_payload(token)
    account = tokens.get("account_id")
    if not isinstance(account, str) or not account:
        claim = payload.get(JWT_CLAIM_PATH)
        account = claim.get("chatgpt_account_id") if isinstance(claim, dict) else None
        if not isinstance(account, str) or not account:
            raise Fail(AUTH, "Codex access token does not contain chatgpt_account_id. Run `codex login` again.")
    exp = payload.get("exp")
    exp = int(exp) if isinstance(exp, (int, float)) else None
    if exp is None or exp - (now if now is not None else time.time()) < EXPIRY_MARGIN_SECS:
        raise Fail(AUTH, f"Your Codex login has expired. {RENEW_HINT}")
    return {"token": token, "account": account, "exp": exp, "path": path}


# --- Images ---

def sniff(data):
    if data.startswith(b"\x89PNG\r\n\x1a\n"):
        return "png"
    if data.startswith(b"\xff\xd8\xff"):
        return "jpeg"
    if len(data) >= 12 and data[:4] == b"RIFF" and data[8:12] == b"WEBP":
        return "webp"
    return None


def load_input_images(paths):
    urls, total = [], 0
    for path in paths:
        if not os.path.isfile(path):
            raise Fail(OTHER, f"Unable to read reference image {path}: must be an existing regular file.")
        if os.path.getsize(path) > MAX_INPUT_IMAGE_BYTES:
            raise Fail(OTHER, f"Unable to read reference image {path}: Referenced image exceeds 20 MiB.")
        with open(path, "rb") as f:
            data = f.read(MAX_INPUT_IMAGE_BYTES + 1)
        fmt = sniff(data)
        if fmt is None:
            raise Fail(OTHER, f"Reference image is not PNG, JPEG or WebP: {path}")
        total += len(data)
        if total > MAX_TOTAL_INPUT_BYTES:
            raise Fail(OTHER, "Reference images exceed 50 MiB in total.")
        urls.append(f"data:image/{fmt};base64,{base64.b64encode(data).decode()}")
    return urls


def decode_image(b64):
    if not isinstance(b64, str) or not b64.strip():
        raise Fail(OTHER, "Codex did not return an image.")
    if len(b64) > -(-MAX_IMAGE_BYTES // 3) * 4:
        raise Fail(OTHER, "Codex image exceeded the 32 MiB size limit.")
    try:
        data = base64.b64decode(b64.strip(), validate=True)
    except ValueError:
        raise Fail(OTHER, "Codex returned invalid base64 image data.")
    fmt = sniff(data)
    if fmt is None:
        raise Fail(OTHER, "Codex returned image data that is not PNG, JPEG or WebP.")
    return data, fmt


# --- Backend ---

class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


OPENER = urllib.request.build_opener(NoRedirect)


def error_kind(error):
    if not isinstance(error, dict):
        return OTHER
    if error.get("code") in QUOTA_CODES or error.get("type") in QUOTA_CODES:
        return QUOTA
    if error.get("code") == "moderation_blocked" or error.get("type") == "image_generation_user_error":
        return MODERATION
    return OTHER


def http_failure(status, headers, body):
    """Return (code, message, retry) for an HTTP error response."""
    if headers.get("cf-mitigated") == "challenge":
        return OTHER, ("Codex connection was challenged by Cloudflare. This does not establish model "
                       "or subscription availability."), False
    try:
        error = json.loads(body).get("error")
    except Exception:
        error = None
    kind = error_kind(error)
    if status == 401:
        hint, kind = f"Codex login was rejected or has expired. {RENEW_HINT}", AUTH
    elif status == 403:
        hint = ("Codex access was denied. This can be a connection or account restriction; "
                "it does not identify the image model.")
    elif kind == QUOTA:
        hint = "Codex subscription quota is unavailable or exhausted. Check your plan or wait for its reset."
    elif kind == MODERATION:
        hint = "Codex could not generate this image. Review the prompt and input images before trying again."
    else:
        hint = "Codex could not complete the image request."
    retry = kind not in (QUOTA, MODERATION) and status in (429, 500, 502, 503, 504)
    return kind, f"Codex image request failed (HTTP {status}). {hint}", retry


def retry_delay(attempt, retry_after):
    try:
        secs = float(retry_after)
        if secs >= 0:
            return min(secs, MAX_RETRY_DELAY) * (1 + random.random() * 0.1)
    except (TypeError, ValueError):
        pass
    return min(2 ** (attempt - 1), MAX_RETRY_DELAY) * (0.9 + random.random() * 0.2)


def post(path, body, creds):
    data = json.dumps(body).encode()
    for attempt in range(1, MAX_RETRIES + 2):
        req = urllib.request.Request(f"{BASE_URL}/{path}", data=data, method="POST", headers={
            "Authorization": f"Bearer {creds['token']}",
            "chatgpt-account-id": creds["account"],
            "originator": "codex_cli_rs",
            "OpenAI-Beta": "responses=experimental",
            "accept": "application/json",
            "content-type": "application/json",
            "user-agent": "codex-img",
        })
        try:
            with OPENER.open(req, timeout=REQUEST_TIMEOUT) as resp:
                raw = resp.read(MAX_RESPONSE_BYTES + 1)
        except urllib.error.HTTPError as e:
            body_text = e.read(MAX_ERROR_BYTES).decode("utf-8", "replace")
            kind, message, retry = http_failure(e.code, e.headers, body_text)
            if retry and attempt <= MAX_RETRIES:
                time.sleep(retry_delay(attempt, e.headers.get("retry-after")))
                continue
            raise Fail(kind, message)
        except (urllib.error.URLError, OSError) as e:
            raise Fail(OTHER, f"Codex connection failed ({getattr(e, 'reason', e)}). No automatic retry was made; "
                              "check connectivity before trying again.")
        if len(raw) > MAX_RESPONSE_BYTES:
            raise Fail(OTHER, "Codex response exceeded the size limit.")
        try:
            return json.loads(raw)
        except ValueError:
            raise Fail(OTHER, "Codex returned an invalid JSON response. No automatic retry was made.")


def is_identifier(value, secrets):
    return (isinstance(value, str) and 1 <= len(value) <= 128
            and all(c.isascii() and (c.isalnum() or c in "_-") for c in value)
            and not any(s and s in value for s in secrets))


def reported(item, secrets):
    checks = {
        "model": lambda v: v.startswith("gpt-image-") and is_identifier(v.replace(".", "-"), secrets),
        "size": lambda v: v != "auto" and is_size(v),
        "quality": lambda v: v in ("low", "medium", "high", "xhigh", "max", "auto"),
        "background": lambda v: v in ("transparent", "opaque", "auto"),
    }
    return {k: item[k] for k, ok in checks.items() if isinstance(item.get(k), str) and ok(item[k])}


def sanitize_usage(usage):
    if not isinstance(usage, dict):
        return None
    counter = lambda v: isinstance(v, (int, float)) and not isinstance(v, bool) and v >= 0
    out = {k: usage[k] for k in ("input_tokens", "output_tokens", "total_tokens") if counter(usage.get(k))}
    for key, fields in (("input_tokens_details", ("cached_tokens", "image_tokens", "text_tokens")),
                        ("output_tokens_details", ("reasoning_tokens", "image_tokens", "text_tokens"))):
        details = usage.get(key) if isinstance(usage.get(key), dict) else {}
        picked = {f: details[f] for f in fields if counter(details.get(f))}
        if picked:
            out[key] = picked
    return out or None


def generate(opts, input_urls, creds):
    body = {"prompt": opts["prompt"], "model": IMAGE_MODEL}
    for key in ("size", "quality", "background"):
        if opts[key] is not None:
            body[key] = opts[key]
    if input_urls:
        body["images"] = [{"image_url": url} for url in input_urls]
    started = time.monotonic()
    resp = post("images/edits" if input_urls else "images/generations", body, creds)
    first = (resp.get("data") or [{}])[0] if isinstance(resp, dict) else {}
    first = first if isinstance(first, dict) else {}
    data, fmt = decode_image(first.get("b64_json"))
    secrets = (creds["token"], creds["account"])
    gen_id = first.get("generation_id")
    return {
        "bytes": data, "format": fmt,
        "id": gen_id if is_identifier(gen_id, secrets) else str(uuid.uuid4()),
        "reported": reported(resp, secrets), "usage": sanitize_usage(resp.get("usage")),
        "durationMs": int((time.monotonic() - started) * 1000),
    }


# --- Output ---

def output_path(output, ext, image_id, index, count):
    safe = "".join(c if c.isascii() and (c.isalnum() or c in "_-") else "_" for c in image_id)[-12:]
    generated = f"codex-img-{time.strftime('%Y%m%dT%H%M%S', time.gmtime())}-{safe}.{ext}"
    if not output:
        return os.path.join(os.getcwd(), generated)
    path = os.path.abspath(output)
    if output.endswith("/") or os.path.isdir(path):
        return os.path.join(path, generated)
    stem, current = os.path.splitext(path)
    if count == 1:
        return path if current else f"{path}.{ext}"
    return f"{stem}-{index + 1}{current or '.' + ext}"


def check_output(output, count):
    """Before any quota is spent: the files -o names must be free and their folder creatable.
    Names made up in a folder are unique. save() still refuses to overwrite."""
    if not output or output.endswith("/") or os.path.isdir(os.path.abspath(output)):
        return
    for index in range(count):
        target = output_path(output, "png", "", index, count)
        if os.path.lexists(target):
            raise Fail(OTHER, f"{target} already exists; codex-img never overwrites it. Choose another -o or delete it.")
        try:
            os.makedirs(os.path.dirname(target) or ".", exist_ok=True)
        except OSError as e:
            raise Fail(OTHER, f"Could not create {os.path.dirname(target)}: {e.strerror or e}")


def write_file(path, data):
    """Create path (never replacing a file) and write data, removing it again if the write fails."""
    try:
        f = open(path, "xb")
    except OSError as e:
        raise Fail(OTHER, f"Could not create {path}: {e.strerror or e}")
    try:
        with f:
            f.write(data)
    except OSError as e:
        with contextlib.suppress(OSError):
            os.remove(path)
        raise Fail(OTHER, f"Could not write {path}: {e.strerror or e}")


def save(data, path):
    """Write a new file that only appears once complete: written to a temporary file first, then
    hard-linked into place, which (unlike a rename) never replaces an existing file."""
    folder = os.path.dirname(path) or "."
    os.makedirs(folder, exist_ok=True)
    temp = os.path.join(folder, f".{os.path.basename(path)}.{uuid.uuid4().hex[:8]}.tmp")
    write_file(temp, data)
    try:
        os.link(temp, path)
    except FileExistsError as e:
        raise Fail(OTHER, f"Could not create {path}: {e.strerror or e}")
    except OSError:  # a filesystem without hard links
        write_file(path, data)
    finally:
        with contextlib.suppress(OSError):
            os.remove(temp)


def run_one(opts, input_urls, creds, index, lock, log):
    tag = f"[{index + 1}] " if opts["count"] > 1 else ""
    log(f"{tag}{'editing' if input_urls else 'generating'}")
    image = generate(opts, input_urls, creds)
    wanted = output_path(opts["output"], "png", image["id"], index, opts["count"])
    path, warning = wanted, None
    if image["format"] != "png":
        ext = "jpg" if image["format"] == "jpeg" else image["format"]
        path = os.path.splitext(wanted)[0] + "." + ext
        warning = f"backend returned {image['format']}, not png; saved as {path}"
    save(image["bytes"], path)
    info = {"path": path, "format": image["format"], "bytes": len(image["bytes"]), "transport": "direct"}
    rep = image["reported"]
    if "model" in rep:
        info["imageModel"] = rep["model"]
    info.update({k: rep[k] for k in ("size", "quality", "background") if k in rep})
    info["generationId"] = image["id"]
    if image["usage"]:
        info["usage"] = image["usage"]
    info["durationMs"] = image["durationMs"]
    with lock:
        if warning:
            print(f"codex-img: {tag}warning: {warning}", file=sys.stderr)
        print(json.dumps(info) if opts["json"] else path, flush=True)
    log(f"{tag}saved {path} ({info.get('size', '?')}, {image['durationMs'] / 1000:.1f}s)")


def main(args):
    try:
        cmd = parse(args)
    except Fail as e:
        print(f"codex-img: {e}\nRun `codex_img.py --help` for usage.", file=sys.stderr)
        return USAGE
    try:
        if cmd["command"] == "help":
            print(HELP)
            return 0
        if cmd["command"] == "version":
            print(VERSION)
            return 0
        if cmd["command"] == "status":
            creds = load_credentials()
            expires = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(creds["exp"]))
            if cmd["json"]:
                print(json.dumps({"ok": True, "authPath": creds["path"], "accountId": creds["account"], "expiresAt": expires}))
            else:
                print(f"Logged in (account {creds['account']}); token valid until {expires}.\nAuth file: {creds['path']}")
            return 0
        opts = cmd
        if opts["prompt"] == "-":
            opts["prompt"] = sys.stdin.read().strip()
        if not opts["prompt"].strip() or len(opts["prompt"]) > MAX_PROMPT_CHARS:
            raise Fail(USAGE, "Image prompt must contain 1 to 32,000 characters.")
        check_output(opts["output"], opts["count"])
        log = (lambda m: None) if opts["quiet"] else (lambda m: print(m, file=sys.stderr, flush=True))
        creds = load_credentials()
        input_urls = load_input_images(opts["images"])
        n = opts["count"]
        refs = f" with {len(input_urls)} reference(s)" if input_urls else ""
        log(f"Requesting {f'{n} images' if n > 1 else 'image'}{refs}...")
    except Fail as e:
        print(f"codex-img: {e}", file=sys.stderr)
        return e.code

    lock, codes = threading.Lock(), []

    def worker(index):
        try:
            run_one(opts, input_urls, creds, index, lock, log)
        except Fail as e:
            with lock:
                print(f"codex-img: {e}", file=sys.stderr)
                codes.append(e.code)
        except Exception as e:  # keep the exit-code contract even on bugs
            with lock:
                print(f"codex-img: unexpected error: {e}", file=sys.stderr)
                codes.append(OTHER)

    threads = [threading.Thread(target=worker, args=(i,)) for i in range(n)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    return max(codes, default=0)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
