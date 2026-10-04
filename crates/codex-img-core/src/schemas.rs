//! JSON Schemas for codex-img's project files, for editors and validators.

/// Schema for `codex-img.json` and the global `presets.json`.
pub const PROJECT: &str = include_str!("../../../schemas/codex-img.schema.json");
/// Schema for batch spec files.
pub const BATCH: &str = include_str!("../../../schemas/batch.schema.json");
