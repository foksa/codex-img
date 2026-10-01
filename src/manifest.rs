//! A JSON record beside a generated image (`<image>.json`) of how it was made: the prompt that
//! was sent, the presets and inputs behind it, and what the backend reported. It can't make the
//! backend repeat an image (there's no seed), but it shows what to change, and lets `batch` notice
//! that an asset's setup changed after its raw image was generated.
use crate::backend::Generated;
use crate::cli::{self, Aspect};
use crate::error::{Error, Result};
use crate::presets::{Kind, Source};
use crate::util;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

/// `hero.png` -> `hero.png.json`.
pub fn path_for(image: &Path) -> PathBuf {
    let mut name = image.file_name().unwrap_or_default().to_os_string();
    name.push(".json");
    image.with_file_name(name)
}

pub fn fingerprint(path: &Path) -> Option<String> {
    std::fs::read(path).ok().map(|bytes| format!("fnv1a64:{:016x}", util::fnv1a64(&bytes)))
}

/// An image the request sent, and why.
pub struct Input {
    pub path: PathBuf,
    /// `input` for -i and batch references, else the reference role.
    pub role: &'static str,
    pub character: Option<String>,
}

/// What a generation asked for.
pub struct Record<'a> {
    pub user_prompt: &'a str,
    pub prompt: &'a str,
    pub used: &'a [(Kind, String, Source)],
    pub aspect: Option<Aspect>,
    pub size: Option<&'a str>,
    pub quality: Option<&'a str>,
    pub background: Option<&'a str>,
    pub inputs: &'a [Input],
}

pub fn build(record: &Record, image: &Generated, shown: &dyn Fn(&Path) -> String) -> Value {
    let mut out = Map::new();
    out.insert("prompt".into(), json!(record.prompt));
    out.insert("userPrompt".into(), json!(record.user_prompt));
    if !record.used.is_empty() {
        let presets: Vec<Value> = record.used.iter().map(|(kind, name, source)| json!({"kind": kind.name(), "name": name, "source": source.name()})).collect();
        out.insert("presets".into(), json!(presets));
    }
    out.insert("request".into(), request(record.aspect, record.size, record.quality, record.background));
    let inputs: Vec<Value> = record
        .inputs
        .iter()
        .map(|input| {
            let mut entry = json!({"path": shown(&input.path), "role": input.role});
            if let Some(name) = &input.character {
                entry["character"] = json!(name);
            }
            if let Some(fingerprint) = fingerprint(&input.path) {
                entry["fingerprint"] = json!(fingerprint);
            }
            entry
        })
        .collect();
    if !inputs.is_empty() {
        out.insert("inputs".into(), json!(inputs));
    }
    let r = &image.reported;
    let mut reported = Map::new();
    for (key, value) in [("imageModel", &r.model), ("size", &r.size), ("quality", &r.quality), ("background", &r.background)] {
        if let Some(value) = value {
            reported.insert(key.into(), json!(value));
        }
    }
    out.insert("reported".into(), Value::Object(reported));
    if let Some(usage) = &image.usage {
        out.insert("usage".into(), usage.clone());
    }
    out.insert("generationId".into(), json!(image.id));
    out.insert("createdAt".into(), json!(util::iso8601(util::now_secs())));
    Value::Object(out)
}

/// The request settings as the manifest records them: only the ones that were set.
pub fn request(aspect: Option<Aspect>, size: Option<&str>, quality: Option<&str>, background: Option<&str>) -> Value {
    let mut request = Map::new();
    let aspect = aspect.map(|a| format!("{}:{}", a.width, a.height));
    for (key, value) in [("aspect", aspect.as_deref()), ("size", size), ("quality", quality), ("background", background)] {
        if let Some(value) = value {
            request.insert(key.into(), json!(value));
        }
    }
    Value::Object(request)
}

