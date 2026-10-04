# Project runs and manifests

A project is the folder holding the nearest `codex-img.json`, searched upward from the
working folder. An empty `{}` file marks a project too. Run from the project or one of its
subfolders; choosing an output folder or batch spec elsewhere doesn't choose another project.

`codex-img init [folder] [--no-events] [--json]` creates one (`{}`, or `{"events": false}`).
It refuses inside an existing project and never overwrites; `--json` prints `{file, root,
events}`. It doesn't edit `.gitignore`.

## What a run saves

- One event file per run: `.codex-img/runs/<UTC stamp>_<run id>.ndjson`. These never rotate.
- `.codex-img/README.md` on the first logged run, explaining that committing this folder is
  optional. To keep it local, add `.codex-img/` to `.gitignore`; the CLI never edits it.
- `<image>.json` manifests automatically for generated images, including kept raw originals
  and tile edits. Each records the prompt, presets, inputs and their fingerprints, and the
  backend's reported settings, version parent and conversion options. An existing manifest is preserved.
  Images made from a parent also record `kind`: `"rerun"` (rerun, batch re-roll) or `"edit"`
  (refine, batch refine); `job.started` carries the same field.

Paths inside the project are relative to its root in event files and manifest inputs.
Paths outside it stay absolute. Resolve relative paths against the project root, even when
the command ran in a subfolder. Project event files omit the absolute root.

Outside a project, plain runs write manifests only with `--manifest`, batch raw images still
get them, and tile doesn't write them. `tile --force` in a project can replace images but
preserves existing manifests and warns; use a new output name to keep matching records.

## Logging controls

- Loose runs use `$XDG_STATE_HOME/codex-img/events.ndjson`, else
  `~/.local/state/codex-img/events.ndjson`, with `%LOCALAPPDATA%` as the Windows fallback.
  Paths are absolute; the log rotates at 8 MB into `events.ndjson.1`.
- `CODEX_IMG_EVENTS=<path>` forces one log, with absolute paths and `root` on project
  `run.started` events. This is useful for tests and debugging.
- `CODEX_IMG_EVENTS=off` (also `0`, `false`, `no`, case insensitive) suppresses event files
  and registry updates, including in projects. Manifests still apply.
- A top-level `"events": false` in `codex-img.json` does the same for that project's runs,
  persistently. In the global `presets.json` (`$XDG_CONFIG_HOME/codex-img/`, else
  `~/.config/codex-img/`) it covers loose runs and projects without the key. Precedence:
  `CODEX_IMG_EVENTS`, then the project file, then the global file, then on. An explicit
  `CODEX_IMG_EVENTS=<path>` still logs there. Manifests are written regardless. Such a project
  has no run files.
- `run.started.generation` is true for backend requests and false for local work, including batch restore, conversion and sheets. Count completed jobs for usage, not planned jobs. Older events lack this marker: backend stages (`generating`, `editing`, `in_progress`, `completed`) identify paid jobs; complete jobs without stages were free. Include incomplete/unknown records with an accuracy note rather than undercounting.
- `job.done.historyPath` records where the previous batch raw and its adjacent manifest moved on re-roll or restore. Resolve it against the project root, like other paths.
- New events use `v:2`; v1 lines keep absolute paths.
- Event or registry write errors never stop a generation.

The default global log's folder also holds `projects.json`, a list of `{root, lastRun}`
entries ordered newest first. `root` is absolute; `lastRun` is Unix milliseconds at run
start. Project runs update it even with an override log. Writes use a lock, a temporary file
and a rename so concurrent runs keep each other's entries and readers see complete JSON.

No command in this reference needs a generation to inspect history. Read the event files,
manifests and registry directly; only generate images when the user asks for them.

## Repeat a request or link a version

`codex-img rerun <image> [-n N] [-o out] [--json]` reads `<image>.json`. It sends the recorded
submitted prompt, preserving historical preset text and palette colours, and applies the
recorded conversion. It costs N images (default 1, maximum 10), creates new files and sets
`parent` to the original image. There is no seed: the request repeats, the pixels vary.

Changed reference contents stop before quota. `--anyway` allows them with a warning for
each path. Missing references always stop, even with `--anyway`; use an edited request to
remove one. Older manifests without conversion settings replay what's recorded with a
warning and default conversion. You can redo conversion locally without quota.

One plain `-i` infers a parent. Style, character and composition references don't.
`--no-parent` suppresses inference; `--parent <image>` sets it explicitly. Parent paths use
the same project-relative rule as inputs. Manifests for outputs outside the project include
an absolute `root` so rerun can resolve their references.

`codex-img presets list --json` uses no quota. It prints one object per line with kind, name,
text, absolute reference paths and source. Palette entries include hex `colors`; `hiddenBy`
marks entries overridden by another layer.

## Review notes

