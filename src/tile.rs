//! `codex-img tile`: make a panorama wrap around seamlessly, so it can repeat side by side
//! without a mirror. The image is rolled by half its width, which moves the wrap seam to the
//! centre, and sent as an edit asking the model to join it. The Codex endpoint ignores masks, so
//! the edit comes back redrawn everywhere, if only slightly: only the band around the seam is taken
//! from it, cut along the paths where edit and original agree best, colour-matched, and the image
//! is rolled back to its original framing.
use crate::auth;
use crate::backend::{Backend, Request, Transport};
use crate::cli;
use crate::convert;
use crate::error::{Error, Result};
use crate::events;
use crate::images::{self, Encoding, Format, InputImage};
use crate::manifest;
use crate::transform;
use crate::util;
use base64::Engine;
#[cfg(test)]
use image::{Rgba, RgbaImage};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub(crate) const PROMPT: &str = "This is a wide panorama whose left and right halves were swapped, so there is a hard vertical seam \
down the exact centre. Repaint only a narrow vertical band around the centre so the scenery continues naturally across it \
with no visible seam: the sky, clouds and landscape must flow smoothly from the left half into the right half. Keep \
everything else exactly as it is, pixel for pixel: same framing, same scale, same colours, same style. Do not move, resize \
or redraw anything outside the centre band.";

/// The cut may move this many pixels sideways per row, so it can follow a shallow slope.
use codex_img_core::tile::{roll, join_preview, splice};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileOptions {
    pub input: String,
    pub output: String,
    pub format: Format,
    /// What the picture shows, to help the model continue it.
    pub prompt: Option<String>,
    pub quality: Option<String>,
    pub preview: Option<String>,
    pub keep_edit: bool,
    pub force: bool,
    pub json: bool,
    pub quiet: bool,
    pub parent: Option<String>,
    pub no_parent: bool,
    pub manifest: bool,
    pub edit_only: bool,
    pub replay_reference: Option<(String, bool)>,
    pub submitted_prompt: Option<String>,
}

pub fn help() -> &'static str {
    r#"Usage:
  codex-img tile <input> -o <output> [options]

Makes a panorama wrap around seamlessly, so it can repeat side by side without a
mirror (skies and backdrops in side-scrolling and racing games). Uses one image of
quota. The image is rolled by half its width, which moves the wrap seam to the
centre, and the model repaints a band there to join it. Only that band is taken
from the edit, cut where the edit and the original agree, and the result keeps the
original framing: every pixel outside the band stays as it was.

Options:
  -o, --output <file>       The seamless image (png, jpeg or webp by extension)
      --prompt <text>       What the picture shows, e.g. "a coastline with a town
                            on a cliff"; helps the model continue it
  -q, --quality <q>         low | medium | high | auto (a hint to the backend)
      --preview <file>      Also write the result twice side by side, cropped
                            around the join, to check it in one image
      --keep-edit           Keep the model's whole edit as <output>.edit.png
      --parent <image>      Link to this earlier image instead of the input
      --no-parent           Do not infer a parent from the panorama
      --force               Replace existing output files
      --json                Print a JSON object describing the result
      --quiet               No progress on stderr

Landmarks that were cut in half at the edges (a mountain, a sun) come out whole.
The tile keeps its size; resize it afterwards with `codex-img convert`."#
}

