---
name: codex-img
description: Generate or edit raster images (PNG/JPEG/WebP) with the `codex-img` CLI, which uses the user's ChatGPT/Codex subscription. Use when the user asks to create, draw, render, or edit a picture, photo, illustration, icon, sticker, mockup, or other bitmap asset, or to change an existing image with AI. Not for charts, diagrams, or vector/SVG art that code can produce exactly.
---

# codex-img

`codex-img` sends one request to the Codex image endpoint (the same one the Codex CLI uses) and saves the result to disk. Your prompt goes to the image model as written. The only additions are sentences that options ask for, like `-a`'s frame or `--palette`'s colours, and `--json` shows the full prompt as `submittedPrompt`. Each call counts against the user's ChatGPT subscription limits (exactly how isn't known), so only generate when the user has asked for an image, and don't produce extra variations nobody asked for.

## Read when

| Task | Read |
|---|---|
| Writing or refining any prompt; use-case recipes (icons, products, UI, slides, edits) | [references/prompting.md](references/prompting.md) |
| Game sprites, a set of assets, camera angles, characters, palettes, `batch`, `sheet`, `tile` | [references/game-assets.md](references/game-assets.md) |
| Changing format, size or file weight of existing images, removing a painted-in background | [references/convert.md](references/convert.md) |
| `codex-img` is not installed (`command -v codex-img` finds nothing) | [references/fallback.md](references/fallback.md) |

## Quick reference

```sh
codex-img "<prompt>" -o <path> --json                          # generate
codex-img "<prompt>" -a 16:9 -o <path> --json                  # with a frame shape
codex-img "<prompt>" -i <image> -o <path> --json               # edit an image (repeat -i, max 5 images in all)
codex-img "<prompt>" --style-ref <image> -o <path> --json      # new image in that image's style
codex-img "<prompt>" -n 3 -o <dir>/ --json                     # 3 variations in parallel (only if asked)
codex-img presets                                              # views, styles, characters, palettes; no quota
codex-img status --json                                        # check login, no quota
codex-img convert <file> -o <out.webp> --json                  # convert/trim/resize locally, no quota
```

| Option | Values |
|---|---|
| `-o` | File (`hero.png`) or directory (`assets/`). Format is inferred from the extension. Existing files are never overwritten, so choose a new name for each iteration; a taken name fails at once (exit `1`), before any quota is spent. |
| `-i` | The image to edit (PNG, JPEG or WebP). For images that only guide a new one, use the role options below. |
| `--style-ref`, `--character-ref`, `--composition-ref` | Reference images with a role: style only, the same character, or layout only. Each gets a line in the prompt saying how to use it, numbered after the `-i` images. Max 5 images in all. A reference sets the output's frame, and its setting tends to carry over, so pass `-a` and describe the new background. |
| `-a` | Frame shape `W:H`, from 1:3 to 3:1 (`16:9`, `3:2`, `1:1`, `2:3`, `9:16`). It's what sets the shape: `-s` is ignored by the backend. About 1.57 megapixels (2:3 is 1024x1536, 16:9 is 1672x941); for exact pixels add `--resize WxH --fit cover`. |
| `-b` | `transparent` for real alpha (PNG only), `opaque`, `auto`. For transparent assets also ask for a clean isolated subject with no background, shadow or floor. |
| `--view` | Camera preset: `side`, `front`, `top-down`, `three-quarter`, `isometric`, or the project's own. For game art, use it instead of describing the angle. |
| `--style`, `--character` | Named presets of the project (`codex-img presets` lists them). Check before writing style or character text by hand. |
| `--palette` | A fixed palette: hex codes, a file, or a preset (`pico-8`, `nes`, `sweetie-16`, ...). Adds the hex codes to the prompt and snaps every colour to the palette. Never name a palette in the prompt without it: the model doesn't know the colours. |
| `-f` | `png` (default), `jpeg`, or `webp` (lossy, keeps transparency). For images going on a website, lossy `webp` is usually the smallest by far. |
| `-c` | Quantize PNG to 2–256 colours. For icons, stickers and flat art, `-c 64` to `-c 256` usually cuts the file 10x or more with no visible change. Not for photos or gradients. |
| `--trim`, `--resize`, `--hard-alpha` | Applied before saving, as in `convert`; useful for sprites: `-b transparent --trim=4 --resize 400x`. These and `--palette` keep the untouched original as `<name>.raw.png` (`rawPath` in the JSON); keep it, since there's no seed to regenerate it. |
| `-q` | `low` \| `medium` \| `high` \| `auto`; a hint, and the subscription caps it at medium. |

