# Converting existing images: `codex-img convert`

`convert` handles PNG, JPEG and WebP files locally, with no login and no quota. Use it instead of ImageMagick, Pillow or a custom script for format changes, cropping to content, resizing and size reduction, including on images generated earlier. Don't call the image model just to change the format or size. `codex-img convert --help` lists every option.

## Contents
- [Format and file size](#format-and-file-size)
- [Trim and resize](#trim-and-resize)
- [Removing a painted-in background](#removing-a-painted-in-background)
- [Palettes](#palettes)
- [Files and edges](#files-and-edges)

## Format and file size

- `codex-img convert in.png -o out.webp` (or `-f jpeg`) changes the format. `--output-quality N` (1–100) trades size for quality; `--lossless` makes WebP exact.
- `codex-img convert icon.png -c 64` writes `icon.min.png`, a palette PNG, for flat art (icons, stickers, logos). `-c 64` to `-c 256` usually cuts the file 10x or more with no visible change. Don't use it for photos or soft gradients; if banding shows, raise the count or add `--dither`.
- `codex-img convert in.png` writes `in.min.png`, lossless recompression only. PNG output is always recompressed, which roughly halves the backend's files.
- For images going on a website, lossy WebP is usually the smallest by far: a 946 KB PNG became 7.5 KB.
- JPEG has no transparency, so transparent areas become white.

## Trim and resize

- **`--trim`** crops transparent borders to the visible pixels. Visible means alpha above 16, so the faint specks generated images leave on the background don't count.
  - `--trim=4` keeps 4 transparent pixels around them.
  - An image with no transparent border is left as it is.
  - `--json` reports the crop as `trim: {x, y, width, height}` in input pixels, for placing sprites.
- **`--resize 400x`** (or `x300`) keeps the aspect ratio. With `--resize WxH`, `--fit` decides how another shape fits:
  - `inside` (the default): fits in the box, so one side may be smaller.
  - `cover`: exactly WxH, cropping the centre.
  - `contain`: exactly WxH, with transparent padding.
  - `fill`: stretches.

  Trim runs first, then resize.
- **`--nearest`** copies pixels with `--resize`, for whole-number upscaling of pixel art. Downscaling generated pixel art gives uneven pixels because the art is not on a true grid; grid detection is a later feature. It is a flag, with no filter selection.
- **`--no-enlarge`** makes `--resize` a maximum size: smaller images keep their size instead of being scaled up.
- **`--hard-alpha`** makes every pixel fully solid or fully transparent, and keeps it that way through `--resize` and `-c`. Use it for pixel art and crisp sprite edges.
- **Sprites:** `codex-img convert raw/car.png --trim=4 --resize 400x -o sprites/`, or the same flags when generating.

## Removing a painted-in background

The model sometimes paints a background despite "no ground" or "transparent background" in the prompt. It happens most often with sea under boats and docks, or sky behind a building.

Remove it with `--key auto` and a `--key-region` whose edges really are background. `auto` samples the background's colours along those edges. `--key` flood-fills from the transparent area and the border, so matching paint inside the outline survives, and leftover foam and specks go too.

- **Ground under a sprite:** `--key auto --key-region bottom:30% --key-cut --trim --trim-density 0.15`, after `--hard-alpha` for pixel art. `--trim-density 0.15` stops leftover specks from becoming the bottom edge and making the sprite float.
- **Sky behind an object:** `--key auto --key white --key-region top:80% --key-spread 16 --trim`.
  - `--key-spread` follows the sky's gradient, and `white` takes clouds that have outlines.
  - Scenery close to the sky's colour (hazy mountains) goes too; use a shallower region to keep it.
- **Never point `auto` at an edge that is the object itself** (a trunk, wheels): it would key it away.
- **If `auto` gets it wrong,** name the colours: `--key blue --key white`, or `--key '#3070c0:40'`.
- **Check the result** on a `codex-img sheet`.

## Palettes

`--palette <colours>` snaps every colour to a palette, and `--palette-clean` does it for art that wasn't made with it. See [game-assets.md](game-assets.md#palettes).

## Files and edges

- **Several inputs:** `-o` must be a directory ending in `/`.
- **GIF input:** read like the others (the first frame; truly animated GIFs are refused) and written as PNG unless `-f` says otherwise.
- **Directory trees:** `-r` converts every image under a directory input and mirrors its relative paths under `-o`. With `--json`, the last line is `{"total":{...}}`.
- **Overwriting:** `convert` never overwrites the input, and overwrites other existing files only with `--force`.
  - The same input and options always give the same bytes.
  - So re-running a pipeline into the same `-o` folder rewrites only the files whose output changed.
- **Transparent edges are handled for you; you don't need to clean transparent pixels yourself.**
  - Resizing uses premultiplied alpha, so the colour stored under transparent pixels can't bleed into the edges. Generated images often hide a dark vignette there.
  - PNG and lossless WebP output also gets the nearest visible colour written under transparent pixels. Engines that filter without premultiplying then don't show a dark halo either. `--no-bleed` turns that off.

## Preview reports and removal masks

`--json` reports the existing input/output sizes and trim box plus `hardAlphaPixels`,
`keyedOutPixels`, and `paletteColors` when a palette was selected. Hard-alpha counts include
newly transparent pixels before and after resizing; keyed-out counts only keying.
`--mask-out <png>` with `--key` writes a black/white mask at the input size: white pixels were
removed by keying, black were retained. It remains in input coordinates after trim/resize.
Use one input and a separate new mask filename. No login, network or quota is involved.

Saved conversions write run events linked to their input. Use `CODEX_IMG_EVENTS=off` for temporary previews.
The terminal/agent interface remains `codex-img convert`; no flags changed.
