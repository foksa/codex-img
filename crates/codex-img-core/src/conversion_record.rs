//! Store conversion as CLI options, so replay uses the same parser and validation as a new run.
use crate::{images::{Encoding, Format}, transform::{Colour, Fit, Key, Transform}, palette};
use serde_json::{json, Value};

pub fn build(format: Format, encoding: &Encoding, transform: &Transform) -> Value {
    let mut args = vec![];
    let mut value = |flag: &str, value: String| { args.push(format!("{flag}={value}")); };
    if let Some(n) = encoding.colors { value("--colors", n.to_string()); }
    if let Some(n) = encoding.quality { value("--output-quality", n.to_string()); }
    if let Some(n) = transform.trim { value("--trim", n.to_string()); }
    if let Some(n) = transform.hard_alpha { value("--hard-alpha", n.to_string()); }
    if let Some(n) = transform.key_cut { value("--key-cut", format!("{n}%")); }
    if let Some(n) = transform.key_spread { value("--key-spread", n.to_string()); }
    if let Some(region) = transform.key_region {
        let bands = ["top", "bottom", "left", "right"].into_iter().zip(region.bands).filter(|(_, percent)| *percent > 0)
            .map(|(edge, percent)| format!("{edge}:{percent}%")).collect::<Vec<_>>();
        value("--key-region", bands.join(","));
    }
    if let Some(density) = transform.trim_density {
        let edges = ["top", "bottom", "left", "right"].into_iter().zip(density.edges).filter(|(_, set)| *set).map(|(edge, _)| edge).collect::<Vec<_>>();
        value("--trim-density", format!("{}:{}%", edges.join(","), density.percent));
    }
    if let Some(resize) = transform.resize { value("--resize", format!("{}x{}", resize.width.map(|n| n.to_string()).unwrap_or_default(), resize.height.map(|n| n.to_string()).unwrap_or_default())); }
    if let Some(fit) = transform.fit { value("--fit", match fit { Fit::Inside => "inside", Fit::Cover => "cover", Fit::Contain => "contain", Fit::Fill => "fill" }.into()); }
    for key in &transform.keys {
        value("--key", match key {
            Key::Auto { tolerance } => format!("auto:{tolerance}"),
            Key::Rgb { rgb, tolerance } => format!("{}:{tolerance}", palette::hex(*rgb)),
            Key::Named(colour) => match colour { Colour::Red => "red", Colour::Orange => "orange", Colour::Yellow => "yellow", Colour::Green => "green", Colour::Cyan => "cyan", Colour::Blue => "blue", Colour::Purple => "purple", Colour::Pink => "pink", Colour::White => "white", Colour::Gray => "gray", Colour::Black => "black" }.into(),
        });
    }
    if let Some(palette) = &transform.palette { value("--palette", palette.colors.iter().copied().map(palette::hex).collect::<Vec<_>>().join(",")); }
    for (enabled, flag) in [(encoding.dither, "--dither"), (encoding.lossless, "--lossless"), (transform.no_bleed, "--no-bleed"), (transform.no_enlarge, "--no-enlarge"), (transform.nearest, "--nearest"), (transform.palette.as_ref().is_some_and(|p| p.clean), "--palette-clean")] {
        if enabled { args.push(flag.into()); }
    }
    json!({"format":format.name(), "args":args})
}

pub fn allowed(argument: &str) -> bool {
    matches!(argument.split('=').next().unwrap_or_default(), "--colors" | "--output-quality" | "--trim" | "--hard-alpha" | "--key-cut" | "--key-spread" | "--key-region" | "--trim-density" | "--resize" | "--fit" | "--key" | "--palette" | "--dither" | "--lossless" | "--no-bleed" | "--no-enlarge" | "--nearest" | "--palette-clean")
}


/// Swatches use the spec's folder, just like imagePathForFile in the JSON editor.
pub fn palette_for_spec(value: &str, spec: &std::path::Path) -> String {
    use std::path::{Component, Path, PathBuf};
    let image = Path::new(value);
    let Some(base) = spec.parent().filter(|base| base.is_absolute()) else { return value.into(); };
    if !image.is_absolute() { return value.into(); }
    let target: Vec<_> = image.components().collect();
    let base: Vec<_> = base.components().collect();
    let equal = |a: &Component<'_>, b: &Component<'_>| {
        if cfg!(windows) { a.as_os_str().to_string_lossy().eq_ignore_ascii_case(&b.as_os_str().to_string_lossy()) } else { a == b }
    };
    // A different drive or UNC share has no relative path.
    if !target.first().zip(base.first()).is_some_and(|(a, b)| equal(a, b)) { return value.into(); }
    let shared = target.iter().zip(&base).take_while(|(a, b)| equal(a, b)).count();
    let mut relative = PathBuf::new();
    for _ in &base[shared..] { relative.push(".."); }
    for component in &target[shared..] { relative.push(component.as_os_str()); }
    relative.to_string_lossy().replace('\\', "/")
}

