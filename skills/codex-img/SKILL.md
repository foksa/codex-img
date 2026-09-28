---
name: codex-img
description: Generate or edit raster images (PNG/JPEG/WebP) with the `codex-img` CLI, which uses the user's ChatGPT/Codex subscription. Use when the user asks to create, draw, render, or edit a picture, photo, illustration, icon, sticker, mockup, or other bitmap asset, or to change an existing image with AI. Not for charts, diagrams, or vector/SVG art that code can produce exactly.
---

# codex-img

`codex-img` sends one request to the Codex image endpoint (the same one the Codex CLI uses) and saves the result to disk. Your prompt goes to the image model exactly as written; nothing rewrites it, so the prompt you write is the prompt that gets rendered. Each call uses the user's image quota, so only generate when the user has asked for an image, and don't produce extra variations nobody asked for.

## Quick reference

```sh
codex-img "<prompt>" -o <path> --json                  # generate
codex-img "<prompt>" -i <image> -o <path> --json       # edit / use as reference (repeat -i, max 5)
codex-img "<prompt>" -n 3 -o <dir>/ --json             # 3 variations in parallel (only if asked)
codex-img status --json                                # check login, uses no quota
codex-img convert <file>... [-o <path>] [-f fmt] [-c n] --json   # convert existing images, uses no quota
```

| Option | Values |
|---|---|
| `-o` | File (`hero.png`) or directory (`assets/`). Format is inferred from the extension. Existing files are never overwritten, so choose a new name for each iteration. |
| `-i` | PNG, JPEG or WebP input, repeatable up to 5 |
| `-s` | `1536x1024` (landscape), `1024x1536` (portrait), `auto`; a hint for the shape, not exact pixels |
| `-b` | `transparent` for real alpha (PNG only), `opaque`, `auto` |
| `-q` | `low` \| `medium` \| `high` \| `auto`; a hint, and the subscription caps it at medium |
| `-f` | `png` (native), `jpeg` (converted locally; transparency becomes white) or `webp` (lossless, keeps transparency). Use PNG unless the user wants another format. |
| `-c` | (PNG is always recompressed losslessly, so plain PNG files are already about half the backend's size.) Quantize PNG to a palette of 2–256 colours, keeping transparency. For icons, stickers, logos and flat illustrations meant for the web or an app, `-c 64` to `-c 256` usually cuts the file 10x or more with no visible change. Don't use it for photos or soft gradients; if banding shows, raise the count or add `--dither`. |

Use `-` as the prompt to read it from stdin. That avoids shell quoting problems for long prompts:

```sh
codex-img - -o out.png --json <<'EOF'
multi-line prompt here, with "quotes" and $symbols
EOF
```

## Converting and shrinking existing images

`codex-img convert` handles PNG, JPEG and WebP files locally, with no login and no quota. Use it instead of ImageMagick or Pillow for format changes and size reduction, including on images you generated earlier. Don't call the image model just to change the format.

- `codex-img convert in.png -o out.webp`, or `-f jpeg`, changes the format.
- `codex-img convert icon.png -c 64` writes `icon.min.png`, a palette PNG, for flat art.
- `codex-img convert in.png` writes `in.min.png`, lossless recompression only.
- With several inputs, `-o` must be a directory ending in `/`.

It never overwrites existing files or the input. It doesn't resize; use `sips` or ImageMagick for that. Keep prompts that start with the word "convert" quoted, because a bare `convert` as the first argument runs this subcommand.

## Workflow

1. Write the prompt (see below). Save to the path the user wants, or to a sensible project location such as `assets/`. Don't clutter the repo root.
2. Run with `--json`, and allow a timeout of at least 5 minutes: a single image usually takes 20–60s. stdout has one JSON line per image, with `path`, `size`, `durationMs` and more. Progress goes to stderr. A `warning:` line on stderr means the file was saved under a different extension than requested; use the `path` from the JSON.
3. **Look at the result** (open or read the image file) before reporting back. Check that it matches the request, especially any text, counts and composition.
4. To refine, run again with the previous output as `-i` and a prompt that changes one thing ("change only X; keep everything else unchanged").

## Exit codes, and what to do

| Code | Meaning | Action |
|---|---|---|
| 0 | Success | Paths are on stdout |
| 2 | Login missing, expired or rejected | **Don't retry.** Tell the user to open Codex (`codex` CLI or app) so it renews the login, or run `codex login`. |
| 3 | Subscription image quota used up | **Don't retry.** Tell the user and stop. |
| 4 | Blocked by moderation | Don't resubmit the same prompt. Explain, and ask before rephrasing anything sensitive. |
| 1 | Network, timeout or backend error | The backend may already have used quota. Tell the user, and retry at most once. |
| 64 | Bad arguments | Fix the command |

## If `codex-img` isn't installed

When `command -v codex-img` finds nothing, use the bundled fallback, `scripts/codex_img.py` in this skill's directory (Python 3.9+, standard library only). It takes the same arguments and has the same exit codes and `--json` output:

```sh
python3 <skill-dir>/scripts/codex_img.py "<prompt>" -o <path>.png --json
```

Its limits:
- **PNG only.** It refuses `-f`, and any `-o` extension other than `.png`, before spending quota. If the user wants JPEG or WebP, or exact pixel dimensions, generate a PNG and then convert or resize it yourself (`sips`, ImageMagick, Pillow).
- **No `-c`/`--dither` quantization, no `convert` subcommand, and no `--via-responses` or `--model`.**

If `python3` is missing as well, tell the user rather than trying another image service. They can install the binary from the project's releases page.

## Behaviour to expect

- `-s` is a hint. The backend picks the final pixels: a `1536x1024` request has come back at 1536x1024 and also at 1370x1148, and square usually comes back around 1254x1254. Also state the shape in the prompt ("wide landscape"), check `size` in the JSON, and if you need exact dimensions, resize or crop afterwards (for example with `sips -z 1024 1024 in.png --out out.png` on macOS, or ImageMagick) and say so.
- Edits keep the input image's framing and aspect ratio, and change only what the prompt asks for.
- The backend chooses the image model and ignores any model name. Asking for a specific model such as "Images 2.5" in the prompt doesn't change it either. `size` and `quality` in the JSON are what the backend reported.
- Leave `--via-responses` and `--model` alone. They're a fallback route that rewrites the prompt and ignores `--size`; use them only if the default route is failing.

## Writing prompts

Order: scene/background → subject → key details → constraints → intended use.

- Say what the image is for ("App Store icon", "hero image for a landing page", "product photo for a catalogue"). That sets the level of polish.
- If the user's request is already detailed, pass it through cleanly. If it's vague, add only framing, lighting and style cues. Don't add characters, props, slogans or brand colours that weren't implied.
- Text in the image: put the exact words in quotes, specify font style, colour and placement, and ask for "verbatim, no extra text". Check the spelling in the result.
- Edits: "Change only <X>. Keep <Y> unchanged." Repeat the things that must stay the same on every iteration.
- Several inputs: label them by role ("Image 1: the product to keep; Image 2: style reference only").
- Photorealism: say `photorealistic`, and add camera and lighting language plus real-world texture.
- Transparent assets: use `-b transparent -o x.png`, and ask for a clean isolated subject with no background, shadow or floor.

More recipes by use case: [references/prompting.md](references/prompting.md).
