//! Resolve named presets in the CLI; local palette processing belongs to core.
pub use codex_img_core::palette::*;
use crate::{error::{Error, Result}, presets::{Kind, Library}};
use std::path::Path;
/// Hex codes, else a file (relative to `base`), else the name of a palette preset.
pub fn resolve(spec: &str, base: &Path, library: impl FnOnce() -> Result<Library>) -> Result<Vec<Rgb>> {
    if is_list(spec) {
        return parse_list(spec).map_err(Into::into);
    }
    let file = base.join(spec);
    if file.is_file() {
        return from_file(&file).map_err(Into::into);
    }
    let library = library()?;
    let preset = library.get(Kind::Palette, spec)?;
    parse_list(preset.text.as_deref().unwrap_or_default()).map_err(|e| Error::usage(format!("palette \"{spec}\": {}", e.message)))
}


#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resolves_palette_presets_and_files_without_backend_access() {
        let dir = crate::auth::tests::temp_dir("palette-resolve");
        std::fs::write(dir.join("p.hex"), "ff0000\n00ff00\n").unwrap();
        assert_eq!(resolve("p.hex", &dir, || panic!("a file needs no presets")).unwrap().len(), 2);
        assert_eq!(resolve("pico-8", &dir, || Ok(Library::builtin())).unwrap().len(), 16);
        assert!(resolve("nope", &dir, || Ok(Library::builtin())).unwrap_err().message.contains("Unknown palette \"nope\""));
    }
}
