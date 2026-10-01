#!/usr/bin/env bash
# Smoke test for a built codex-img binary: the local commands on a real file, no login or quota.
# Usage: tests/smoke.sh <path to codex-img>
set -euo pipefail

fixture="$(cd "$(dirname "$0")" && pwd)/fixtures/sprite.png"
bin="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
# Relative paths from here on, so no shell path translation (Git Bash on Windows) is involved.
cd "$work"
fail() { echo "smoke test failed: $*" >&2; exit 1; }

"$bin" --version

# convert: keying, trimming, quantizing, and WebP output.
"$bin" convert "$fixture" --hard-alpha --key auto --key-region bottom:40% --trim -c 16 -o out/ --json > convert.json
grep -q '"size":"24x26"' convert.json || fail "convert: expected the 24x26 object without its water, got $(cat convert.json)"
"$bin" convert "$fixture" -o out/sprite.webp --quiet

# Existing files are kept unless --force; --force leaves identical bytes alone.
if "$bin" convert "$fixture" -o out/sprite.webp --quiet 2> /dev/null; then fail "convert overwrote a file without --force"; fi
"$bin" convert "$fixture" -o out/sprite.png --hard-alpha --key auto --key-region bottom:40% --trim -c 16 --force --json | grep -q '"unchanged":true' \
  || fail "convert --force rewrote identical output"

# sheet
"$bin" sheet out/sprite.png out/sprite.webp -o sheet.png --quiet
[ -s sheet.png ] || fail "sheet wrote nothing"

# batch --convert-only, and a second run that changes nothing.
mkdir -p art/raw/props
cp "$fixture" art/raw/props/sprite.png
cat > art/assets.json <<'JSON'
{"raw_dir": "raw", "out_dir": "../public", "defaults": {"trim": true},
 "assets": {"props/sprite": {"prompt": "unused: the raw image exists", "max": [16, 16]}}}
JSON
"$bin" batch art/assets.json --convert-only --quiet
[ -s public/props/sprite.png ] || fail "batch wrote no output"
"$bin" batch art/assets.json --convert-only --json | grep -q '"status":"unchanged"' || fail "batch rewrote an unchanged output"

# presets: built-in views, then a project preset; global presets go to a folder of our own.
export XDG_CONFIG_HOME="$work/config"
"$bin" presets --json | grep -q '"name":"isometric","source":"built-in"' || fail "presets: no built-in isometric view"
"$bin" presets add style smoke --text "flat colours" --ref "$fixture" > /dev/null
[ -s presets/styles/smoke/1-sprite.png ] || fail "presets add didn't copy a ref from outside the project"
"$bin" presets promote style smoke > /dev/null
"$bin" presets show style smoke --json | grep -q '"source":"project"' || fail "presets: the project preset should win over the global one"
"$bin" presets remove style smoke > /dev/null
"$bin" presets show style smoke --json | grep -q '"source":"global"' || fail "presets: the promoted global preset is gone"

echo "smoke test passed"
