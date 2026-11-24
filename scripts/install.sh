#!/bin/sh
# Build codex-img, copy the binary onto PATH and symlink the skill into agent skill dirs.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
bin_dir=${BIN_DIR:-$HOME/.local/bin}

cd "$root"
cargo build --release --quiet
mkdir -p "$bin_dir"
# Copy, not symlink: the install must survive `cargo clean`. Remove first so
# `install` never writes through an old symlink into target/.
rm -f "$bin_dir/codex-img"
install -m 755 "$root/target/release/codex-img" "$bin_dir/codex-img"
echo "binary: $bin_dir/codex-img (a copy; re-run ./scripts/install.sh after pulling a new version)"

for skills in "$HOME/.claude/skills" "$HOME/.codex/skills"; do
	[ -d "$(dirname "$skills")" ] || continue
	mkdir -p "$skills"
	ln -sfn "$root/skills/codex-img" "$skills/codex-img"
	echo "skill:  $skills/codex-img"
done
