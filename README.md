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
| `--trim`, `--resize`, `--fit`, `--hard-alpha`, `--no-enlarge`, `--no-bleed` | Trim, resize, hard alpha and edge bleed before saving, as in [`convert`](#converting-existing-images). With `--trim`, `--resize` or `--hard-alpha` the untouched original is also kept as `<name>.raw.<ext>`, since quota was spent on it and there's no seed to regenerate it |
| `-n, --count` | Images to generate in parallel (1–10). Each one is a separate request |
| `--json` | Prints one JSON line per image: path, size, quality (the backend's), `outputQuality` (the JPEG/WebP quality codex-img applied, when it encoded lossily), revised prompt, usage, duration. After `--trim`, `--resize` or `--hard-alpha`: `size` is the saved file's, plus `rawSize` (the backend's), `rawPath` and `trim` |
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
| `--trim[=PAD]` | Crop transparent borders to the visible pixels (alpha above 16: generated images scatter fainter specks over the background, and those don't count), keeping `PAD` transparent pixels around them. `--json` reports the crop as `trim: {x, y, width, height}` in input pixels, so sprites can keep their anchor. An image with no transparent border (any opaque image, every JPEG) is left as it is, padding included |
| `--resize SIZE` | `WxH`, `Wx` or `xH`, up to 8192 per side. A missing side keeps the aspect ratio. Runs after `--trim` |
| `--fit MODE` | For `WxH` with another aspect ratio: `inside` (default; fits in the box, so one side may be smaller), `cover` (exactly WxH, crops the centre), `contain` (exactly WxH, pads with transparency, or white in JPEG), `fill` (stretches) |
| `--hard-alpha[=N]` | Make every pixel fully solid (alpha above `N`, default 16) or fully transparent. Runs before `--trim`, and again at 50% after `--resize` so resampled edges stay hard. For pixel art and other crisp sprites. It also fixes the backend's "solid" pixels, which come back at alpha 250–254 |
| `--no-enlarge` | Only shrink: the scale never goes above 1, so `--resize` leaves smaller images at their size. `cover` still crops to the box's aspect ratio, `contain` pads the unscaled image, `fill` caps each side on its own |
| `--key COLOUR` | Make painted-in ground transparent (the sea under a boat, grass under a tree): pixels of this colour that connect to the transparent background or the image's border. It's a flood fill, so matching paint enclosed by the sprite's outline (a blue stripe on a hull) survives, and small islands the removed ground leaves behind, like foam and spray, go with it. `COLOUR` is `auto[:TOL]` (the ground's own colours, sampled from the bottom 5% of the visible content), a name (`red`, `orange` for browns too, `yellow`, `green`, `cyan`, `blue`, `purple`, `pink`, `white`, `gray`, `black`), or `#rrggbb[:TOL]`. `TOL` is per channel, default 32. Repeatable. Runs after `--hard-alpha` and before `--trim` |
| `--key-region EDGE:N%` | Only key out within a band of the visible content, e.g. `bottom:30%`, so matching colours higher up, like a sky-blue window, are safe |
| `--ground-cut[=F]` | Before `--key`, cut off the bottom rows where at least `F` (default 0.4) of the visible pixels match the key: everything below the line where the object meets the painted ground, like a boat's waterline |
| `--trim-density [EDGES:]F` | With `--trim`, also drop sparse rows at the bottom: those with fewer visible pixels than `F` (e.g. `0.15`) of the fullest row. Leftover specks under a sprite then don't become its bottom edge, which would make a sprite that stands on its bottom edge float. Other edges: `top:0.15`, `bottom,left:0.15`, `all:0.15`. A thin mast, pole or trunk is sparse too, so check the result |
| `--no-bleed` | Keep the colour stored under fully transparent pixels (see below) |
| `--force` | Replace existing output files (never an input). The file is written to a temporary name and renamed into place, and one that already holds the same bytes is left untouched (`--json` reports `unchanged: true`) |

Generated images with a transparent background often store a dark vignette under the transparent pixels. It's invisible until something resizes or filters the image without premultiplying alpha, and then it shows up as a dark halo around the edges. `convert` and generation deal with it twice. Its own resize uses premultiplied alpha, so the hidden colour never reaches the result. For PNG and lossless WebP output, it also writes the nearest visible colour under every fully transparent pixel (edge bleed, as texture tools do), so a game engine's texture filtering blends toward the edge colour. Visible pixels and alpha don't change. As a side effect, the file usually gets much smaller, because the noisy hidden colours are gone. Lossy WebP replaces those colours on its own, JPEG has no alpha, and palette PNGs (`-c`) are left alone so the bled colours don't use up palette entries. `--no-bleed` keeps the stored colours as they are; use it with `--lossless` when every pixel must be kept exactly, including invisible ones. The `--json` output also has `inputSize`. Animated WebP and PNG (APNG) are refused rather than converted, because only the first frame would survive. A file that already matches the request (a lossy WebP converted to WebP with default settings, for example) is copied rather than re-encoded, so it doesn't lose quality again. Without `--force`, existing files are never overwritten, and the input never is. The same input and options always give the same bytes, `-c` included, so re-running a pipeline with `--force` only rewrites files whose output really changed. Each input is converted independently: if one fails, the others still run and the exit code reports the failure.

The image model sometimes paints ground under a sprite despite "no ground" in the prompt: most often a patch of sea under boats and docks. For a game whose ground is drawn separately, that shows as a patch under the sprite, and leftover specks make it float. This removes it:

```sh
codex-img convert boat.png --hard-alpha --key auto --key-region bottom:30% --ground-cut --trim --trim-density 0.15 -o sprites/
```

`--key auto` can't tell painted ground from the object's own base, so use it only on images that have painted ground. On a sprite without ground, the bottom rows are a trunk, wheels or a pole, and those would be keyed away. When `auto` picks up too much or too little, name the colours instead (`--key blue --key white`). Check a batch on a contact sheet (`codex-img sheet`).

A bare `convert` as the first argument always runs this subcommand. A prompt that starts with the word still works when it's quoted, e.g. `codex-img "convert this sketch into a watercolor" -i sketch.png`.

### Contact sheets

`codex-img sheet` lays images out in one labelled grid, so a batch can be reviewed as a single image. That's cheaper for an agent than opening each file, and it's how problems across a set show up. Sprites stand on a line at the bottom of their cell, so one with empty rows or leftover specks under it visibly floats. Like `convert`, it needs no login and no quota.

```sh
codex-img sheet public/assets/harbor/*.png -o harbor-sheet.png
codex-img sheet sprites/*.png -o sheet.png --same-scale --cols 8 --force
```

| Option | |
|---|---|
| `-o FILE` | The sheet (required): `.png`, `.jpg` or `.webp` |
| `--cols N` | Columns (default: a roughly square grid) |
| `--cell PX` | Cell size, 32–2048 (default 240) |
| `--bg COLOUR` | Background as `#rrggbb` (default `#96be96`, a muted green that neither white nor dark sprites blend into) |
| `--labels WHAT` | `name` (default), `path` (as given on the command line) or `none` |
| `--center` | Centre sprites in their cells instead of standing them on the baseline |
| `--same-scale` | Shrink every sprite by the same factor, so their sizes stay comparable. By default each shrinks to fit its own cell. Sprites are never enlarged |
| `--force` | Replace an existing sheet |

An input that can't be read is reported and left out, and the others are still laid out. Labels use a small built-in font that covers ASCII; other characters show as `?`.

### Seamless panoramas

`codex-img tile` makes a panorama wrap around, so it can repeat side by side without a mirror, for skies and backdrops in side-scrolling and racing games. Mirroring a tile shows every landmark twice; `tile` joins the image's own right edge to its left edge instead. It uses one image of quota.

```sh
codex-img tile sky.png -o sky-tile.png --preview /tmp/join.png
codex-img tile sky.png -o sky-tile.png --prompt "a harbour town on a cliff, sailboats, big clouds"
```

How it works:

1. The image is rolled by half its width, so the wrap seam moves to the centre.
2. The model is asked to repaint a band there so the scenery continues across it. The Codex image endpoint ignores masks, so the whole image comes back redrawn, though only slightly outside that band.
3. Only the band is taken from the edit. On each side it's cut along the path where the edit and the original agree best. The cut goes around anything the edit redrew in full, such as a mountain that was split at the edge and comes back whole.
4. The spliced pixels are colour-matched to the original, and the image is rolled back.

The result keeps the original's size and framing, every pixel outside the band is unchanged, and the repaired join sits at the left and right edges. `--preview` writes two copies side by side around the join, so the result can be checked in one image. `--keep-edit` keeps the model's full edit, and `--json` reports the repaired band's width.

### Asset batches

`codex-img batch` builds a whole set of assets, a game's art for example, from one JSON spec. It generates each asset's raw image if it's missing (4 at a time), then converts every raw image into its published form. Raw images are never regenerated, so delete one to re-roll it. Converted files are rewritten only when their bytes change, so an unchanged asset stays unchanged in git.

```json
{
  "raw_dir": "raw",
  "out_dir": "../public/assets",
  "style": "16-bit arcade pixel art, bold saturated colours, clean dark outlines, no text.",
  "defaults": {"background": "transparent", "hard_alpha": true, "colors": 160, "trim": true},
  "assets": {
    "trees/oak": {"prompt": "A single big old oak tree.", "size": "1536x1024", "max": [420, 380]},
    "harbor/boat": {"prompt": "A fishing boat, side view.", "size": "1536x1024", "max": [420, 300],
                    "key": "auto", "key_region": "bottom:30%", "ground_cut": true, "trim_density": 0.15},
    "harbor/sky": {"prompt": "A wide harbour sky panorama.", "size": "1536x1024", "background": "opaque",
                   "format": "webp", "hard_alpha": false, "colors": null, "trim": false, "output_quality": 85},
    "mill/full": {"prompt": "A windmill.", "size": "1024x1536", "publish": false},
    "mill/sails": {"prompt": "Edit this image: only the four sails, hub centred.", "size": "1024x1024",
                   "reference": "mill/full", "max": [400, 400]}
  }
}
```

```sh
codex-img batch art/assets.json                    # everything: generate what's missing, convert all
codex-img batch art/assets.json harbor trees/oak   # a folder of keys, or one key
codex-img batch art/assets.json --dry-run          # what would be generated and converted
codex-img batch art/assets.json --convert-only     # no login, no quota
```

- **Keys** are paths: the raw image goes to `<raw_dir>/<key>.png` and the output to `<out_dir>/<key>.<format>`. Both directories are relative to the spec file, and default to `raw` and `out`.
- **`style`** is appended to every prompt. One shared style sentence keeps a large set looking like one game.
- **`defaults`** apply to every asset. An asset overrides them, and `null` or `false` turns one off, like the sky above.
- **Generation fields:** `prompt`, `size`, `quality`, `background`, `reference` (other keys whose raw images are passed as `-i`; they're generated first) and `images` (other `-i` files). `publish: false` generates an asset without converting it, for references.
- **Conversion fields** are the `convert` options with underscores: `format`, `colors`, `dither`, `output_quality`, `lossless`, `trim`, `hard_alpha`, `resize`, `fit`, `no_enlarge`, `no_bleed`, `key`, `key_region`, `ground_cut` and `trim_density`. `max: [W, H]` is short for `resize` with `no_enlarge`.
- **Validation:** the whole spec is checked before anything runs. An unknown field, a missing reference or a reference loop is an error that names the asset.
- **Output:** one line per asset and step (`ok`, `skip`, `same`, `FAILED`), or one JSON object each with `--json`. After a login or quota error, no more images are started. The exit code is the worst failure's, so a run with any failure exits non-zero.
- **Options:** `-j N` sets how many images are generated at the same time (default 4, max 10), and `--generate-only` skips the conversion step.

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
