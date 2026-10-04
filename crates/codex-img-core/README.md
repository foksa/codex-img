# codex-img-core

Local, deterministic image processing used by the codex-img CLI, for other tools to share.
No login, tokens, transport, backend requests, generation orchestration or event writing.

- `images`: decode, encode, quantize, optimize; format and encoding validation.
- `transform`: transparency, keying and mask, trim, palette fitting, resize and bleed.
- `palette`: hex/GPL/swatch readers and pixel operations. Named preset lookup stays in callers.
- `sheet` and `tile`: local composition, roll, splice and join preview.
- `conversion`: the CLI pipeline, including exact unchanged-input passthrough; callers can
  supply cached decoded pixels with `process_decoded`.
- `settings` and `conversion_record`: validated local settings and manifest/batch field writers.
- `output`: complete-file creation without replacement and explicit atomic replacement.

Core errors distinguish invalid settings from local I/O/processing failures. The binary
maps those to its existing exit codes. CLI parsing, backend/auth, batch orchestration,
project/preset discovery and event writers remain in the root package.

The root workspace defaults to both CLI and core: `cargo test` runs both. Root
`cargo build --release` still produces `target/release/codex-img`. Another Cargo project using
core must apply the same `[patch.crates-io]` for the vendored exoquant (`vendor/exoquant`) to get
byte-identical output.

`tests/dependencies.rs` runs `cargo tree -p codex-img-core` to reject network/auth libraries.
The pre-extraction compatibility matrix lives in `../../tests/fixtures/convert/`.
