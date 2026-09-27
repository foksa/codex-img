# codex-img

Generate images from the command line with your ChatGPT/Codex subscription. It needs no API key and doesn't run the `codex` agent.

`codex-img` calls the same image endpoint the Codex CLI uses, `chatgpt.com/backend-api/codex/images/generations`, or `/images/edits` when you pass reference images. That's one JSON request per image, with no chat model in between. It reuses the ChatGPT login that `codex login` saved in `~/.codex/auth.json`.

## Install

```sh
./scripts/install.sh    # needs a Rust toolchain (https://rustup.rs)
```

This builds `target/release/codex-img`, a single ~1.4 MB binary with no runtime dependencies, and symlinks:
- the binary to `~/.local/bin/codex-img` (override with `BIN_DIR`);
- the agent skill `skills/codex-img` into `~/.claude/skills/` and `~/.codex/skills/`, for whichever of those tools you have installed.

You need to be logged in once with `codex login` (ChatGPT sign-in). `codex-img status` checks the login offline, without using any quota.

## For agents

`skills/codex-img/SKILL.md` is an Agent Skill. Claude Code, Codex and other skill-aware agents load it automatically when a task involves making or editing an image. It covers the commands to run, when to spend quota, how to handle each exit code (don't retry auth or quota errors), and prompt-writing tips. Longer recipes are in `skills/codex-img/references/prompting.md`.

## Usage

```sh
codex-img "a red fox in snow, flat vector"                  # -> ./codex-img-<time>-<id>.png
codex-img "app icon, paper plane" -o icon.webp
codex-img "wide landscape of a lighthouse" --size 1536x1024
codex-img "make it night with aurora" -i fox.png -o fox-night.jpg
codex-img "sticker of a cat" --background transparent -n 4 -o stickers/
echo "long prompt..." | codex-img - --json
codex-img status --json                                     # login check, uses no quota
```

| Option | |
|---|---|
| `-o, --output` | File or directory. A trailing `/` or an existing directory gets generated names. With `-n`, files get `-1`, `-2`, … suffixes. Existing files are never overwritten. |
| `-i, --image` | Reference image to edit or compose (PNG/JPEG/WebP, repeatable, max 5) |
| `-f, --format` | `png` \| `jpeg` \| `webp`. Defaults to the `-o` extension, then `png`. The endpoint returns PNG; `jpeg` is converted locally, with transparency flattened onto white; `webp` needs `--via-responses` |
| `-s, --size` | `WxH` or `auto` |
| `-q, --quality` | `low` \| `medium` \| `high` \| `auto` |
| `-b, --background` | `transparent` \| `opaque` \| `auto` |
| `--via-responses` | Fallback route: a routing model calls the `image_generation` tool through the Responses API. The prompt may be rewritten and `--size` is ignored, but it can return WebP |
| `-m, --model` | Routing model for `--via-responses` (default `gpt-5.5`) |
| `-n, --count` | Images to generate in parallel (1–10). Each one is a separate request |
| `--json` | Prints one JSON line per image: path, size, quality, revised prompt, usage, duration |
| `--quiet` | No progress output on stderr |

Paths go to stdout and progress goes to stderr, so the tool composes well in scripts and agent tools.
Exit codes: `0` ok, `1` error, `2` auth, `3` quota, `4` moderation, `64` usage.

## Notes

- **Routes.** By default `codex-img` does what the Codex CLI does: it sends `{prompt, model: "gpt-image-2", size, quality, background}` to `/images/generations`, or to `/images/edits` with `images: [{image_url: "data:…"}]`. Your prompt reaches the image model unchanged. `--via-responses` uses the Responses API with the `image_generation` tool instead (ported from pi-codex-image-gen).
- **Model.** The backend picks the image model and ignores the `model` field; the Codex CLI hardcodes `gpt-image-2` too. The Responses route reports it as `gpt-image-2-codex`. Whenever the backend upgrades what it serves (such as Images 2.5), both routes get the upgrade.
- **What's honoured.** `size` is a hint: `1536x1024` has come back at 1536x1024 and also at 1370x1148, and square comes back around 1254x1254. Check `size` in the `--json` output. `background: transparent` gives real alpha. `quality` is capped at medium on the subscription. The endpoint doesn't validate its inputs and ignores unknown values, so `codex-img` checks them before sending.
- `codex-img` only reads `auth.json`. It never refreshes or writes tokens, so it can't interfere with your `codex` login. If the login has expired or gets rejected, it exits with code `2` and tells you to open Codex (the `codex` CLI or the app) so it renews the login, or to run `codex login`. `CODEX_HOME` overrides `~/.codex`.
- The image model is chosen by the backend. It is currently `gpt-image-2-codex`, reported as `imageModel` in `--json`. `--model` only picks the routing model.
- Debugging: `CODEX_IMG_DEBUG_RAW=/tmp/raw.txt codex-img …` saves the raw response body (the JSON, or the event stream with `--via-responses`). It contains the full base64 image.
- Image generation uses your subscription's image quota.

## Credits

The request format and stream parser are ported from [pi-codex-image-gen](https://github.com/jvm/pi-mono/tree/main/packages/pi-codex-image-gen) (Apache-2.0). See `NOTICE`.
