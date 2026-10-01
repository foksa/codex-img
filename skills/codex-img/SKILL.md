---
name: codex-img
description: Generate or edit raster images (PNG/JPEG/WebP) with the `codex-img` CLI, which uses the user's ChatGPT/Codex subscription. Use when the user asks to create, draw, render, or edit a picture, photo, illustration, icon, sticker, mockup, or other bitmap asset, or to change an existing image with AI. Not for charts, diagrams, or vector/SVG art that code can produce exactly.
---

# codex-img

`codex-img` sends one request to the Codex image endpoint (the same one the Codex CLI uses) and saves the result to disk. Your prompt goes to the image model exactly as written; nothing rewrites it, so the prompt you write is the prompt that gets rendered. Each call counts against the user's ChatGPT subscription limits (exactly how isn't known), so only generate when the user has asked for an image, and don't produce extra variations nobody asked for.

## Quick reference

```sh
codex-img "<prompt>" -o <path> --json                  # generate
codex-img "<prompt>" -a 16:9 -o <path> --json          # with a frame shape
codex-img "<prompt>" -i <image> -o <path> --json       # edit / use as reference (repeat -i, max 5)
codex-img "<prompt>" --view side -o <path> --json      # camera preset (side, front, top-down, three-quarter, isometric)
codex-img "<prompt>" --character captain -o <path> --json   # named preset (also --style); `codex-img presets` lists them
codex-img "<prompt>" -n 3 -o <dir>/ --json             # 3 variations in parallel (only if asked)
codex-img status --json                                # check login, uses no quota
codex-img convert <file>... [-o <path>] [-f fmt] [-c n] [--trim] [--resize WxH] --json   # convert/trim/resize, no quota
```