Use `-` as the prompt to read it from stdin, which avoids shell quoting problems:

```sh
codex-img - -o out.png --json <<'EOF'
multi-line prompt here, with "quotes" and $symbols
EOF
```

A prompt whose first word is `convert`, `sheet`, `batch`, `tile` or `presets` runs that subcommand instead, so start the prompt with another word.

## Workflow

1. Write the prompt (see below). Save to the path the user wants, or to a sensible project location such as `assets/`. Don't clutter the repo root.
2. Run with `--json`, and allow a timeout of at least 5 minutes: a single image usually takes 20–60s.
   - stdout has one JSON line per image, with `path`, `size`, `durationMs`, and `submittedPrompt` when options added to the prompt.
   - Progress goes to stderr.
   - A `warning:` line on stderr means one of these:
     - the file was saved under a different extension than requested;
     - it was left unprocessed (for example `--trim` found nothing visible);
     - the `.raw` original couldn't be kept;
     - the frame came back off the `-a` ratio.

     Use the `path` from the JSON.
3. **Look at the result** (open or read the image file) before reporting back. Check that it matches the request, especially any text, counts and composition.
4. To refine, run again with the previous output as `-i` and a prompt that changes one thing ("change only X; keep everything else unchanged").

## Exit codes, and what to do

| Code | Meaning | Action |
|---|---|---|
| 0 | Success | Paths are on stdout |
| 2 | Login missing, expired or rejected | **Don't retry.** Tell the user to open Codex (`codex` CLI or app) so it renews the login, or run `codex login`. |
| 3 | Quota or usage limit reached | **Don't retry.** Tell the user and stop. |
| 4 | Blocked by moderation | Don't resubmit the same prompt. Explain, and ask before rephrasing anything sensitive. |
| 1 | Network, timeout or backend error | The backend may already have used quota. Tell the user, and retry at most once. |
| 64 | Bad arguments | Fix the command |

## Behaviour to expect

- **The prompt sets the shape, and `-s` doesn't.** In tests, a portrait and a landscape `-s` both came back nearly square.
  - Use `-a W:H` whenever the shape matters, and check `size` in the JSON.
  - With `-a`, the backend has reported `quality: low` and used fewer image tokens, yet the images looked as detailed. Don't treat that `low` as a failure.
- **Edits keep the input image's framing and aspect ratio,** unless you pass `-a`, which reframes the scene around the subject. They change only what the prompt asks for.
- **The backend chooses the image model.** `codex-img` can't pick one, and naming a model such as "Images 2.5" in the prompt doesn't select it. `size` and `quality` in the JSON are what the backend reported.
- **Leave `--via-responses` and `--model` alone.** They're a fallback route that rewrites the prompt and ignores `--size`; use them only if the default route is failing.

## Writing prompts

Order: scene/background → subject → key details → constraints → intended use. The full guide, a labeled-line template and recipes are in [references/prompting.md](references/prompting.md).

- **Say what the image is for** ("App Store icon", "hero image for a landing page", "product photo for a catalogue"). That sets the level of polish.
- **Keep detailed requests as they are.** If the user's request is already detailed, pass it through cleanly.
- **Add little to vague requests.** Add only framing, lighting and style cues. Don't add characters, props, slogans or brand colours that weren't implied.
- **Text in the image:** put the exact words in quotes, specify font style, colour and placement, and ask for "verbatim, no extra text". Check the spelling in the result.
- **Edits:** "Change only <X>. Keep <Y> unchanged." Repeat the things that must stay the same on every iteration.
- **Room for copy or UI:** ask for negative space, but don't choose a side unless the layout needs one.
- **Photorealism:** say `photorealistic`, and add camera and lighting language plus real-world texture.
