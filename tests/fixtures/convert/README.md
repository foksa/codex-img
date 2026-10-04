# Pre-core conversion baseline

Frozen at commit `77b8781`, before moving any implementation, with the current release CLI.
Release executable: `target/release/codex-img`, **2,401,392 bytes** (macOS arm64, repository
release profile, `cargo build --release --offline`).

`cases.json` covers 37 local conversions: trim and padding/density, default/custom hard
alpha, automatic/named/RGB/repeated keys with region/cut/spread and masks, palettes with
and without cleanup, quantization with and without dithering, one-sided resize, all four
fits, no-enlarge, nearest up/downsampling, no-bleed, PNG/JPEG/WebP, lossy defaults/custom
quality, lossless WebP, and JPEG/WebP inputs. The combined case exercises pipeline ordering.
Inputs are deterministic synthetic pixels; no generations or backend calls were used.

`tests/convert_goldens.rs` compares output and mask bytes plus stable JSON report fields.

Do not regenerate these during extraction or as part of a test. `generate.py` documents
how they were captured (`python3 tests/fixtures/convert/generate.py target/release/codex-img`);
use it only when deliberately reviewing a change to the conversion contract, starting
with an empty `golden/` folder. Tests never replace expected outputs.