`codex-img comments [--json] [folder]` lists single-image manifest comments and batch asset
comments in specs. JSON lines use `image` and `comment` for images, or `spec`, `key`,
`rawPath` and `comment` for batch assets. `codex-img stars [--json] [folder]` lists
`image` and `star` for starred manifest images. Paths are relative to the scanned folder.
The default scan uses the project root; it skips internal history, build/dependency folders
and symlink directories. These commands need no login and use no quota.

Change only the review field through the CLI, preserving the rest of the JSON:

```sh
codex-img comments art/hero.png --set "Make the hat blue"
codex-img comments --batch art/assets.json --key hero --set "Blue hat"
codex-img stars art/hero.png --set true
```

Only that member's text changes; the rest of the file keeps its hand-written layout, so a
git-tracked spec gets a one-line diff. An empty comment clears it; `--set false` removes a star. Add `--expect "previous text"`
or `--expect true|false` to stop if another editor changed that field. Leave comments
that you have not addressed in place. Batch comments are ignored by generation, conversion
and the changed check.

## Send a comment as an edit

`codex-img refine <image> "<change>" [-o out] [--json]` costs one image. It edits the saved
image, keeps its recorded request/conversion settings and resolves recorded preset names
using current definitions. It compares submitted text and input fingerprints with the
current composition, warning with preset names about changed text, reference selection or
reference contents. When combined text cannot identify one changed preset, it says so.
Missing recorded presets and current references stop before quota. It stores no preset
definition snapshots. The change is sent as "Change only: <change>. Keep everything else
exactly as it is.", so don't add either part yourself; a closing copy of the last sentence is
dropped rather than repeated.

`--from-comment` uses the image's saved comment instead of positional change text. The new
manifest gets `fromComment` and `parent`. Only after saving the new image and manifest does
it clear the old comment; failures and comments changed during the run are preserved.
`--expect-comment "previous text"` stops for a stale note before starting. Batch refine
uses `refine --batch <spec> --key <key> "<change>"` or `--from-comment`. It preserves
the raw’s recorded generation settings and preset names (current definitions), archives the
old raw after success, then applies the batch’s current conversion settings. Use
`batch <spec> --reroll <key>` to adopt changed generation settings, and
`batch <spec> --restore <key> <version>` to promote a version without quota. Restore keeps
the source image and copies its manifest except for comment/star, with the source as parent.

## Validate before running

Use `codex-img check <file> --json` for a batch spec or `codex-img.json`. It uses the same
parsers as generation, needs no login and spends no quota. It prints one JSON list of
`{path, message}` diagnostics (`[]` for valid), with exit 0 or 64. Fix reported assets or
fields before running. Schemas in `schemas/batch.schema.json` and
`schemas/codex-img.schema.json` describe the accepted fields for editors. A top-level
`"$schema"` string is accepted without fetching it. CLI checks still decide whether a spec
is usable, including preset names, option combinations and reference cycles.

`check --kind=batch|presets` forces the file type when validating an editor’s temporary copy. Otherwise it is inferred from the filename and fields.

Review listing warns about broken image manifests and recognizable batch specs (a top-level `assets` key), while ignoring unrelated JSON/JSONC files quietly. `stars` only reads image manifests; `--json` stdout stays NDJSON and a completed listing exits 0.

Restore accepts PNG, JPEG and WebP sources, including images without a manifest. Such sources get a minimal raw manifest with `parent` and the project `root` where applicable. Restore preserves existing manifest fields without inserting missing `inputs` or `setup`. Source comments and stars stay on the source and are not copied. If the current raw manifest has a non-empty comment, restore stops before changing images; clear or move that comment first (batch comments belong in the spec).

Batch `--inspect --json` includes an `edited` boolean. For refined raws, `changed` compares the spec with the first generation manifest in the parent chain. Missing history gives `changed: false, edited: true`. Comments still do not count as changes.

Batch generation, re-roll, restore and `refine --batch` lock each spec separately by its canonical path under `.codex-img/locks/`. Different specs can run concurrently. If the same spec is busy, stderr says “Waiting for another batch run on <spec>…” before waiting. Add `--no-wait` to fail immediately instead (also supported by `refine --batch`). `--convert-only` takes one spec lock for the conversion phase, so its workers run in parallel and `--no-wait` fails once before any conversion if the spec is busy. Dry runs and inspection do not lock. An unused legacy `.codex-img/batch.lock` is removed when a new spec lock is acquired; a legacy lock held by an older process is left in place.

Pending generated files that were not activated are excluded from inspect history. Re-roll
`job.started` events omit the future parent; `job.done` includes `parent` and `historyPath`
only after activation. Single-preset drift warnings show short recoverable old/new text
excerpts. Outside projects, review locks live beside the global event log, including when
events are off; reviewing a loose image does not create `.codex-img/` beside it.