pub fn batch_spec(spec: &std::path::Path, key: &str, expected: &str, settings: &[String]) -> crate::error::Result<String> {
    let mut value: Value = serde_json::from_str(expected).map_err(|e| crate::error::Error::usage(e.to_string()))?;
    let fields = value["assets"][key]
        .as_object_mut()
        .ok_or_else(|| crate::error::Error::usage("The asset changed."))?;
    let conversion = [
        "format",
        "colors",
        "dither",
        "output_quality",
        "lossless",
        "trim",
        "hard_alpha",
        "resize",
        "max",
        "fit",
        "no_enlarge",
        "nearest",
        "no_bleed",
        "key",
        "key_region",
        "key_cut",
        "key_spread",
        "trim_density",
        "palette",
        "palette_clean",
    ];
    // Null overrides defaults too; leaving a field absent would silently inherit a tool
    // setting the preview did not use. Generation fields and comments stay untouched.
    for field in conversion {
        fields.insert(field.into(), Value::Null);
    }
    for arg in settings {
        let (flag, value) = arg.split_once('=').unwrap_or((arg.as_str(), "true"));
        let key = flag.trim_start_matches("--").replace('-', "_");
        let value = if [
            "dither",
            "lossless",
            "palette_clean",
            "no_enlarge",
            "nearest",
            "no_bleed",
        ]
        .contains(&key.as_str())
        {
            Value::Bool(true)
        } else if key == "palette" {
            Value::String(palette_for_spec(value, spec))
        } else {
            Value::String(value.into())
        };
        if key == "key" {
            let entry = fields.entry(key).or_insert(Value::Null);
            if !entry.is_array() {
                *entry = Value::Array(vec![]);
            }
            entry.as_array_mut().unwrap().push(value);
        } else {
            fields.insert(key, value);
        }
    }
    serde_json::to_string_pretty(&value).map_err(|e| crate::error::Error::usage(e.to_string()))
}


/// Replace only conversion settings; callers own generation metadata and review fields.
pub fn set_manifest(manifest: &mut Value, format: Format, encoding: &Encoding, transform: &Transform) -> crate::error::Result<()> {
    manifest.as_object_mut().ok_or_else(|| crate::error::Error::usage("The manifest must be an object."))?
        .insert("conversion".into(), build(format, encoding, transform));
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn writers_preserve_reviews_and_generation_and_override_inherited_conversion() {
        let mut manifest = json!({"prompt":"Fox", "comment":"Keep", "star":true, "unknown":7});
        set_manifest(&mut manifest, Format::Webp, &Encoding { lossless:true, ..Default::default() }, &Transform::default()).unwrap();
        assert_eq!(manifest["conversion"], json!({"format":"webp","args":["--lossless"]}));
        assert_eq!(manifest["comment"], "Keep"); assert_eq!(manifest["unknown"], 7); assert_eq!(manifest["star"], true);
        assert!(set_manifest(&mut Value::Null, Format::Png, &Encoding::default(), &Transform::default()).is_err());
        let original = r#"{"defaults":{"trim":5,"palette":"pico-8"},"assets":{"hero":{"prompt":"Fox","comment":"Keep","quality":"high","unknown":7}}}"#;
        // An absolute root on every platform: "/project" has no drive on Windows.
        let project = std::path::Path::new(if cfg!(windows) { r"C:\project" } else { "/project" });
        let palette = format!("--palette={}", project.join("refs/palette.png").display());
        let value: Value = serde_json::from_str(&batch_spec(&project.join("specs/assets.json"), "hero", original, &["--format=png".into(), palette, "--key=blue".into(),"--key=cyan".into()]).unwrap()).unwrap();
        assert_eq!(value["assets"]["hero"]["palette"], "../refs/palette.png");
        assert!(value["assets"]["hero"]["trim"].is_null());
        assert_eq!(value["assets"]["hero"]["key"], json!(["blue","cyan"]));
        assert_eq!(value["assets"]["hero"]["comment"], "Keep"); assert_eq!(value["assets"]["hero"]["quality"], "high"); assert_eq!(value["assets"]["hero"]["unknown"], 7);
    }
}
