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

- **What gets generated:** only missing raw images (`raw/<key>.png`), so delete one to re-roll it. Then every raw image is converted, rewriting only files whose bytes change.
- **Run `--dry-run` first:** generation uses quota. `--convert-only` needs no login.
- **Fields:**
  - Generation: `prompt`, `aspect`, `quality`, `background`, `reference` (other keys' raw images, generated first) and `images`.
  - Presets: `view`, `style`, `character` and `palette`, plus `style_ref`, `character_ref` and `composition_ref`.
  - Conversion: the `convert` options with underscores (`hard_alpha`, `key_region`, …), and `max: [W, H]` for a size limit.
  - `null` or `false` turns a default off.
- **Two kinds of `style`:** the top-level `style` is plain text appended to every prompt, while an asset's `style` names a preset.
- **Manifests:** each raw image gets `<key>.png.json`, recording the prompt sent, the presets, the inputs and what the backend reported. When the spec or an input has changed since, the asset's `skip` line says so (`changed: true` with `--json`). Ask the user before deleting the raw image to re-roll it, because that spends quota.
- **Output sizes:** each convert line reports the final size (`-> public/assets/tree.png (420x156, 18 KB)`), so read sizes from there instead of opening every file.

## Reviewing a set: `sheet`

```sh
codex-img sheet assets/harbor/*.png -o /tmp/harbor-sheet.png
```

It lays the images out in one labelled grid; look at that one image instead of opening each file.
- **What stands out:** sprites stand on a common baseline, so floating sprites, leftover background patches and style drift across a set are easy to spot.
- **Options:** `--same-scale` keeps relative sizes, `--force` replaces an earlier sheet, and `--bg '#rrggbb'` changes the background (default: muted green).
- **Where to write it:** outside the project (for example `/tmp`), because sheets are for review, not assets.

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