| Option | Values |
|---|---|
| `-o` | File (`hero.png`) or directory (`assets/`). Format is inferred from the extension. Existing files are never overwritten, so choose a new name for each iteration. A taken name fails at once (exit `1`), before any quota is spent. |
| `-i` | PNG, JPEG or WebP input, repeatable up to 5 |
| `-a` | Frame shape `W:H`, from 1:3 to 3:1: `16:9`, `3:2`, `1:1`, `2:3`, `9:16`. It starts the prompt with a sentence asking for that ratio, which is what sets the shape; a `warning:` on stderr means it came back more than 2% off. The backend keeps about 1.57 megapixels (2:3 is 1024x1536, 16:9 is 1672x941), so for exact pixels add `--resize WxH --fit cover` |
| `-s` | Ignored by the backend in tests; use `-a` for the shape |
| `-b` | `transparent` for real alpha (PNG only), `opaque`, `auto` |
| `--trim`, `--resize`, `--fit`, `--hard-alpha` | Same as in `convert` (below), applied before saving. Useful for sprites: `-b transparent --trim=4 --resize 400x`. The untouched original is also saved as `<name>.raw.png` and reported as `rawPath`; keep it, since there's no seed to regenerate it. `size` is then the saved file's, and `rawSize` the backend's |
| `-q` | `low` \| `medium` \| `high` \| `auto`; a hint, and the subscription caps it at medium |
| `-f` | `png` (native), `jpeg` (converted locally; transparency becomes white) or `webp` (lossy, keeps transparency; tune with `--output-quality 1-100`, default 80, or use `--lossless` for exact pixels). Use PNG unless the user wants another format; for images going on a website, lossy `webp` is usually the smallest by far (a 946 KB PNG became 7.5 KB). |
| `-c` | (PNG is always recompressed losslessly, so plain PNG files are already about half the backend's size.) Quantize PNG to a palette of 2–256 colours, keeping transparency. For icons, stickers, logos and flat illustrations meant for the web or an app, `-c 64` to `-c 256` usually cuts the file 10x or more with no visible change. Don't use it for photos or soft gradients; if banding shows, raise the count or add `--dither`. |

Use `-` as the prompt to read it from stdin. That avoids shell quoting problems for long prompts:

```sh
codex-img - -o out.png --json <<'EOF'
multi-line prompt here, with "quotes" and $symbols
EOF
```

## Presets and reference roles

- **Camera angle for game art: use `--view` instead of describing it.**
  - `side` or `front`: flat elevations with no top surface and a straight bottom edge, for pseudo-3D roadside sprites and side-scrollers.
  - `top-down`: straight down, for top-down racers, shooters and map tiles. Buildings may still show a sliver of wall.
  - `three-quarter`: from above, front square to the camera, for classic RPGs.
  - `isometric`: 2:1, turned 45°.
  - The project may define its own views; check `codex-img presets`.
- **Before writing style or character text by hand,** run `codex-img presets` (no quota). If the project defines what you need, use `--style <name>` and `--character <name>`, or the batch fields `view`, `style` and `character`. That keeps a set consistent.
- **To keep a character across images,** make one clean anchor image. Then create a preset whose text describes only the character's looks: `codex-img presets add character <name> --ref <anchor.png> --text "<looks only>"`. Never use the anchor's whole prompt as the text: "isolated on a transparent background", or a pose, would end up in every scene. Then pass `--character <name>` and describe the scene. Add `--global` only when the user wants it in every project, and `presets promote` moves a project preset there later.
- **A reference with a role, without a preset:** use `--style-ref`, `--character-ref` or `--composition-ref <image>` instead of `-i`. Each is labelled in the prompt for you, after the `-i` images, with at most 5 images in all.
  - Any reference image sets the frame (pass `-a` for another shape).
  - Its setting tends to carry over, so describe the new background.
- **`--json` shows `submittedPrompt`,** the full prompt that was sent. Check it when a result surprises you.
- **`--manifest`** writes `<image>.json` with that prompt, the presets and the inputs. `batch` always writes one beside each raw image, and its `skip` line says when the spec has changed since; delete the raw image to re-roll it.

## Converting, trimming and resizing existing images

`codex-img convert` handles PNG, JPEG and WebP files locally, with no login and no quota. Use it instead of ImageMagick, Pillow or a custom script for format changes, cropping to content, resizing and size reduction, including on images you generated earlier. Don't call the image model just to change the format or size.

- `codex-img convert in.png -o out.webp`, or `-f jpeg`, changes the format. Add `--output-quality N` to trade size for quality.
- `codex-img convert icon.png -c 64` writes `icon.min.png`, a palette PNG, for flat art.
- `codex-img convert in.png` writes `in.min.png`, lossless recompression only.
- `--trim` crops transparent borders to the visible pixels (alpha above 16, so the faint specks generated images leave on the background don't count); `--trim=4` keeps 4 transparent pixels around them. An image with no transparent border is left as it is. `--json` reports the crop as `trim: {x, y, width, height}` in input pixels, for placing sprites.
- `--resize 400x` (or `x300`) keeps the aspect ratio. With `--resize WxH`, `--fit` decides: `inside` (default; fits in the box, one side may be smaller), `cover` (exactly WxH, crops the centre), `contain` (exactly WxH, transparent padding), `fill` (stretches). Trim runs first, then resize. Add `--no-enlarge` for a maximum size: smaller images keep their size instead of being scaled up.
- Game sprites: `codex-img convert raw/car.png --trim=4 --resize 400x -o sprites/`, or the same flags when generating. Keep the raw generated image; there's no seed to regenerate it.
- Generated transparent images have soft, slightly see-through alpha (even "solid" areas come back at 250–254). For pixel art or any style with crisp edges, add `--hard-alpha`: every pixel becomes fully solid or fully transparent, and it stays that way through `--resize` and `-c`. Compare one result with the project's existing art to decide. A full pixel-art sprite step: `codex-img convert raw/car.png --hard-alpha --trim --resize 400x300 --no-enlarge -c 160 -o assets/`.
- With several inputs, `-o` must be a directory ending in `/`.
- Painted-in background: the model sometimes paints a background despite "no ground" or "transparent background" in the prompt, most often sea under boats and docks, or a sky behind a building. Remove it with `--key auto` and a `--key-region` whose edges really are background. `auto` samples the background's colours along those edges, and `--key` flood-fills from the transparent area and the border, so matching paint inside the outline survives; leftover foam and specks go too.
  - Ground under a sprite: `--key auto --key-region bottom:30% --key-cut --trim --trim-density 0.15` (after `--hard-alpha` for pixel art). `--trim-density 0.15` stops leftover specks from becoming the bottom edge and making the sprite float.
  - Sky behind an object: `--key auto --key white --key-region top:80% --key-spread 16 --trim`. `--key-spread` follows the sky's gradient, and `white` takes clouds with outlines. Scenery close to the sky's colour (hazy mountains) goes too; use a shallower region to keep it.
  - Never point `auto` at an edge that is the object itself (a trunk, wheels): it would key it away. If `auto` gets it wrong, name the colours (`--key blue --key white`, `--key '#3070c0:40'`). Check the result on a `sheet`.

Resizing uses premultiplied alpha, so the colour stored under transparent pixels (generated images often hide a dark vignette there) can't bleed into the edges. PNG and lossless WebP output also gets the nearest visible colour written under transparent pixels, so engines and other tools that filter without premultiplying don't show a dark halo either. `--no-bleed` turns that off. You don't need to clean transparent pixels yourself.

It never overwrites the input, and existing files only with `--force`: re-running a pipeline into the same `-o` directory then rewrites just the files whose output changed (the same input and options always give the same bytes). Keep prompts that start with the word "convert" quoted, because a bare `convert` as the first argument runs this subcommand.

### Reviewing a batch: `codex-img sheet`

`codex-img sheet assets/harbor/*.png -o /tmp/harbor-sheet.png` lays the images out in one labelled grid. Look at that one image instead of opening each file. Sprites stand on a common baseline, so floating sprites, leftover background patches and style drift across a set stand out. `--same-scale` keeps relative sizes, `--force` replaces an earlier sheet, and `--bg '#rrggbb'` changes the background (default: muted green). Write sheets outside the project (for example to `/tmp`), because they're for review, not assets.

### Repeating backgrounds: `codex-img tile`

For a sky or backdrop that repeats side by side, run `codex-img tile sky.png -o sky-tile.png --preview /tmp/join.png` after generating it. The panorama then wraps around seamlessly, so it doesn't need to be mirrored, which shows landmarks twice. It uses one image of quota and keeps the size, framing and every pixel away from the join. Add `--prompt "what the picture shows"` if the join comes out wrong. Always look at the `--preview` image, which shows the join in the middle. Don't prompt landmarks onto an edge as a workaround for mirroring.

### Many assets: `codex-img batch`

For a set of assets, such as a game's art, keep them in a JSON spec and run `codex-img batch spec.json [key or folder...]` instead of scripting many calls. Each asset has a prompt, generation fields (`aspect: "3:2"` for the shape) and `convert` options with underscores (`hard_alpha`, `max: [W, H]`). The spec also has a shared `style` appended to every prompt, `defaults`, and `reference` keys for edits of another asset's raw image. `batch` generates only missing raw images (delete one to re-roll it) and converts all of them, rewriting only files that changed. Run `--dry-run` first to see what would be generated, because that uses quota. `--convert-only` needs no login. Each convert line reports the output's final size (`-> public/assets/tree.png (420x156, 18 KB)`, or `size` with `--json`), so read sizes and aspect ratios from there instead of opening every file. See `codex-img batch --help` for the spec format.

## Workflow

1. Write the prompt (see below). Save to the path the user wants, or to a sensible project location such as `assets/`. Don't clutter the repo root.
2. Run with `--json`, and allow a timeout of at least 5 minutes: a single image usually takes 20–60s. stdout has one JSON line per image, with `path`, `size`, `durationMs` and more. Progress goes to stderr. A `warning:` line on stderr means the file was saved under a different extension than requested, or unprocessed (for example `--trim` found nothing visible), or that the `.raw` original couldn't be kept, or that the frame came back off the `-a` ratio; use the `path` from the JSON.
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

## If `codex-img` isn't installed

When `command -v codex-img` finds nothing, use the bundled fallback, `scripts/codex_img.py` in this skill's directory (Python 3.9+, standard library only). It takes the same arguments and has the same exit codes and `--json` output:

```sh
python3 <skill-dir>/scripts/codex_img.py "<prompt>" -o <path>.png --json
```

Its limits:
- **PNG only.** It refuses `-f`, and any `-o` extension other than `.png`, before spending quota. If the user wants JPEG or WebP, or exact pixel dimensions, generate a PNG and then convert or resize it yourself (`sips`, ImageMagick, Pillow).
- **No `-c`/`--dither` quantization, no `--output-quality`/`--lossless`, no `convert` subcommand (so no trim, resize or edge bleed), and no `--via-responses` or `--model`.**
- **Presets:** only the built-in `--view`s and the `--*-ref` options; no `--style`, `--character`, project views, `presets` or `--manifest`.

If `python3` is missing as well, tell the user rather than trying another image service. They can install the binary from the project's releases page.

## Behaviour to expect

- The prompt sets the shape, and `-s` doesn't: in tests a portrait and a landscape `-s` both came back nearly square. Use `-a W:H` whenever the shape matters, check `size` in the JSON, and if you need exact dimensions, add `--resize WxH --fit cover` (with the fallback script: `sips -z H W in.png --out out.png` on macOS, or ImageMagick) and say so.
- With `-a`, the backend has reported `quality: low` and used fewer image tokens, but the images looked as detailed as without it. Don't treat that `low` as a failure.
- Edits keep the input image's framing and aspect ratio unless you pass `-a`, which reframes the scene around the subject. They change only what the prompt asks for.
- The backend chooses the image model; `codex-img` can't pick one, and naming a model such as "Images 2.5" in the prompt doesn't select it. `size` and `quality` in the JSON are what the backend reported. Other tools advertise image-model and quality options for this backend; on this endpoint they made no visible difference.
- Leave `--via-responses` and `--model` alone. They're a fallback route that rewrites the prompt and ignores `--size`; use them only if the default route is failing.

## Writing prompts

Order: scene/background → subject → key details → constraints → intended use.

- Say what the image is for ("App Store icon", "hero image for a landing page", "product photo for a catalogue"). That sets the level of polish.
- For a prompt with several requirements, use short labeled lines (`Subject:`, `Style/medium:`, `Text (verbatim):`, `Avoid:` ...) instead of one long paragraph. The template is in the reference below.
- If the user's request is already detailed, pass it through cleanly. If it's vague, add only framing, lighting and style cues. Don't add characters, props, slogans or brand colours that weren't implied.
- Text in the image: put the exact words in quotes, specify font style, colour and placement, and ask for "verbatim, no extra text". Check the spelling in the result.
- Edits: "Change only <X>. Keep <Y> unchanged." Repeat the things that must stay the same on every iteration.
- Several inputs: label them by role ("Image 1: the product to keep; Image 2: style reference only"). An `-i` image isn't always the thing to edit: images given only for style or mood mean a new image.
- Room for copy or UI: ask for negative space, but don't choose a side unless the layout needs one.
- Photorealism: say `photorealistic`, and add camera and lighting language plus real-world texture.
- Transparent assets: use `-b transparent -o x.png`, and ask for a clean isolated subject with no background, shadow or floor.

The labeled-line template, how much detail to add, and recipes by use case (slides, wireframes, character consistency, edits): [references/prompting.md](references/prompting.md).
