# Game art and asset sets

Sprites, consistent sets, camera angles, characters and palettes. All commands here except generation itself use no quota.

## Contents
- [Presets: check what the project defines](#presets-check-what-the-project-defines)
- [Camera angle: `--view`](#camera-angle---view)
- [Transparent sprites](#transparent-sprites)
- [Characters across images](#characters-across-images)
- [Palettes](#palettes)
- [Many assets: `batch`](#many-assets-batch)
- [Reviewing a set: `sheet`](#reviewing-a-set-sheet)
- [Texture atlases: `atlas`](#texture-atlases-atlas)
- [Map tile pyramids: `pyramid`](#map-tile-pyramids-pyramid)
- [Repeating backgrounds: `tile`](#repeating-backgrounds-tile)

## Presets: check what the project defines

Run `codex-img presets` before writing style, character or camera text by hand. It lists views, styles, characters and palettes with where each is defined. The first of these to define a name wins:
1. a batch spec's own presets
2. the project's `codex-img.json` (searched upward from the current folder)
3. global presets
4. the built-ins

Using a project's presets keeps a set consistent; wording that varies from prompt to prompt drifts.

```sh
codex-img presets show style harbour                              # text, refs, and the file it's in
codex-img presets add view roadside --text "seen from the road at eye level, ..."
codex-img presets add style harbour --text "16-bit pixel art, bold colours" --ref art/boat.png
codex-img presets remove style harbour
codex-img presets promote style harbour                           # project -> global (every project)
```

- **Where `add` writes:** to the project's `codex-img.json`, created in the current folder if there's none. Use `--global` only when the user wants the preset in every project.
- **Refs:** a ref inside the project is stored as a relative path. One outside it is copied to `presets/<kind>/<name>/`.
- **Text:** give only what defines the preset. It's added to every prompt that uses it.
- **Overriding:** a built-in view or palette is overridden by defining one with the same name.

## Camera angle: `--view`

Use `--view` instead of describing the angle. One view per game keeps every sprite in the same perspective.

| Game | View |
|---|---|
| Pseudo-3D racer roadside, side-scroller, platformer | `side` (or `front` for objects facing the player) |
| Top-down racer or shooter, map tiles, board pieces | `top-down` |
| Classic RPG towns and characters | `three-quarter` |
| City builder, strategy, isometric tiles | `isometric` |

- **Why it matters:** "front view at a slight angle" or "standing on a dock" gives a visible top surface. In a low-camera game that looks like the ground sloping up behind the object. `side` and `front` ask for a flat elevation with no top surface and a straight bottom edge.
- **What tests showed:** `top-down` buildings may still show a sliver of their front wall; trees and cars come out as clean plan views.
- **A project's own view:** if none of these fit, define one once with `codex-img presets add view <name> --text "..."`.

## Transparent sprites

```sh
codex-img "Game asset sprite: a red race car. Isolated on a transparent background, no ground, no shadow." \
  --view side -b transparent --trim=4 --resize 400x -o sprites/car.png      # + sprites/car.raw.png
```

- **Ask for the right subject:** a single isolated subject with crisp edges, generous padding, and no shadow, floor or reflection. Say "no platform, no dock, no ground" for things that usually stand on something.
- **Hard edges:** generated alpha is soft, and even "solid" areas come back at 250–254. For pixel art or crisp edges, add `--hard-alpha`, which makes every pixel fully solid or fully transparent, and keeps it that way through `--resize` and `-c`. Compare one result with the project's existing art to decide.
- **Painted-in backgrounds:** despite "no ground", boats and docks tend to come with painted sea, and buildings with sky. Remove them with `convert --key` (see [convert.md](convert.md#removing-a-painted-in-background)).
- **Keep the raw image:** there's no seed to regenerate it.

A full pixel-art sprite step from a raw image:

```sh
codex-img convert raw/car.png --hard-alpha --trim --resize 400x300 --no-enlarge -c 160 -o assets/
```

## Characters across images

1. Make one clean anchor image of the character.
2. Save a preset whose text is **only the character's looks**:

   ```sh
   codex-img presets add character captain --ref anchor.png \
     --text "a stocky walrus sea captain with big tusks, yellow raincoat, white captain's hat"
   ```

3. Generate scenes with `--character captain` and a prompt that describes only the scene and action. The reference is labelled and the looks are added for you.

Never use the anchor's whole prompt as the text. In tests, "isolated on a transparent background, standing" in the character text made every scene fade to transparent at the edges. For a one-off, `--character-ref anchor.png` does the same without a preset.

## Palettes

```sh
codex-img "a treasure chest sprite" --palette '#2B1D14,#6B3E26,#C7743A,#F2C14E,#F7EBD0,#3B6E5A' -b transparent -o chest.png
codex-img "a treasure chest sprite" --palette pico-8 -b transparent -o chest.png
codex-img convert old-art/*.png --palette game-boy --palette-clean -o snapped/
codex-img presets add palette harbour --from lospec-swatch.png      # or --text '#... #...'
```

- **What a palette can be:** hex codes, a `.gpl` or `.hex` file, a swatch image, or a preset. Built in are `pico-8`, `game-boy`, `nes`, `c64`, `zx-spectrum`, `cga`, `ega`, `ega-64`, `sweetie-16`, `dawnbringer-16`, `dawnbringer-32`, `endesga-32`, `resurrect-64` and `aap-64`.
- **What `--palette` does:**
  - It adds the hex codes to the prompt. With 4–16 colours, 81–88% of pixels came back close to the palette; a palette's name alone got 8%.
  - It then snaps every colour to the palette. Alpha is hardened, and the colours are snapped again after `--resize`.
  - PNG output is a palette PNG of exactly those colours. The original is kept as `.raw.png`.
- **Limits:** PNG or `--lossless` WebP only, and not with `-c`.
- **`--palette-clean`** is for art that wasn't made with the palette. Without it, shading between palette colours turns into speckles and streaks of another hue. Don't use it on art generated with `--palette`: it changes more than it needs to, pixel art especially.
- **Keep other colour words consistent with the palette.** A "red gem" with a palette that has no red can't come out right.
- **With 64 colours,** the prompt matters less, but the snap still gives good results.

## Many assets: `batch`

For a set of assets, such as a game's art, keep them in a JSON spec and run `codex-img batch spec.json [key or folder...]` instead of scripting many calls. `codex-img batch --help` has the full spec format.

```json
{
  "style": "16-bit arcade pixel art, bold colours, clean dark outlines, no text.",
  "palettes": {"harbour": "#2B1D14 #6B3E26 #C7743A #F2C14E #F7EBD0 #3B6E5A"},
  "characters": {"captain": {"text": "a stocky walrus sea captain in a yellow raincoat", "refs": ["refs/captain.png"]}},
  "defaults": {"background": "transparent", "view": "side", "palette": "harbour", "trim": true, "max": [400, 400]},
  "assets": {
    "harbor/crane": {"prompt": "Game asset sprite: an old dockside crane.", "aspect": "2:3"},
    "harbor/captain": {"prompt": "The captain waving hello, full body.", "character": "captain", "view": null},
    "mill/full": {"prompt": "A windmill.", "publish": false},
    "mill/sails": {"prompt": "Only the four sails, hub centred.", "reference": "mill/full"}
  }
}
```

- **What gets generated:** only missing raw images (`raw/<key>.png`), use `batch <spec> --reroll <key>` to replace one while keeping its history. Then every raw image is converted, rewriting only files whose bytes change. An asset whose generation failed in that run is left out of conversion, so its old output stays as it was.
- **Run `--dry-run` first:** generation uses quota. `--convert-only` needs no login.
- **Fields:**
  - Generation: `prompt`, `aspect`, `quality`, `background`, `reference` (other keys' raw images, generated first) and `images`.
  - Presets: `view`, `style`, `character` and `palette`, plus `style_ref`, `character_ref` and `composition_ref`.
  - Conversion: the `convert` options with underscores (`hard_alpha`, `key_region`, …), and `max: [W, H]` for a size limit.
  - `null` or `false` turns a default off.
- **Two kinds of `style`:** the top-level `style` is plain text appended to every prompt, while an asset's `style` names a preset.
- **Project history:** runs belong to the project found above the working folder, even when the batch spec or output is elsewhere. Events go to `.codex-img/runs/`; see [projects.md](projects.md).
- **Manifests:** each raw image gets `<key>.png.json`, recording the prompt sent, the presets, the inputs and what the backend reported. When the spec or an input has changed since, the asset's `skip` line says so (`changed: true` with `--json`). Re-roll only when the user asks, because it spends quota. Use `--dry-run --json` first to show the full plan, including missing dependencies.
- **Output sizes:** each convert line reports the final size (`-> public/assets/tree.png (420x156, 18 KB)`), so read sizes from there instead of opening every file.

## Reviewing a set: `sheet`

```sh
codex-img sheet assets/harbor/*.png -o /tmp/harbor-sheet.png
```

It lays the images out in one labelled grid; look at that one image instead of opening each file.
- **What stands out:** sprites stand on a common baseline, so floating sprites, leftover background patches and style drift across a set are easy to spot.
- **Options:** `--same-scale` keeps relative sizes, `--force` replaces an earlier sheet, and `--bg '#rrggbb'` changes the background (default: muted green).
- **History:** saved sheets emit free run events, like other runs. Set `CODEX_IMG_EVENTS=off` for a temporary preview.
- **Where to write it:** outside the project (for example `/tmp`), because sheets are for review, not assets.

## Texture atlases: `atlas`

```sh
codex-img atlas assets/units/ -o public/units.webp --prefix units/ --lossless --trim --extrude 1
```

It packs images into pages with TexturePacker "hash" JSON (PixiJS, Phaser). Frame names are paths under the directory without the extension. More pages are added as `units-1.webp`/`units-1.json` when needed, and the first JSON links them through `related_multi_packs`. Output is deterministic. Identical images are stored once, with every name pointing at the shared pixels. WebP pages default to `--effort 9` (smallest, slowest). `codex-img atlas --help` lists padding, `--max-size` and `--pot`.

## Map tile pyramids: `pyramid`

```sh
codex-img pyramid map/baseTiles -o public/base/ --map-size 3500x2000 --lossless
```

It takes a directory of `x_y.png` tiles and writes zoomed-out levels (1/2, 1/4, … until one tile holds the map) on the same grid, plus `pyramid.json`. Edge tiles are cropped to `--map-size`, `--merge 512` writes bigger tiles, and blank tiles are skipped.

## Repeating backgrounds: `tile`

For a sky or backdrop that repeats side by side:

```sh
codex-img tile sky.png -o sky-tile.png --preview /tmp/join.png
```

- **What it does:** the panorama then wraps around seamlessly, so it doesn't need mirroring, which shows landmarks twice.
- **Cost:** one image of quota. Size, framing and every pixel away from the join are kept.
- **If the join comes out wrong,** add `--prompt "what the picture shows"`.
- **Always look at the `--preview` image,** which shows the join in the middle.
- **Don't prompt landmarks onto an edge** as a workaround for mirroring.

Inside a project, tile outputs and `--keep-edit` images also get manifests. `--force` may replace images, but existing manifests are preserved and a warning is printed. Use a new output name to keep each image and its record together.

For another version of an existing result, `codex-img rerun <image> -o <new-path>`
repeats its recorded request and conversion, using one image of quota. Tile reruns use
the original panorama for the seam repair. The new manifest links to the old result with
`parent`; ordinary edits with one `-i` infer this link too (`--no-parent` disables it,
`--parent <image>` sets it). See [projects.md](projects.md) for reference checks and legacy
manifests. `presets list --json` lists full text, absolute refs, sources and palette hex
colours without using quota.

### Batch history and edits

`batch <spec> --inspect --json` lists per-asset raw/output state, changed status, comments,
stars, conversion settings and history. It is offline and free. Re-roll exact keys with
`batch <spec> --reroll <key>...`; current raws stay active until their replacements succeed.
Old PNGs and manifests move to `.codex-img/history/<key>/`, in the project or beside an
unprojected spec. Failures preserve the current version. A concurrent raw/manifest change
stops activation and keeps the generated image in history for recovery.

`batch <spec> --restore <key> <image>` copies any version into the raw slot (a manifest is optional),
keeps the source, archives the old raw, converts non-PNG input to PNG, and records the source
as parent. It spends no quota, then applies current conversion settings.

`refine --batch <spec> --key <key> "<change>"` (or `--from-comment`) costs one image. It uses
the raw’s recorded generation settings and recorded preset names with their current
definitions and drift warnings. It replaces the raw through history, then applies current
batch conversion settings. Re-roll is how to adopt the current spec’s generation settings.
From-comment edits clear the asset note only after success, preserving changed notes.

The batch conversion flag `nearest: true` copies source pixels when resizing. Use it for whole-number upscaling of pixel art; generated pixel art is not on a true grid, so downscaling it this way gives uneven pixels. Grid detection is deferred.

After reviewing a batch dry-run plan, use `--expect-images N` to stop before quota if the number of generations (including missing dependencies) changed. This guard belongs to the CLI, so the app’s cost remains enforced when the child starts.

Restore leaves source comments/stars on the source; clear or move any current raw manifest
comment before restoring. `--inspect --json` adds `edited`, with change detection following
the original generation’s parent chain. Pending files are excluded from history. Batch locks
are per canonical spec; `--no-wait` fails immediately on contention, and conversion-only
runs take one spec lock for the conversion phase, allowing parallel workers; a busy spec
fails once before workers start under `--no-wait`.
