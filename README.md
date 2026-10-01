# codex-img

Generate images from the command line with your ChatGPT/Codex subscription. It needs no API key and doesn't run the `codex` agent.

`codex-img` calls the same image endpoint the Codex CLI uses, `chatgpt.com/backend-api/codex/images/generations`, or `/images/edits` when you pass reference images. That's one JSON request per image, with no chat model in between. It reuses the ChatGPT login that `codex login` saved in `~/.codex/auth.json`.

## Install

**Prebuilt binary:** download the archive for your platform from [Releases](https://github.com/foksa/codex-img/releases). Builds exist for macOS (Apple Silicon and Intel), Linux (x86-64 and ARM64, fully static) and Windows (x86-64, a `.zip`). Unpack it and put `codex-img` on your `PATH`. The archive also contains the agent skill in `skills/codex-img`.

```sh
tar -xzf codex-img-*-aarch64-apple-darwin.tar.gz
install codex-img-*/codex-img ~/.local/bin/
```

On macOS, a binary downloaded with a browser may be quarantined; clear that with `xattr -d com.apple.quarantine ~/.local/bin/codex-img`.

On Windows (PowerShell):

```powershell
Expand-Archive codex-img-*-x86_64-pc-windows-msvc.zip -DestinationPath $env:LOCALAPPDATA\codex-img
# then add the unpacked codex-img-* folder to your PATH, or run codex-img.exe from it
```

The login is read from `%USERPROFILE%\.codex\auth.json` (or `%CODEX_HOME%\auth.json`), where `codex login` puts it.

**From source:**

```sh
./scripts/install.sh    # needs a Rust toolchain (https://rustup.rs)
```

On Windows, `install.sh` doesn't apply: run `cargo build --release` and use `target\release\codex-img.exe`.

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
codex-img "a lighthouse on a cliff at dusk" -a 16:9               # frame shape, see below
codex-img "make it night with aurora" -i fox.png -o fox-night.jpg
codex-img "sticker of a cat" --background transparent -n 4 -o stickers/
codex-img "flat app icon, paper plane" -c 64 -o icon.png          # palette PNG, ~10-70x smaller
codex-img "race car sprite" -b transparent --trim=4 --resize 400x -o car.png   # + car.raw.png
echo "long prompt..." | codex-img - --json
codex-img status --json                                     # login check, uses no quota
```

| Option | |
|---|---|
| `-o, --output` | File or directory. A trailing `/` or an existing directory gets generated names. With `-n`, files get `-1`, `-2`, … suffixes. Existing files are never overwritten: a taken name is refused before any quota is spent. |
| `-i, --image` | Reference image to edit or compose (PNG/JPEG/WebP, repeatable, max 5) |
| `-f, --format` | `png` \| `jpeg` \| `webp`. Defaults to the `-o` extension, then `png`. The endpoint returns PNG and the rest is converted locally: `jpeg` with transparency flattened onto white; `webp` lossy (libwebp), keeping transparency |
| `--output-quality` | 1–100 for `jpeg` (default 90) and lossy `webp` (default 80). Separate from `-q`, which is only a hint to the backend |
| `--lossless` | Lossless `webp` with every pixel kept exactly. Much bigger than lossy |
| `-c, --colors` | Quantize PNG output to a palette of 2–256 colours, like pngquant. Transparency is kept. Best for flat art: a 969 KB icon came out at 13 KB with 64 colours |
| `--dither` | Dither while quantizing. Smooths gradients and photos, but makes files larger |
| `-a, --aspect` | `W:H` from 1:3 to 3:1, such as `16:9`, `2:3` or `1:1`. Starts the prompt with "The frame must be in 16:9 landscape format, wider than it is tall.", which is what sets the shape (see [the backend](#the-backend)), and warns on stderr if the image comes back more than 2% off. Can't be combined with `--size`. For exact pixels, add `--resize WxH --fit cover` |
| `-s, --size` | `WxH` or `auto`. Sent as the request's `size`, which the backend has ignored in tests; use `--aspect` |
| `--view` | Camera preset: `side`, `front`, `top-down`, `three-quarter`, `isometric`, or one of your own. See [Presets](#presets-and-reference-roles) |
| `--style`, `--character` | Named style and character presets (repeatable): their text goes into the prompt, their images are sent as labelled references |
| `--style-ref`, `--character-ref`, `--composition-ref` | Reference images with a role (repeatable). They're sent after the `-i` images, max 5 images in all, and each gets a line in the prompt saying how to use it |
| `--palette`, `--palette-clean` | Limit the image to a palette: hex codes, a `.gpl`/`.hex` file, a swatch image, or a palette preset (14 built in, such as `pico-8`, `nes` or `resurrect-64`, or your own). See [Palettes](#palettes) |
| `--manifest` | Also writes `<image>.json`: the prompt sent, presets, inputs (with fingerprints) and what the backend reported |
| `-q, --quality` | `low` \| `medium` \| `high` \| `auto` |
| `-b, --background` | `transparent` \| `opaque` \| `auto` |
| `--via-responses` | Fallback route: a routing model calls the `image_generation` tool through the Responses API. The prompt may be rewritten and `--size` is ignored |
| `-m, --model` | Routing model for `--via-responses` (default `gpt-5.5`) |
| `--trim`, `--resize`, `--fit`, `--hard-alpha`, `--no-enlarge`, `--no-bleed` | Trim, resize, hard alpha and edge bleed before saving, as in [`convert`](#converting-existing-images). With `--trim`, `--resize` or `--hard-alpha` the untouched original is also kept as `<name>.raw.<ext>`, since quota was spent on it and there's no seed to regenerate it |
| `-n, --count` | Images to generate in parallel (1–10). Each one is a separate request |
| `--json` | Prints one JSON line per image: path, size, `submittedPrompt` (when presets, labels or `--aspect` changed the prompt), quality (the backend's), `outputQuality` (the JPEG/WebP quality codex-img applied, when it encoded lossily), revised prompt, usage, duration. After `--trim`, `--resize` or `--hard-alpha`: `size` is the saved file's, plus `rawSize` (the backend's), `rawPath` and `trim` |
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

### Presets and reference roles

Presets name the parts of a prompt you'd otherwise repeat and let drift:

- **Views** (camera): `side`, `front`, `top-down`, `three-quarter` and `isometric` are built in, and you can define your own.
- **Styles:** text and, optionally, style reference images.
- **Characters:** text and identity reference images, so one character keeps its looks across scenes.

```sh
codex-img "Game asset sprite: an old dockside crane. Isolated, no ground." --view side -b transparent -o crane.png
codex-img presets add character captain --ref art/captain.png \
    --text "a stocky walrus sea captain with big tusks, in a yellow raincoat and a white captain's hat"
codex-img "The captain steering a ship's wheel in a storm." --character captain -a 3:2 -o storm.png
codex-img "A fox." --style-ref art/style.png -a 1:1 -o fox.png      # a reference with a role, no preset
codex-img presets                                                   # what's defined, and where
```

**The prompt that's sent** is put together in this order, and `--json` shows it as `submittedPrompt`:
1. The `--aspect` sentence.
2. `Camera:` followed by the view's text.
3. One line per reference image. `-i` images come first and keep their numbers, so "Image 1" in your prompt still means the first `-i`.
   - `Image 2: style reference only: take its palette, rendering and line work, not its subject or layout.`
   - `Image 3: character reference for "captain": keep the same character (face, proportions, outfit, colours) in a new pose and scene.`
   - `Image 4: composition reference only: follow its layout and framing, not its subject or style.`
4. `The character "captain": <its text>`.
5. Your prompt, then each style's text.

**Where presets live.** The first of these that defines a name wins:
1. a batch spec's own `views`, `styles` and `characters`
2. the project: the nearest `codex-img.json` in the current folder or one above it
3. global: `$XDG_CONFIG_HOME/codex-img/presets.json`, else `~/.config/codex-img/presets.json`
4. built in: the views, and the palettes listed under [Palettes](#palettes)

Preset files are only read when a preset is named, so a broken one can't stop a plain run. `codex-img presets` lists every preset with its source, and marks the ones another layer hides.

```json
{
  "views": {"roadside": "seen straight on from the side at eye level, its bottom edge a straight line"},
  "styles": {"harbour": {"text": "16-bit pixel art, bold colours", "refs": ["refs/boat.png"]}},
  "characters": {"captain": {"text": "a stocky walrus sea captain", "refs": ["art/captain.png"]}}
}
```

A string is short for `{"text": ...}`, and ref paths are relative to the file. Edit the file by hand, or use:

| Command | |
|---|---|
| `presets add <kind> <name> [--text T] [--ref IMG]... [--global] [--force]` | Writes to the project's `codex-img.json` (created in the current folder if there's none), or the global file. A ref inside the project is stored as a relative path; one outside it is copied to `presets/<kind>/<name>/`. Global refs are always copied, to `refs/<kind>/<name>/` in the global folder. `--from IMG` is the same as `--ref IMG` |
| `presets show <kind> <name>` | Text, refs (and whether they exist) and the file it's defined in |
| `presets remove <kind> <name> [--global]` | Removes the entry, and the refs folder `add` made. Refs inside the project are never deleted |
| `presets promote <kind> <name> [--force]` | Copies a project preset, refs included, to the global file |

Give a character or style only the text that defines it. The text goes into every prompt that uses it, so a pose or "isolated on a transparent background" would end up in every scene too (see [the backend](#the-backend)).

**The Python fallback** has the built-in views and the `--*-ref` options, but not named styles, characters, palettes or your own views.

### Palettes

```sh
codex-img "a treasure chest sprite" --palette '#2B1D14,#6B3E26,#C7743A,#F2C14E,#F7EBD0,#3B6E5A' -b transparent -o chest.png
codex-img "a treasure chest sprite" --palette game-boy -b transparent --trim --resize 400x -o chest.png
codex-img convert old-art/*.png --palette pico-8 --palette-clean -o snapped/      # art made without it
codex-img presets add palette harbour --from lospec-swatch.png                    # or --text '#... #...'
```

`--palette` does two things:
- **On generation,** it adds the hex codes to the prompt ("Use only these 6 colours, exactly, and no others: #2B1D14, …").
- **After generation, and in `convert`,** it snaps every colour to the nearest palette colour, so the file has exactly those colours:
  - Alpha is hardened at 127 unless `--hard-alpha` says otherwise.
  - Stray pixels are cleaned at full size.
  - Colours are snapped again after `--resize`, which blends neighbours.
  - PNG output is a palette PNG. WebP needs `--lossless`, and JPEG and `-c` are refused.

The untouched original is kept as `<name>.raw.png`.

A palette can be:
- hex codes (commas or spaces, `#` optional)
- a GIMP `.gpl` file
- a `.hex` text file (as Lospec exports it)
- a swatch image (its distinct colours)
- the name of a palette preset: a built-in one (below), or one under `"palettes"` in `codex-img.json`, global presets or a batch spec

| Built-in palette | Colours | |
|---|---|---|
| `pico-8` | 16 | PICO-8 fantasy console |
| `game-boy` | 4 | original Game Boy greens (`#0F380F` to `#9BBC0F`) |
| `nes` | 55 | NES, as Lospec lists it (NES palettes differ by emulator) |
| `c64` | 16 | Commodore 64, as Lospec lists it (one of several measured versions) |
| `zx-spectrum` | 15 | ZX Spectrum, normal and bright |
| `cga` | 4 | CGA palette 1, high intensity: black, magenta, cyan, white |
| `ega` | 16 | EGA's default 16 colours (the RGBI set, with brown `#AA5500`) |
| `ega-64` | 64 | every colour EGA could show (2 bits per channel) |
| `sweetie-16` | 16 | by GrafxKid |
| `dawnbringer-16`, `dawnbringer-32` | 16, 32 | by DawnBringer |
| `endesga-32` | 32 | by ENDESGA |
| `resurrect-64` | 64 | by Kerrie Lake |
| `aap-64` | 64 | by Adigun A. Polack |

The values come from [Lospec](https://lospec.com/palette-list). Each can be overridden by defining a palette of the same name. There's no Game Boy Color palette, because that console had no fixed one.

In a batch spec, use the `palette` and `palette_clean` fields.

`--palette-clean` is for art that wasn't generated with the palette. It reduces the image to 32 colours, matches hue before lightness, then despeckles. Without it, shading that falls between palette colours flickers between them as speckles and streaks of another hue. It changes images that were prompted with the palette more than it needs to (pixel art especially), so it's off by default.

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
| `--key COLOUR` | Make an unwanted background transparent, such as the sea painted under a boat or the sky behind a building: pixels of this colour that connect to the transparent background or the image's border. It's a flood fill, so matching paint enclosed by the object's outline (a blue stripe on a hull) survives, and small islands the removed background leaves behind, like foam and spray, go with it. `COLOUR` is `auto[:TOL]`, a name (`red`, `orange` for browns too, `yellow`, `green`, `cyan`, `blue`, `purple`, `pink`, `white`, `gray`, `black`), or `#rrggbb[:TOL]`. `auto` uses the background's own colours, sampled from the outer 5% along the `--key-region` edges, or all four edges without a region. `TOL` is per channel, default 32. Repeatable. Runs after `--hard-alpha` and before `--trim` |
| `--key-region BANDS` | Only key out within bands along edges of the visible content, each a share of its height or width: `bottom:30%` (ground under a sprite), `top:80%` (sky behind a building), `top:40%,left:15%`, or `all:20%`. Matching colours elsewhere, like a sky-blue window, stay safe |
| `--key-spread STEP` | Let keying spread from each removed pixel into neighbours whose colour differs by at most `STEP` per channel (e.g. 16), step by step. It then follows gradients the key colours don't cover, like a sky fading from blue to gold, and stops at outlines. Higher steps also take low-contrast scenery, like distant hills |
| `--key-cut[=F]` | Before `--key`, from each `--key-region` edge inward, cut off whole rows (or columns) while at least `F` (default 0.4) of their visible pixels match the key: below a boat's waterline, for example. Only for objects that span most of the line; a narrow building in a wide sky would be cut through |
| `--trim-density [EDGES:]F` | With `--trim`, also drop sparse rows at the bottom: those with fewer visible pixels than `F` (e.g. `0.15`) of the fullest row. Leftover specks under a sprite then don't become its bottom edge, which would make a sprite that stands on its bottom edge float. Other edges: `top:0.15`, `bottom,left:0.15`, `all:0.15`. A thin mast, pole or trunk is sparse too, so check the result |
| `--no-bleed` | Keep the colour stored under fully transparent pixels (see below) |
| `--force` | Replace existing output files (never an input). The file is written to a temporary name and renamed into place, and one that already holds the same bytes is left untouched (`--json` reports `unchanged: true`) |

Generated images with a transparent background often store a dark vignette under the transparent pixels. It's invisible until something resizes or filters the image without premultiplying alpha, and then it shows up as a dark halo around the edges. `convert` and generation deal with it twice. Its own resize uses premultiplied alpha, so the hidden colour never reaches the result. For PNG and lossless WebP output, it also writes the nearest visible colour under every fully transparent pixel (edge bleed, as texture tools do), so a game engine's texture filtering blends toward the edge colour. Visible pixels and alpha don't change. As a side effect, the file usually gets much smaller, because the noisy hidden colours are gone. Lossy WebP replaces those colours on its own, JPEG has no alpha, and palette PNGs (`-c`) are left alone so the bled colours don't use up palette entries. `--no-bleed` keeps the stored colours as they are; use it with `--lossless` when every pixel must be kept exactly, including invisible ones. The `--json` output also has `inputSize`. Animated WebP and PNG (APNG) are refused rather than converted, because only the first frame would survive. A file that already matches the request (a lossy WebP converted to WebP with default settings, for example) is copied rather than re-encoded, so it doesn't lose quality again. Without `--force`, existing files are never overwritten, and the input never is. The same input and options always give the same bytes, `-c` included, so re-running a pipeline with `--force` only rewrites files whose output really changed. Each input is converted independently: if one fails, the others still run and the exit code reports the failure.

The image model sometimes paints a background despite "no ground" or "transparent background" in the prompt. Most often it's a patch of sea under boats and docks; for pictures with an opaque background, it's the whole sky. `--key` removes such a background:

```sh
# Sea painted under a boat: sample along the bottom, cut below the waterline, stop specks making it float.
codex-img convert boat.png --hard-alpha --key auto --key-region bottom:30% --key-cut --trim --trim-density 0.15 -o sprites/
# Sky behind a building: sample along the top, follow the gradient, add white for clouds with outlines.
codex-img convert barn.png --key auto --key white --key-region top:80% --key-spread 16 --trim -o sprites/
```

`--key auto` can't tell the background from the object's own edge. On a sprite without painted ground, the bottom rows are a trunk, wheels or a pole, and sampling there would key them away. So point the region at edges that really are background. When `auto` picks up too much or too little, name the colours instead (`--key blue --key white`) or change `--key-spread`. Scenery with nearly the sky's colour, like hazy distant mountains, goes with the sky; use a shallower `--key-region` to keep it. Check a batch on a contact sheet (`codex-img sheet`).

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
    "trees/oak": {"prompt": "A single big old oak tree.", "aspect": "3:2", "max": [420, 380]},
    "harbor/boat": {"prompt": "A fishing boat, side view.", "aspect": "3:2", "max": [420, 300],
                    "key": "auto", "key_region": "bottom:30%", "key_cut": true, "trim_density": 0.15},
    "harbor/sky": {"prompt": "A wide harbour sky panorama.", "aspect": "3:1", "background": "opaque",
                   "format": "webp", "hard_alpha": false, "colors": null, "trim": false, "output_quality": 85},
    "mill/full": {"prompt": "A windmill.", "aspect": "2:3", "publish": false},
    "mill/sails": {"prompt": "Edit this image: only the four sails, hub centred.", "aspect": "1:1",
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
- **Palettes:** `palette` (hex codes as a string or a list, a file relative to the spec, or a palette preset) and `palette_clean`. The hex codes join the prompt when the raw image is generated, and the conversion snaps to them.
- **Presets:** `view`, `style` and `character` name presets, as the options of the same names do; `style` and `character` can be lists. `style_ref`, `character_ref` and `composition_ref` are reference images with a role, relative to the spec. The spec can define its own presets under top-level `views`, `styles` and `characters`, which win over the project's and global ones. The top-level `style` is still plain text appended to every prompt; an asset's `style` names a preset.
- **Manifests:** each generated raw image gets `<key>.png.json` beside it, with the prompt sent, the presets, the inputs with fingerprints, and what the backend reported. When the spec or an input has changed since, the asset's `skip` line says so (`changed: true` with `--json`). The asset is never regenerated on its own, because that would spend quota; delete the raw image to re-roll it.
- **Generation fields:** `prompt`, `aspect`, `size`, `quality`, `background`, `reference` (other keys whose raw images are passed as `-i`; they're generated first) and `images` (other `-i` files). `aspect` leads the prompt, ahead of the `style`, and a frame more than 2% off it is reported as a warning on the asset's line. `publish: false` generates an asset without converting it, for references.
- **Conversion fields** are the `convert` options with underscores: `format`, `colors`, `dither`, `output_quality`, `lossless`, `trim`, `hard_alpha`, `resize`, `fit`, `no_enlarge`, `no_bleed`, `key`, `key_region`, `key_spread`, `key_cut` and `trim_density`. `max: [W, H]` is short for `resize` with `no_enlarge`.
- **Validation:** the whole spec is checked before anything runs. An unknown field, a missing reference or a reference loop is an error that names the asset.
- **Output:** one line per asset and step (`ok`, `skip`, `same`, `FAILED`), or one JSON object each with `--json`. After a login or quota error, no more images are started. The exit code is the worst failure's, so a run with any failure exits non-zero.
- **Options:** `-j N` sets how many images are generated at the same time (default 4, max 10), and `--generate-only` skips the conversion step.

## The backend

The Codex image endpoint isn't a public API, and nothing about its behaviour is documented. This section separates what codex-img controls from what has been observed in use (as of September and October 2026). The observations can change whenever the backend does, without a codex-img release.

**What codex-img controls**

- **The request.** By default `codex-img` does what the Codex CLI does: it sends `{prompt, model: "gpt-image-2", size, quality, background}` to `/images/generations`, or to `/images/edits` with `images: [{image_url: "data:…"}]`. Your prompt reaches the image model unchanged, apart from the one framing sentence `--aspect` puts in front of it. `--via-responses` uses the Responses API with the `image_generation` tool instead (ported from pi-codex-image-gen), where a routing model may rewrite the prompt.
- **Input checks.** The endpoint doesn't validate its inputs and ignores values it doesn't know, so `codex-img` checks every option before sending anything, and checks that the output file is free before spending quota.
- **Everything after the image arrives:** conversion, trimming, resizing, palettes, background removal, and never overwriting a file. This runs locally, is covered by tests, and doesn't depend on the backend.

**What has been observed, not guaranteed**

- **Model.** On the direct route, `codex-img` sends `model: "gpt-image-2"`, as the Codex CLI does, and the response doesn't say which model made the image. The Responses route has reported `gpt-image-2-codex`. Which model serves each route, and whether that changes, isn't known. `--model` only picks the routing model for `--via-responses`.
- **Shape comes from the prompt, not `size`.** In October 2026 tests, the same prompt with `size: 1024x1536` and with `size: 1536x1024` both came back at 1312x1199. Leading the prompt with a ratio sentence (what `--aspect` does) gave that ratio every time, on generations and on edits: an edit of a 2:3 image with `-a 16:9` came back 16:9, with the scene widened around the subject. The pixel count stays around 1.57 megapixels, so 2:3 came back at exactly 1024x1536 and 16:9 at 1672x941. Other tools have reported 3:4 at 1086x1448, 9:16 at 941x1672, 21:9 at 1916x821, and anything beyond 3:1 clamped to 3:1. Without `--aspect`, shape words in the prompt ("wide landscape") steer it too, less precisely. Add `--resize WxH --fit cover` to get exact pixels.
- **Quality** is capped at medium on the subscription. Prompts led by a ratio sentence came back reported as `low`, with fewer image tokens (343 for 2:3 and 301 for 16:9, against 829 without it), even with `-q medium`; the images still looked fully detailed. Why isn't known.
- **The `model` field isn't checked.** Sending `gpt-image-2.5-sunburst` instead of `gpt-image-2` was accepted without an error, and gave the same size, quality and token count, so there's no sign it picked another model.
- **Transparency.** `background: transparent` gives real alpha, but "solid" pixels come back at alpha 250–254 (`--hard-alpha` fixes that), and the model sometimes paints a background anyway (`--key` removes it).
- **Views** (built-in camera presets, October 2026, flat-coloured game sprites of a cottage, a street lamp, a car and a tree). `side`, `front`, `isometric` and `top-down` came out as asked for the car and the tree, and `side`/`front` gave flat elevations for the cottage and the lamp. A building seen `top-down` still showed a sliver of its front wall. The first wording of `three-quarter` turned the cottage 45° (isometric-looking), and the first `top-down` gave a front view with a big roof; the built-in texts now say "front square to the camera, not turned" and "like a map: for a building, only its roof", which fixed both.
- **Reference images become part of the scene.** Any input image goes to `/images/edits`, so the result takes that image's frame (a 2:3 style reference gave a 2:3 result; `-a` overrides it). A style reference photo of an apple on a table gave a fox on that same table: describe the new setting in the prompt. In two A/B pairs, the role labels made no visible difference next to a plain `-i`; they're there so several images can't be confused.
- **Character text leaks into scenes.** A character preset whose text was a whole sprite prompt ("isolated on a transparent background, standing") gave scenes that faded to transparent at the edges. With only the character's looks as text, the same scenes filled the frame and the character kept its face, outfit and colours.
- **Palettes.** Treasure-chest sprites, October 2026. Hex codes in the prompt put 81–88% of pixels within distance 16 (RGB) of a palette colour and 97–99% within 40, yet every image still had 9,000–14,000 distinct colours. A palette's name alone ("PICO-8") gave 8% within 16, and "a limited palette of 8 colours" changed nothing. A swatch image as a style reference didn't beat the hex codes. Listing PICO-8's colours, or naming it, made the model switch to pixel art by itself. Snapping in OKLab after the prompt changed little visibly (0.1–1% stray pixels before the despeckle). Snapping an image generated without the palette gave purple streaks in brown wood, which `--palette-clean` turns into the palette's neutral grey. With 64 colours (Resurrect 64, AAP-64), the prompt mattered less: 33–49% of pixels within 16 (as many distinct colours as with no palette), though the design still took the palette's greys and purples. Because a 64-colour palette covers most shades, the plain snap looked right even on an image generated without it.
- **Masks** are accepted and ignored: requests with and without one used the same number of input tokens. `tile` works around this.
- **No seed.** An image can't be generated again exactly, which is why `codex-img` keeps the untouched original whenever it changes pixels.

**Reported, not requested.** In `--json`, `imageModel`, `quality`, `background` and `size` are what the backend reported in its response, not what `codex-img` asked for, and they aren't checked against the image. The exception is `size` after `--trim`, `--resize` or `--hard-alpha`: then it's measured from the saved file, and the backend's value moves to `rawSize`. `usage` is the token counts the backend reported.

**Quota.** Requests run on your ChatGPT subscription login, but how they count against your plan's limits isn't known: the endpoint doesn't say which allowance a request uses, what it costs or how much is left, so `codex-img` can't either. When the backend answers with a recognised quota or usage-limit error, `codex-img` exits with code `3` and `batch` stops starting new images. `batch --dry-run` lists what would be generated before anything is sent.

## Notes

- `codex-img` only reads `auth.json`. It never refreshes or writes tokens, so it can't interfere with your `codex` login. If the login has expired or gets rejected, it exits with code `2` and tells you to open Codex (the `codex` CLI or the app) so it renews the login, or to run `codex login`. `CODEX_HOME` overrides `~/.codex`.
- Debugging: `CODEX_IMG_DEBUG_RAW=/tmp/raw.txt codex-img …` saves the raw response body (the JSON, or the event stream with `--via-responses`). It contains the full base64 image.

## Credits

The request format and stream parser are ported from [pi-codex-image-gen](https://github.com/jvm/pi-mono/tree/main/packages/pi-codex-image-gen) (Apache-2.0). See `NOTICE`.
