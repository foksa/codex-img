# codex-img

Generate images from the command line with your ChatGPT/Codex subscription. It needs no API key and doesn't run the `codex` agent.

`codex-img` calls the same image endpoint the Codex CLI uses, `chatgpt.com/backend-api/codex/images/generations`, or `/images/edits` when you pass reference images. That's one JSON request per image, with no chat model in between. It reuses the ChatGPT login that `codex login` saved in `~/.codex/auth.json`.

## Install

**Prebuilt binary:** download the archive for your platform from [Releases](https://github.com/foksa/codex-img/releases). Builds exist for macOS (Apple Silicon and Intel) and Linux (x86-64 and ARM64, fully static). Unpack it and put `codex-img` on your `PATH`. The archive also contains the agent skill in `skills/codex-img`.

```sh
tar -xzf codex-img-*-aarch64-apple-darwin.tar.gz
install codex-img-*/codex-img ~/.local/bin/
```

On macOS, a binary downloaded with a browser may be quarantined; clear that with `xattr -d com.apple.quarantine ~/.local/bin/codex-img`.

**From source:**

```sh
./scripts/install.sh    # needs a Rust toolchain (https://rustup.rs)
```

This builds `target/release/codex-img`, a single ~2 MB binary with no runtime dependencies, and symlinks:
- the binary to `~/.local/bin/codex-img` (override with `BIN_DIR`);
- the agent skill `skills/codex-img` into `~/.claude/skills/` and `~/.codex/skills/`, for whichever of those tools you have installed.

You need to be logged in once with `codex login` (ChatGPT sign-in). `codex-img status` checks the login offline, without using any quota.

## For agents

`skills/codex-img/SKILL.md` is an Agent Skill. Claude Code, Codex and other skill-aware agents load it automatically when a task involves making or editing an image. It covers the commands to run, when to spend quota, how to handle each exit code (don't retry auth or quota errors), and prompt-writing tips. Longer recipes are in `skills/codex-img/references/prompting.md`.

When the binary isn't installed, the skill falls back to `skills/codex-img/scripts/codex_img.py`. It's a standard-library-only Python 3.9+ script with the same flags, exit codes and `--json` output, but it only covers the direct route and only writes PNG: there's no JPEG/WebP conversion, no quantization and no `--via-responses`. Agents can convert the output themselves. That makes the skill folder usable on its own, just by copying it into `~/.claude/skills/` or `~/.codex/skills/`.

## Usage

```sh
codex-img "a red fox in snow, flat vector"                  # -> ./codex-img-<time>-<id>.png
codex-img "app icon, paper plane" -o icon.webp
codex-img "wide landscape of a lighthouse" --size 1536x1024
codex-img "make it night with aurora" -i fox.png -o fox-night.jpg
codex-img "sticker of a cat" --background transparent -n 4 -o stickers/
codex-img "flat app icon, paper plane" -c 64 -o icon.png          # palette PNG, ~10-70x smaller
codex-img "race car sprite" -b transparent --trim=4 --resize 400x -o car.png   # + car.raw.png
echo "long prompt..." | codex-img - --json
codex-img status --json                                     # login check, uses no quota
```

| Option | |
|---|---|
| `-o, --output` | File or directory. A trailing `/` or an existing directory gets generated names. With `-n`, files get `-1`, `-2`, … suffixes. Existing files are never overwritten. |
| `-i, --image` | Reference image to edit or compose (PNG/JPEG/WebP, repeatable, max 5) |
| `-f, --format` | `png` \| `jpeg` \| `webp`. Defaults to the `-o` extension, then `png`. The endpoint returns PNG and the rest is converted locally: `jpeg` with transparency flattened onto white; `webp` lossy (libwebp), keeping transparency |
| `--output-quality` | 1–100 for `jpeg` (default 90) and lossy `webp` (default 80). Separate from `-q`, which is only a hint to the backend |
| `--lossless` | Lossless `webp` with every pixel kept exactly. Much bigger than lossy |
| `-c, --colors` | Quantize PNG output to a palette of 2–256 colours, like pngquant. Transparency is kept. Best for flat art: a 969 KB icon came out at 13 KB with 64 colours |
| `--dither` | Dither while quantizing. Smooths gradients and photos, but makes files larger |
| `-s, --size` | `WxH` or `auto` |
| `-q, --quality` | `low` \| `medium` \| `high` \| `auto` |
| `-b, --background` | `transparent` \| `opaque` \| `auto` |
| `--via-responses` | Fallback route: a routing model calls the `image_generation` tool through the Responses API. The prompt may be rewritten and `--size` is ignored |
| `-m, --model` | Routing model for `--via-responses` (default `gpt-5.5`) |
| `--trim`, `--resize`, `--fit`, `--no-bleed` | Trim, resize and edge bleed before saving, as in [`convert`](#converting-existing-images). With `--trim` or `--resize` the untouched original is also kept as `<name>.raw.<ext>`, since quota was spent on it and there's no seed to regenerate it |
| `-n, --count` | Images to generate in parallel (1–10). Each one is a separate request |
| `--json` | Prints one JSON line per image: path, size, quality (the backend's), `outputQuality` (the JPEG/WebP quality codex-img applied, when it encoded lossily), revised prompt, usage, duration. After `--trim`/`--resize`: `size` is the saved file's, plus `rawSize` (the backend's), `rawPath` and `trim` |
| `--quiet` | No progress output on stderr |

PNG output is always recompressed losslessly with oxipng. That roughly halves the backend's PNGs (946 KB → 462 KB in testing) without changing a visible pixel. The only pixels that change are fully transparent ones, which get the nearest edge colour (see edge bleed under [Converting existing images](#converting-existing-images); `--no-bleed` turns it off).

Which format to pick, from one generated 1536x1024 image (946 KB from the backend):

| Output | Size | Good for |
|---|---|---|
| `webp` (lossy, q80) | 7.5 KB | Web images, gradients, photos |
| `png -c 64` | 8 KB | Flat art, icons, stickers; keeps transparency |
| `jpeg` (q90) | 82 KB | Places that don't take WebP |
| `png` | 462 KB | Exact pixels, further editing |
| `webp --lossless` | 469 KB | Exact pixels, in WebP |

Paths go to stdout and progress goes to stderr, so the tool composes well in scripts and agent tools.
Exit codes: `0` ok, `1` error, `2` auth, `3` quota, `4` moderation, `64` usage.

### Converting existing images

`codex-img convert` runs the same local pipeline on files you already have. It needs no login and uses no quota.

```sh
codex-img convert hero.png -o hero.webp          # PNG/JPEG/WebP in, any of them out (lossy WebP)
codex-img convert icon.png -c 64                 # -> icon.min.png, palette PNG
codex-img convert photo.png                      # -> photo.min.png, lossless recompression only
codex-img convert shots/*.png -f jpeg -o out/    # several inputs need a directory
codex-img convert car.png --trim=4 --resize 400x -o sprites/    # crop to content, then resize
codex-img convert cockpit.png --resize 1920x1080 --fit cover    # exactly 1920x1080, centre crop
```

Without `-o`, output goes next to the input as `<name>.<ext>`, or `<name>.min.<ext>` when that would be the input itself. It takes `-o`, `-f`, `-c`, `--dither`, `--output-quality`, `--lossless`, `--json` and `--quiet`, with the same meaning as above, plus:

| Option | |
|---|---|
| `--trim[=PAD]` | Crop transparent borders to the visible pixels, keeping `PAD` transparent pixels around them. `--json` reports the crop as `trim: {x, y, width, height}` in input pixels, so sprites can keep their anchor. Opaque images are left as they are |
| `--resize SIZE` | `WxH`, `Wx` or `xH`, up to 8192 per side. A missing side keeps the aspect ratio. Runs after `--trim` |
| `--fit MODE` | For `WxH` with another aspect ratio: `inside` (default; fits in the box, so one side may be smaller), `cover` (exactly WxH, crops the centre), `contain` (exactly WxH, pads with transparency, or white in JPEG), `fill` (stretches) |
| `--no-bleed` | Keep the colour stored under fully transparent pixels (see below) |

Generated images with a transparent background often store a dark vignette under the transparent pixels. It's invisible until something resizes or filters the image without premultiplying alpha, and then it shows up as a dark halo around the edges. `convert` and generation deal with it twice. Its own resize uses premultiplied alpha, so the hidden colour never reaches the result. For PNG and lossless WebP output, it also writes the nearest visible colour under every fully transparent pixel (edge bleed, as texture tools do), so a game engine's texture filtering blends toward the edge colour. Visible pixels and alpha don't change. As a side effect, the file usually gets much smaller, because the noisy hidden colours are gone. Lossy WebP replaces those colours on its own, JPEG has no alpha, and palette PNGs (`-c`) are left alone so the bled colours don't use up palette entries. `--no-bleed` keeps the stored colours as they are; use it with `--lossless` when every pixel must be kept exactly, including invisible ones. The `--json` output also has `inputSize`. Animated WebP and PNG (APNG) are refused rather than converted, because only the first frame would survive. A file that already matches the request (a lossy WebP converted to WebP with default settings, for example) is copied rather than re-encoded, so it doesn't lose quality again. Existing files, including the input, are never overwritten. Each input is converted independently: if one fails, the others still run and the exit code reports the failure.

A bare `convert` as the first argument always runs this subcommand. A prompt that starts with the word still works when it's quoted, e.g. `codex-img "convert this sketch into a watercolor" -i sketch.png`.

## Notes

- **Routes.** By default `codex-img` does what the Codex CLI does: it sends `{prompt, model: "gpt-image-2", size, quality, background}` to `/images/generations`, or to `/images/edits` with `images: [{image_url: "data:…"}]`. Your prompt reaches the image model unchanged. `--via-responses` uses the Responses API with the `image_generation` tool instead (ported from pi-codex-image-gen).
- **Model.** The backend picks the image model and ignores the `model` field; the Codex CLI hardcodes `gpt-image-2` too. The Responses route reports it as `gpt-image-2-codex`. Whenever the backend upgrades what it serves (such as Images 2.5), both routes get the upgrade.
- **What's honoured.** `size` is a hint: `1536x1024` has come back at 1536x1024 and also at 1370x1148, and square comes back around 1254x1254. Check `size` in the `--json` output, or add `--resize WxH --fit cover` to get exact pixels. `background: transparent` gives real alpha. `quality` is capped at medium on the subscription. The endpoint doesn't validate its inputs and ignores unknown values, so `codex-img` checks them before sending.
- `codex-img` only reads `auth.json`. It never refreshes or writes tokens, so it can't interfere with your `codex` login. If the login has expired or gets rejected, it exits with code `2` and tells you to open Codex (the `codex` CLI or the app) so it renews the login, or to run `codex login`. `CODEX_HOME` overrides `~/.codex`.
- The image model is chosen by the backend. It is currently `gpt-image-2-codex`, reported as `imageModel` in `--json`. `--model` only picks the routing model.
- Debugging: `CODEX_IMG_DEBUG_RAW=/tmp/raw.txt codex-img …` saves the raw response body (the JSON, or the event stream with `--via-responses`). It contains the full base64 image.
- Image generation uses your subscription's image quota.

## Credits

The request format and stream parser are ported from [pi-codex-image-gen](https://github.com/jvm/pi-mono/tree/main/packages/pi-codex-image-gen) (Apache-2.0). See `NOTICE`.
