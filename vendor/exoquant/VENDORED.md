# Vendored exoquant 0.2.0

Copied from crates.io (MIT, see LICENSE) and used through `[patch.crates-io]` in the
top-level Cargo.toml. Changes from upstream:

- `Histogram` stores its counts in a `BTreeMap` instead of a `HashMap`, and `Color`
  derives `PartialOrd`/`Ord` for it. The randomly seeded `HashMap` handed colours to the
  quantizer in a different order on every run, so the same image quantized to different
  palettes and `codex-img -c` wrote different bytes each time.
- `#![allow(warnings)]` in lib.rs: the 2016 code trips newer lints.
- Removed `examples/` and the `lodepng` dev-dependency they used.