pub fn parse(args: &[String]) -> Result<Option<TileOptions>> {
    let mut parent = None; let mut no_parent = false;
    let (mut input, mut output, mut prompt, mut quality, mut preview) = (None, None, None, None, None);
    let (mut keep_edit, mut force, mut json, mut quiet) = (false, false, false, false);
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if !arg.starts_with('-') {
            if input.replace(arg.clone()).is_some() {
                return Err(Error::usage("tile takes one input image."));
            }
            continue;
        }
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) if arg.starts_with("--") => (n, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        let mut value = || inline.clone().or_else(|| iter.next().cloned()).ok_or_else(|| Error::usage(format!("{name} needs a value.")));
        match name {
            "-o" | "--output" => output = Some(value()?),
            "--prompt" => prompt = Some(value()?),
            "-q" | "--quality" => quality = cli::one_of("quality", Some(value()?), &["low", "medium", "high", "auto"])?,
            "--preview" => preview = Some(value()?),
            "--parent" => parent = Some(value()?),
            "--no-parent" => no_parent = true,
            "--keep-edit" => keep_edit = true,
            "--force" => force = true,
            "--json" => json = true,
            "--quiet" => quiet = true,
            "-h" | "--help" => return Ok(None),
            _ => return Err(Error::usage(format!("Unknown tile option: {arg}"))),
        }
    }
    let input = input.ok_or_else(|| Error::usage("tile needs an input image."))?;
    let output = output.ok_or_else(|| Error::usage("tile needs -o <file>, e.g. -o sky-tile.png."))?;
    let format_of = |path: &str| Path::new(path).extension().and_then(|e| e.to_str()).and_then(Format::parse);
    let format = format_of(&output).ok_or_else(|| Error::usage("tile's -o must end in .png, .jpg or .webp."))?;
    if preview.as_deref().is_some_and(|p| format_of(p).is_none()) {
        return Err(Error::usage("--preview must end in .png, .jpg or .webp."));
    }
    if prompt.as_deref().is_some_and(|p| p.chars().count() > 4000) {
        return Err(Error::usage("--prompt must be at most 4,000 characters."));
    }
    if parent.is_some() && no_parent { return Err(Error::usage("--parent and --no-parent cannot be combined.")); }
    Ok(Some(TileOptions { input, output, format, prompt, quality, preview, keep_edit, force, json, quiet, parent, no_parent, manifest: false, edit_only: false, replay_reference: None, submitted_prompt: None }))
}

pub fn run(opts: &TileOptions) -> Result<i32> {
    execute(opts, &Backend::default(), &auth::load_credentials)
}

fn execute(opts: &TileOptions, backend: &Backend, credentials: &dyn Fn() -> Result<auth::Credentials>) -> Result<i32> {
    let cwd = std::env::current_dir().map_err(|e| Error::other(format!("No current folder: {e}")))?;
    execute_in(opts, backend, credentials, &cwd)
}