pub fn write(path: &Path, manifest: &Value) -> Result<()> {
    let text = serde_json::to_string_pretty(manifest).map_err(|e| Error::other(e.to_string()))? + "\n";
    cli::write_output(path, text.as_bytes(), true).map(|_| ())
}

pub fn read(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Whether `prompt`, the `request` settings (from `request`) or an input differs from what
/// `manifest` recorded. Inputs are compared by fingerprint, so a re-rolled reference counts as a change.
pub fn changed(manifest: &Value, prompt: &str, request: &Value, inputs: &[PathBuf]) -> bool {
    if manifest.get("prompt").and_then(Value::as_str) != Some(prompt) || manifest.get("request") != Some(request) {
        return true;
    }
    let recorded: Vec<Option<&str>> = manifest.get("inputs").and_then(Value::as_array).map_or_else(Vec::new, |list| list.iter().map(|i| i.get("fingerprint").and_then(Value::as_str)).collect());
    let current: Vec<Option<String>> = inputs.iter().map(|p| fingerprint(p)).collect();
    recorded.len() != current.len() || recorded.iter().zip(&current).any(|(a, b)| a.is_none() || *a != b.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{Reported, Transport};
    use crate::images::Format;

    #[test]
    fn records_the_request_and_notices_changes() {
        let dir = crate::auth::tests::temp_dir("manifest");
        let reference = dir.join("ref.png");
        std::fs::write(&reference, b"one").unwrap();
        let image = Generated {
            bytes: Vec::new(),
            format: Format::Png,
            transport: Transport::Direct,
            id: "gen_1".into(),
            reported: Reported { size: Some("1024x1536".into()), quality: Some("low".into()), ..Default::default() },
            routing_model: None,
            revised_prompt: None,
            response_id: None,
            usage: Some(json!({"total_tokens": 9})),
            duration: std::time::Duration::ZERO,
        };
        let inputs = [Input { path: reference.clone(), role: "character", character: Some("captain".into()) }];
        let used = [(Kind::View, "side".to_string(), Source::BuiltIn)];
        let record = Record { user_prompt: "A fox.", prompt: "Camera: side.\n\nA fox.", used: &used, aspect: Some(Aspect { width: 2, height: 3 }), size: None, quality: None, background: Some("transparent"), inputs: &inputs };
        let manifest = build(&record, &image, &|p: &Path| p.file_name().unwrap().to_string_lossy().into_owned());
        assert_eq!(manifest["presets"], json!([{"kind": "view", "name": "side", "source": "built-in"}]));
        assert_eq!(manifest["request"], json!({"aspect": "2:3", "background": "transparent"}));
        assert_eq!(manifest["inputs"][0]["path"], "ref.png");
        assert_eq!(manifest["inputs"][0]["fingerprint"], json!(fingerprint(&reference).unwrap()));
        assert_eq!((manifest["reported"]["quality"].as_str(), manifest["generationId"].as_str()), (Some("low"), Some("gen_1")));

        let path = path_for(&dir.join("fox.png"));
        assert_eq!(path, dir.join("fox.png.json"));
        write(&path, &manifest).unwrap();
        let back = read(&path).unwrap();
        let same = request(Some(Aspect { width: 2, height: 3 }), None, None, Some("transparent"));
        assert!(!changed(&back, "Camera: side.\n\nA fox.", &same, std::slice::from_ref(&reference)));
        assert!(changed(&back, "A fox.", &same, std::slice::from_ref(&reference)), "the prompt changed");
        let opaque = request(Some(Aspect { width: 2, height: 3 }), None, None, Some("opaque"));
        assert!(changed(&back, "Camera: side.\n\nA fox.", &opaque, std::slice::from_ref(&reference)), "the background changed");
        assert!(changed(&back, "Camera: side.\n\nA fox.", &same, &[]), "an input was dropped");
        std::fs::write(&reference, b"two").unwrap();
        assert!(changed(&back, "Camera: side.\n\nA fox.", &same, &[reference]), "the reference was re-rolled");
    }
}
