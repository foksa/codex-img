#!/bin/sh
# Build codex-img and symlink the binary onto PATH and the skill into agent skill dirs.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
bin_dir=${BIN_DIR:-$HOME/.local/bin}

cd "$root"
cargo build --release --quiet
mkdir -p "$bin_dir"
ln -sfn "$root/target/release/codex-img" "$bin_dir/codex-img"
echo "binary: $bin_dir/codex-img"

for skills in "$HOME/.claude/skills" "$HOME/.codex/skills"; do
	[ -d "$(dirname "$skills")" ] || continue
	mkdir -p "$skills"
	ln -sfn "$root/skills/codex-img" "$skills/codex-img"
	echo "skill:  $skills/codex-img"
done