pub(crate) fn execute_in(opts: &TileOptions, backend: &Backend, credentials: &dyn Fn() -> Result<auth::Credentials>, cwd: &Path) -> Result<i32> {
    let started = Instant::now();
    let log = |message: &str| {
        if !opts.quiet {
            eprintln!("{message}");
        }
    };
    let output = PathBuf::from(&opts.output);
    let edit_path = output.with_file_name(format!("{}.edit.png", output.file_stem().unwrap_or_default().to_string_lossy()));
    let mut targets = vec![output.clone()];
    targets.extend(opts.preview.as_ref().map(PathBuf::from));
    if opts.keep_edit {
        targets.push(edit_path.clone());
    }
    let project = crate::project::root(cwd).is_some();
    if (project || opts.manifest) && !opts.force {
        targets.extend(targets.clone().iter().filter(|p| *p == &output || *p == &edit_path).map(|p| manifest::path_for(p)));
    }
    // Check before spending quota.
    if let Some(taken) = targets.iter().find(|p| !opts.force && p.exists()) {
        return Err(Error::other(format!("{} already exists; add --force to replace it.", taken.display())));
    }
    let (bytes, _) = convert::read_image(Path::new(&opts.input))?;
    if let Some((expected, anyway)) = &opts.replay_reference {
        let current = format!("fnv1a64:{:016x}", util::fnv1a64(&bytes));
        if expected != &current {
            if !anyway { return Err(Error::usage(format!("Reference changed while preparing rerun: {}. Stopped before quota.", opts.input))); }
            eprintln!("codex-img: warning: using changed reference {} (--anyway).", opts.input);
        }
    }
    let original = images::decode(&bytes)?;
    let (w, h) = original.dimensions();
    if w < 64 || h < 16 || w > 9999 || h > 9999 {
        return Err(Error::usage("tile needs an image from 64x16 to 9999x9999 pixels."));
    }
    let rolled = roll(&original, w / 2);
    let reference = InputImage { data_b64: base64::engine::general_purpose::STANDARD.encode(images::encode(&rolled, Format::Png, &Encoding::default())?), format: Format::Png };
    let run = events::Run::start_from(cwd, "tile", 1, json!({}));
    let credentials = credentials().inspect_err(|error| run.end(Some(error)))?;
    let prompt = opts.submitted_prompt.clone().unwrap_or_else(|| match &opts.prompt {
        Some(scene) => format!("{PROMPT} The picture shows: {scene}"),
        None => PROMPT.to_string(),
    });
    let size = format!("{w}x{h}");
    let session_id = util::random_id();
    log("Requesting the seam repair...");
    let request = Request {
        prompt: &prompt,
        transport: Transport::Direct,
        model: None,
        output_format: Format::Png,
        size: Some(&size),
        quality: opts.quality.as_deref(),
        background: Some("opaque"),
        input_images: std::slice::from_ref(&reference),
        session_id: &session_id,
    };
    let parent = opts.parent.as_ref().map(|path| cwd.join(path)).or_else(|| (!opts.no_parent).then(|| cwd.join(&opts.input)));
    let job = run.job(json!({
        "parent": parent.as_deref().map(events::absolute),
        "prompt": opts.prompt.as_deref().unwrap_or(PROMPT),
        "submittedPrompt": prompt,
        "inputs": [{"path": events::absolute(Path::new(&opts.input)), "role": "input"}],
        "request": manifest::request(None, Some(&size), opts.quality.as_deref(), Some("opaque")),
        "output": events::absolute(&output),
    }));
    let result = (|| -> Result<i32> {
        let generated = backend.generate(&request, &credentials, &|stage| {
            job.stage(stage);
            log(stage)
        })?;
        // The quota is spent: keep the edit first if asked, so a failure below doesn't lose it.
        if opts.keep_edit {
            cli::write_output(&edit_path, &generated.bytes, opts.force)?;
        }
        let edited = images::decode(&generated.bytes)?;
        let edited_size = edited.dimensions();
        let edited = if edited_size == (w, h) { edited } else { transform::resample(&edited, w, h) };
        let spliced = splice(&rolled, &edited);
        let tile = roll(&spliced.image, w - w / 2);

        let mut encoded = if opts.edit_only { images::convert(&generated.bytes, opts.format, &Encoding::default())? } else { images::encode(&tile, opts.format, &Encoding::default())? };
        if opts.format == Format::Png {
            encoded = images::optimize_png(&encoded).unwrap_or(encoded);
        }
        cli::write_output(&output, &encoded, opts.force)?;
        if let Some(preview) = &opts.preview {
            let format = Path::new(preview).extension().and_then(|e| e.to_str()).and_then(Format::parse).unwrap_or(Format::Png);
            cli::write_output(Path::new(preview), &images::encode(&join_preview(&tile), format, &Encoding::default())?, opts.force)?;
        }
        let (from, to) = spliced.band;
        let mut info = json!({
            "path": output.display().to_string(),
            "input": opts.input,
            "size": size,
            "editSize": format!("{}x{}", edited_size.0, edited_size.1),
            // The repaired band now straddles the left and right edges, where the tile meets itself.
            "bandWidth": to - from,
            "preview": opts.preview,
            "edit": opts.keep_edit.then(|| edit_path.display().to_string()),
            "durationMs": started.elapsed().as_millis() as u64,
        });
        if project || opts.manifest {
            let inputs = [manifest::Input { path: PathBuf::from(&opts.input), role: "input", character: None, fingerprint: Some(format!("fnv1a64:{:016x}", util::fnv1a64(&bytes))) }];
            let record = manifest::Record {
                user_prompt: opts.prompt.as_deref().unwrap_or(PROMPT),
                prompt: &prompt,
                used: &[],
                aspect: None,
                size: Some(&size),
                quality: opts.quality.as_deref(),
                background: Some("opaque"),
                inputs: &inputs,
                parent: parent.as_deref(),
                conversion: Some(&crate::conversion_record::build(opts.format, &Encoding::default(), &transform::Transform::default())),
            };
            let mut note = manifest::build(&record, &generated, &|p| run.manifest_path(p));
            note["source"] = json!("tile");
            note["tile"] = json!({"prompt":opts.prompt,"keepEdit":opts.keep_edit,"preview":opts.preview.is_some(),"editOnly":opts.edit_only});
            for path in [Some(&output), opts.keep_edit.then_some(&edit_path)].into_iter().flatten() {
                let mut note = note.clone();
                if let Some(root) = run.manifest_root(path) { note["root"] = json!(root); }
                if path == &edit_path {
                    note["tile"]["editOnly"] = json!(true);
                    note["conversion"] = crate::conversion_record::build(Format::Png, &Encoding::default(), &transform::Transform::default());
                }
                // --force may replace images, but existing manifests remain the user's files.
                if let Err(error) = manifest::write(&manifest::path_for(path), &note) {
                    eprintln!("codex-img: warning: could not write the manifest: {}", error.message);
                }
            }
            let path = manifest::path_for(&output);
            if path.is_file() {
                info["manifestPath"] = json!(path.display().to_string());
            }
        }
        if opts.json {
            println!("{info}");
        } else {
            println!("{}", output.display());
        }
        job.done(json!({"path": events::absolute(&output), "size": size, "durationMs": info["durationMs"].clone(),
            "manifestPath": info["manifestPath"].as_str().map(|p| events::absolute(Path::new(p)))}));
        log(&format!("saved {} ({size}, repaired band {} px wide)", output.display(), to - from));
        Ok(0)
    })();
    if let Err(error) = &result {
        job.failed(error);
    }
    run.end(None);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::tests::{creds, direct_response, serve};

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_options() {
        let o = parse(&args(&["sky.png", "-o", "sky-tile.webp", "--prompt", "a harbour", "--preview", "p.png", "--keep-edit"])).unwrap().unwrap();
        assert_eq!((o.format, o.prompt.as_deref(), o.preview.as_deref(), o.keep_edit), (Format::Webp, Some("a harbour"), Some("p.png"), true));
        assert_eq!(parse(&args(&["-h"])).unwrap(), None);
        let usage = |list: &[&str]| parse(&args(list)).unwrap_err().message;
        assert!(usage(&["-o", "t.png"]).contains("input"));
        assert!(usage(&["a.png", "b.png", "-o", "t.png"]).contains("one input"));
        assert!(usage(&["a.png"]).contains("-o"));
        assert!(usage(&["a.png", "-o", "t.gif"]).contains(".png"));
        assert!(usage(&["a.png", "-o", "t.png", "--preview", "p.gif"]).contains("--preview"));
        assert!(usage(&["a.png", "-o", "t.png", "-q", "max"]).contains("--quality"));
    }

    #[test]
    fn repairs_a_panorama_through_the_edits_endpoint() {
        let dir = crate::auth::tests::temp_dir("tile");
        let input = dir.join("sky.png");
        let sky = RgbaImage::from_fn(128, 32, |x, _| Rgba([(x * 2) as u8, 90, 200, 255]));
        sky.save(&input).unwrap();
        let (out, preview) = (dir.join("sky-tile.png"), dir.join("join.png"));
        let opts = parse(&args(&[input.to_str().unwrap(), "-o", out.to_str().unwrap(), "--preview", preview.to_str().unwrap(), "--keep-edit", "--quiet"]))
            .unwrap()
            .unwrap();
        let (backend, captured) = serve(vec![(200, "application/json", direct_response())]);
        assert_eq!(execute(&opts, &backend, &|| Ok(creds())).unwrap(), 0);
        let calls = captured.lock().unwrap();
        assert_eq!(calls[0].path, "/images/edits");
        assert!(calls[0].body["prompt"].as_str().unwrap().contains("halves were swapped"));
        assert_eq!(calls[0].body["size"], "128x32");
        let tile = image::open(&out).unwrap().to_rgba8();
        assert_eq!(tile.dimensions(), (128, 32));
        assert_eq!(tile.get_pixel(64, 0), sky.get_pixel(64, 0), "the middle of the original framing is untouched");
        assert!(preview.is_file() && dir.join("sky-tile.edit.png").is_file());
        assert!(execute(&opts, &backend, &|| Ok(creds())).unwrap_err().message.contains("--force"), "checked before spending quota");
    }

    #[test]
    fn reruns_tiles_with_the_original_panorama_and_links_new_outputs() {
        let dir = crate::auth::tests::temp_dir("tile-rerun").canonicalize().unwrap();
        std::fs::write(dir.join("codex-img.json"), "{}").unwrap();
        let input = dir.join("sky.png");
        RgbaImage::from_pixel(128, 32, Rgba([90, 90, 200, 255])).save(&input).unwrap();
        let output = dir.join("tile.webp");
        let opts = parse(&args(&[input.to_str().unwrap(), "-o", output.to_str().unwrap(), "--keep-edit", "--quiet"])).unwrap().unwrap();
        let (backend, captured) = serve(vec![(200, "application/json", direct_response()), (200, "application/json", direct_response())]);
        execute_in(&opts, &backend, &|| Ok(creds()), &dir).unwrap();
        let original = std::fs::read(&output).unwrap();
        let replay = crate::rerun::prepare(&crate::rerun::Options { image: output.display().to_string(), count: 1, output: Some("next.webp".into()), anyway: false, json: false, quiet: true }, &dir).unwrap();
        crate::rerun::execute_tile(&replay, &backend, &|| Ok(creds()), &dir).unwrap();
        let calls = captured.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].body["prompt"], calls[1].body["prompt"]);
        assert_eq!(calls[0].body["images"], calls[1].body["images"]);
        assert_eq!(manifest::read(&dir.join("next.webp.json")).unwrap()["parent"], "tile.webp");
        assert_eq!(manifest::read(&dir.join("next.edit.png.json")).unwrap()["conversion"]["format"], "png");
        assert_eq!(std::fs::read(&output).unwrap(), original);
        assert_eq!(image::open(dir.join("next.webp")).unwrap().width(), 128);
    }

    #[test]
    fn project_tiles_record_manifests_and_force_preserves_existing_notes() {
        let dir = crate::auth::tests::temp_dir("tile-project").canonicalize().unwrap();
        std::fs::write(dir.join("codex-img.json"), "{}").unwrap();
        let input = dir.join("sky.png");
        RgbaImage::from_pixel(128, 32, Rgba([90, 90, 200, 255])).save(&input).unwrap();
        let output = dir.join("tile.png");
        let mut opts = parse(&args(&[input.to_str().unwrap(), "-o", output.to_str().unwrap(), "--keep-edit", "--quiet"])).unwrap().unwrap();
        let (backend, captured) = serve(vec![(200, "application/json", direct_response()), (200, "application/json", direct_response())]);
        assert_eq!(execute_in(&opts, &backend, &|| Ok(creds()), &dir).unwrap(), 0);
        let path = manifest::path_for(&output);
        let mut note = manifest::read(&path).unwrap();
        assert_eq!(note["source"], "tile");
        assert_eq!(note["inputs"][0]["path"], "sky.png");
        assert!(dir.join("tile.edit.png.json").is_file());
        note["comment"] = json!("keep this note");
        std::fs::write(&path, note.to_string()).unwrap();
        opts.force = true;
        assert_eq!(execute_in(&opts, &backend, &|| Ok(creds()), &dir).unwrap(), 0);
        assert_eq!(manifest::read(&path).unwrap()["comment"], "keep this note");
        assert_eq!(captured.lock().unwrap().len(), 2);
    }
}
