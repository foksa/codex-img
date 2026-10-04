mod auth;
mod backend;
mod batch;
mod cli;
mod check;
mod history;
mod convert;
mod conversion_record;
mod rerun;
mod review;
mod refine;
mod refine_presets;
mod error;
mod events;
mod images;
mod json_edit;
mod manifest;
mod palette;
mod presets;
mod project;
mod registry;
mod sheet;
mod tile;
mod transform;
mod util;

use backend::{Backend, Request, Transport};
use cli::{Command, Options};
use error::{Error, Kind, Result};
use images::Format;
use serde_json::json;
use std::io::Read;
use std::sync::Arc;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(run(&args));
}

fn run(args: &[String]) -> i32 {
    let command = match cli::parse(args) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("codex-img: {error}\nRun `codex-img --help` for usage.");
            return Kind::Usage.exit_code();
        }
    };
    let result = match command {
        Command::Help => {
            println!("{}", cli::help());
            Ok(0)
        }
        Command::Version => {
            println!("{}", cli::VERSION);
            Ok(0)
        }
        Command::Status { json } => status(json),
        Command::ConvertHelp => {
            println!("{}", convert::help());
            Ok(0)
        }
        Command::Convert(options) => Ok(convert::run(&options)),
        Command::SheetHelp => {
            println!("{}", sheet::help());
            Ok(0)
        }
        Command::Sheet(options) => sheet::run(&options),
        Command::BatchHelp => {
            println!("{}", batch::help());
            Ok(0)
        }
        Command::Batch(options) => batch::run(&options),
        Command::TileHelp => {
            println!("{}", tile::help());
            Ok(0)
        }
        Command::Tile(options) => tile::run(&options),
        Command::PresetsHelp => {
            println!("{}", presets::help());
            Ok(0)
        }
        Command::Presets(options) => presets::run(&options),
        Command::CheckHelp => { println!("{}", check::help()); Ok(0) },
        Command::Check(options) => check::run(&options),
        Command::InitHelp => { println!("{}", project::init_help()); Ok(0) },
        Command::Init(options) => project::run_init(&options),
        Command::RefineHelp => { println!("{}", refine::help()); Ok(0) },
        Command::Refine(options) if options.batch.is_some() => batch::refine(&options),
        Command::Refine(options) => std::env::current_dir().map_err(|e| Error::other(e.to_string())).and_then(|cwd| refine::prepare(&options, &cwd)).and_then(generate),
        Command::ReviewHelp(star) => { println!("{}", review::help(star)); Ok(0) },
        Command::Review(options) => review::run(&options),
        Command::RerunHelp => { println!("{}", rerun::help()); Ok(0) },
        Command::Rerun(options) => std::env::current_dir().map_err(|e| Error::other(e.to_string())).and_then(|cwd| rerun::prepare(&options, &cwd)).and_then(generate),
        Command::Run(options) => generate(*options),
    };
    result.unwrap_or_else(|error| {
        eprintln!("codex-img: {error}");
        error.kind.exit_code()
    })
}

fn status(json: bool) -> Result<i32> {
    let info = auth::login_status()?;
    if json {
        println!("{}", json!({"ok": true, "authPath": info.auth_path.display().to_string(), "accountId": info.account_id, "expiresAt": info.expires_at}));
    } else {
        println!(
            "Logged in (account {}); token valid until {}.\nAuth file: {}",
            info.account_id,
            info.expires_at.as_deref().unwrap_or("unknown"),
            info.auth_path.display()
        );
    }
    Ok(0)
}

fn generate(opts: Options) -> Result<i32> {
    let cwd = std::env::current_dir().map_err(|e| Error::other(format!("No current folder: {e}")))?;
    execute(opts, Backend::default(), &auth::load_credentials, &cwd)
}

fn execute(mut opts: Options, backend: Backend, credentials: &dyn Fn() -> Result<auth::Credentials>, cwd: &std::path::Path) -> Result<i32> {
    if opts.replay.as_ref().is_some_and(|saved| saved.tile.is_some()) { return rerun::execute_tile(&opts, &backend, credentials, cwd); }
    if opts.prompt == "-" {
        let mut prompt = String::new();
        std::io::stdin().read_to_string(&mut prompt).map_err(|e| Error::other(format!("Could not read prompt from stdin: {e}")))?;
        opts.prompt = prompt.trim().to_string();
    }
    if opts.prompt.trim().is_empty() {
        return Err(Error::usage("Image prompt must contain 1 to 32,000 characters."));
    }
    // Preset files are read only when a preset is named, so a broken one can't stop plain runs.
    let places = presets::Places { cwd: cwd.to_path_buf(), global_dir: presets::global_dir() };
    let project = project::root(cwd).is_some();
    opts.manifest |= project;
    let library = if opts.replay.is_none() && opts.setup.names_presets() { places.library()? } else { presets::Library::builtin() };
    if let Some(spec) = &opts.palette {
        let colors = palette::resolve(spec, cwd, || places.library())?;
        opts.transform.palette = Some(transform::PaletteFit { colors: colors.clone(), clean: opts.palette_clean });
        opts.setup.palette = Some(colors);
    }
    let saved_setup = opts.replay.as_ref().and_then(|saved| saved.setup.clone());
    let explicit_inputs = manifest::composer_inputs(&opts, cwd);
    let (user_prompt, mut sent, used, allow_changed) = if let Some(saved) = opts.replay.take() {
        opts.prompt = saved.prompt;
        (saved.user_prompt, saved.inputs, saved.used, Some(saved.allow_changed))
    } else {
        let composed = presets::compose(&opts.prompt, None, &opts.setup, &library, opts.images.len())?;
        let user_prompt = std::mem::replace(&mut opts.prompt, composed.prompt);
        let mut sent: Vec<manifest::Input> = opts.images.iter().map(|path| manifest::Input { path: cwd.join(path), role: "input", character: None, fingerprint: None }).collect();
        sent.extend(composed.images.into_iter().map(|image| manifest::Input { path: cwd.join(image.path), role: image.role.name(), character: image.character, fingerprint: None }));
        (user_prompt, sent, composed.used, None)
    };
    let plain: Vec<_> = sent.iter().filter(|input| input.role == "input").collect();
    let parent = opts.parent.as_ref().map(|path| cwd.join(path)).or_else(|| (!opts.no_parent && plain.len() == 1).then(|| plain[0].path.clone()));
    let conversion = conversion_record::build(opts.format.unwrap_or(Format::Png), &opts.encoding, &opts.transform);
    if opts.prompt.chars().count() > cli::MAX_PROMPT_CHARS {
        return Err(Error::usage("Image prompt must contain 1 to 32,000 characters (with the presets and labels added)."));
    }
    let format = opts.format.unwrap_or(Format::Png);
    // parse() couldn't check against the default format; do it before spending quota.
    opts.encoding.check(Some(format))?;
    opts.transform.check_output(Some(format), &opts.encoding)?;
    cli::check_output(opts.output.as_deref(), format, opts.count, &opts.transform, opts.manifest, project)?;
    let quiet = opts.quiet;
    let log = move |message: &str| {
        if !quiet {
            eprintln!("{message}");
        }
    };

    let run = events::Run::start_from(cwd, "run", opts.count, json!({}));
    let result = (|| -> Result<i32> {
        let credentials = Arc::new(credentials()?);
        let paths: Vec<_> = sent.iter().map(|input| input.path.display().to_string()).collect();
        let inputs = Arc::new(images::load_input_images(&paths)?);
        for (input, bytes) in sent.iter_mut().zip(inputs.iter()) {
            if manifest::capture(input, bytes) && allow_changed.is_some() {
                if allow_changed == Some(false) { return Err(Error::usage(format!("Reference changed while preparing rerun: {}. Stopped before quota.", input.path.display()))); }
                eprintln!("codex-img: warning: using changed reference {} (--anyway).", input.path.display());
            }
        }
        let record = Arc::new((user_prompt, sent, used, parent, conversion, saved_setup, explicit_inputs));
        let session_id = util::random_id();
        let count = opts.count;
        log(&format!(
            "Requesting {}{}...",
            if count > 1 { format!("{count} images") } else { "image".into() },
            if inputs.is_empty() { String::new() } else { format!(" with {} reference(s)", inputs.len()) }
        ));

        let opts = Arc::new(opts);
        let backend = Arc::new(backend);
        let handles: Vec<_> = (0..count)
            .map(|index| {
                let (opts, inputs, credentials, backend, session_id, record, run) =
                    (opts.clone(), inputs.clone(), credentials.clone(), backend.clone(), session_id.clone(), record.clone(), run.clone());
                std::thread::spawn(move || -> Result<()> {
                    let tag = if count > 1 { format!("[{}] ", index + 1) } else { String::new() };
                    let (user_prompt, sent, used, parent, conversion, saved_setup, explicit_inputs) = &*record;
                    let job = run.job(json!({
                        "prompt": user_prompt,
                        "parent": parent.as_deref().map(events::absolute),
                        "submittedPrompt": (*user_prompt != opts.prompt).then_some(&opts.prompt),
                        "inputs": events::inputs(sent),
                        "request": manifest::request(opts.setup.aspect, opts.size.as_deref(), opts.quality.as_deref(), opts.background.as_deref()),
                        "output": opts.output.as_deref().map(|o| events::absolute(std::path::Path::new(o))),
                        "index": (count > 1).then_some(index),
                        "kind": opts.kind,
                    }));
                    let result = (|| -> Result<()> {
                        let request = Request {
                            prompt: &opts.prompt,
                            transport: if opts.via_responses { Transport::Responses } else { Transport::Direct },
                            model: opts.model.as_deref(),
                            output_format: format,
                            size: opts.size.as_deref(),
                            quality: opts.quality.as_deref(),
                            background: opts.background.as_deref(),
                            input_images: &inputs,
                            session_id: &session_id,
                        };
                        let image = backend.generate(&request, &credentials, &|stage| {
                            job.stage(stage);
                            log(&format!("{tag}{stage}"))
                        })?;
                        if let Some(warning) = opts.setup.aspect.zip(images::dimensions(&image.bytes)).and_then(|(a, size)| a.mismatch(size)) {
                            eprintln!("codex-img: {tag}warning: {warning}");
                        }
                        let target = cli::output_path(opts.output.as_deref(), format, &image.id, index, count, util::now_secs());
                        let saved = cli::save_image(&image.bytes, image.format, format, &opts.encoding, &opts.transform, &target)?;
                        if let Some(warning) = &saved.warning {
                            eprintln!("codex-img: {tag}warning: {warning}");
                        }
                        let mut info = cli::describe(&saved, &image);
                        if *user_prompt != opts.prompt {
                            info["submittedPrompt"] = json!(opts.prompt);
                        }
                        if opts.manifest {
                            let request = manifest::Record {
                                user_prompt,
                                prompt: &opts.prompt,
                                used,
                                aspect: opts.setup.aspect,
                                size: opts.size.as_deref(),
                                quality: opts.quality.as_deref(),
                                background: opts.background.as_deref(),
                                inputs: sent,
                                parent: parent.as_deref(),
                                conversion: Some(conversion),
                            };
                            let note = manifest::path_for(&saved.path);
                            let mut record = manifest::build(&request, &image, &|p| run.manifest_path(p));
                            record["setup"] = if let Some(mut setup) = saved_setup.clone() {
                                if let Some(inputs) = setup.get_mut("inputs").and_then(serde_json::Value::as_array_mut) {
                                    for input in inputs { if let Some(path) = input["path"].as_str() { input["path"] = json!(run.manifest_path(std::path::Path::new(path))); } }
                                }
                                setup
                            } else { manifest::setup(&opts, explicit_inputs, &|path| run.manifest_path(path)) };
                            if let Some(transfer) = &opts.transfer { record["fromComment"] = json!(transfer.comment); }
                            if let Some(kind) = opts.kind { record["kind"] = json!(kind); }
                            if let Some(root) = run.manifest_root(&saved.path) { record["root"] = json!(root); }
                            match manifest::write(&note, &record) {
                                Ok(()) => { info["manifestPath"] = json!(note.display().to_string()); if let Some(transfer) = &opts.transfer { refine::finish(transfer); } },
                                Err(error) => eprintln!("codex-img: {tag}warning: could not write the manifest: {}", error.message),
                            }
                            if let Some(raw) = saved.raw_path.as_deref().filter(|_| run.in_project()) {
                                record["conversion"] = conversion_record::build(image.format, &images::Encoding::default(), &transform::Transform::default());
                                if let Err(error) = manifest::write(&manifest::path_for(raw), &record) {
                                    eprintln!("codex-img: {tag}warning: could not write the raw manifest: {}", error.message);
                                }
                            }
                        }
                        job.done(json!({
                            "path": events::absolute(&saved.path),
                            "rawPath": saved.raw_path.as_deref().map(events::absolute),
                            "manifestPath": info["manifestPath"].as_str().map(|p| events::absolute(std::path::Path::new(p))),
                            "size": info["size"].clone(),
                            "durationMs": image.duration.as_millis() as u64,
                        }));
                        let path = saved.path;
                        if opts.json {
                            println!("{info}");
                        } else {
                            println!("{}", path.display());
                        }
                        log(&format!(
                            "{tag}saved {} ({}, {:.1}s)",
                            path.display(),
                            info["size"].as_str().unwrap_or("?"),
                            image.duration.as_secs_f64()
                        ));
                        Ok(())
                    })();
                    if let Err(error) = &result {
                        job.failed(error);
                    }
                    result
                })
            })
            .collect();

        let mut exit_code = 0;
        for handle in handles {
            let result = handle.join().unwrap_or_else(|_| Err(Error::other("Image worker panicked.")));
            if let Err(error) = result {
                eprintln!("codex-img: {error}");
                exit_code = exit_code.max(error.kind.exit_code());
            }
        }
        Ok(exit_code)
    })();
    run.end(result.as_ref().err());
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::tests::{creds, direct_response, serve};
    use std::path::Path;

    fn options(output: &Path, extra: &[&str]) -> Options {
        let mut args: Vec<String> = ["a cat", "-o", output.to_str().unwrap(), "--quiet"].iter().map(|s| s.to_string()).collect();
        args.extend(extra.iter().map(|s| s.to_string()));
        let Command::Run(opts) = cli::parse(&args).unwrap() else { panic!("expected run") };
        *opts
    }

    #[test]
    fn refines_comments_with_a_new_parent_and_preserves_notes_on_failure_or_change() {
        let dir = auth::tests::temp_dir("refine-comments").canonicalize().unwrap();
        std::fs::write(dir.join("codex-img.json"), "{}").unwrap();
        let original = dir.join("original.png"); std::fs::copy("tests/fixtures/sprite.png", &original).unwrap();
        let path = manifest::path_for(&original);
        let note = json!({"prompt":format!("{}\n\nA fox", cli::Aspect {width:1,height:1}.sentence()),"userPrompt":"A fox","request":{"aspect":"1:1","background":"transparent"},"comment":"Make the hat blue","star":true});
        manifest::write(&path, &note).unwrap();
        let opts = refine::Options {image: original.display().to_string(), change: None, from_comment: true, expected: Some("Make the hat blue".into()), output: Some(dir.join("edited.png").display().to_string()), json: false, quiet: true, batch: None, key: None, no_wait: false};
        let prepared = refine::prepare(&opts, &dir).unwrap();
        let (backend, requests) = serve(vec![(200, "application/json", direct_response())]);
        assert_eq!(execute(prepared, backend, &|| Ok(creds()), &dir).unwrap(), 0);
        let saved = manifest::read(&dir.join("edited.png.json")).unwrap();
        assert_eq!(saved["fromComment"], "Make the hat blue"); assert_eq!(saved["parent"], "original.png"); assert_eq!(saved["kind"], "edit");
        assert_eq!(saved["request"], note["request"]); assert!(saved.get("star").is_none());
        assert!(saved["prompt"].as_str().unwrap().contains("Change only: Make the hat blue. Keep everything else exactly as it is."));
        assert_eq!(requests.lock().unwrap().len(), 1);
        let old = manifest::read(&path).unwrap(); assert!(old.get("comment").is_none()); assert_eq!(old["star"], true);
        review::edit(&path, None, "comment", &json!("Make the hat blue"), Some(&json!(""))).unwrap();
        let failed = refine::Options {output: Some(dir.join("failed.png").display().to_string()), ..opts.clone()};
        let prepared = refine::prepare(&failed, &dir).unwrap();
        let (backend, _) = serve(vec![(401, "application/json", "{}".into())]);
        assert_ne!(execute(prepared, backend, &|| Ok(creds()), &dir).unwrap(), 0);
        assert_eq!(manifest::read(&path).unwrap()["comment"], "Make the hat blue");
        let changed = refine::Options {output: Some(dir.join("changed.png").display().to_string()), ..opts};
        let prepared = refine::prepare(&changed, &dir).unwrap();
        review::edit(&path, None, "comment", &json!("A newer note"), None).unwrap();
        let (backend, _) = serve(vec![(200, "application/json", direct_response())]);
        assert_eq!(execute(prepared, backend, &|| Ok(creds()), &dir).unwrap(), 0);
        assert_eq!(manifest::read(&path).unwrap()["comment"], "A newer note");
        assert!(refine::prepare(&changed, &dir).unwrap_err().message.contains("changed on disk"));
    }
    #[test]
    fn refine_uses_current_named_presets_and_keeps_names_for_later_edits() {
        let dir = auth::tests::temp_dir("refine-current").canonicalize().unwrap();
        let marker = dir.join("codex-img.json"); let reference = dir.join("ink.png");
        std::fs::copy("tests/fixtures/sprite.png", &reference).unwrap();
        std::fs::write(&marker, r##"{"styles":{"ink":{"text":"Old ink","refs":["ink.png"]}},"palettes":{"mono":"#000000 #FFFFFF"}}"##).unwrap();
        let first = dir.join("first.png");
        let (backend, _) = serve(vec![(200, "application/json", direct_response())]);
        execute(options(&first, &["--style", "ink", "--palette", "mono"]), backend, &|| Ok(creds()), &dir).unwrap();
        std::fs::write(&marker, r##"{"styles":{"ink":{"text":"New ink","refs":["ink.png"]}},"palettes":{"mono":"#001122 #FFFFFF"}}"##).unwrap();
        let opts = refine::Options {image: first.display().to_string(), change: Some("Blue hat".into()), from_comment: false, expected: None, output: Some(dir.join("child.png").display().to_string()), json: false, quiet: true, batch: None, key: None, no_wait: false};
        let prepared = refine::prepare(&opts, &dir).unwrap();
        let (backend, captured) = serve(vec![(200, "application/json", direct_response())]);
        assert_eq!(execute(prepared, backend, &|| Ok(creds()), &dir).unwrap(), 0);
        assert_eq!(captured.lock().unwrap().len(), 1);
        let child = manifest::read(&dir.join("child.png.json")).unwrap();
        assert!(child["prompt"].as_str().unwrap().contains("New ink")); assert!(child["prompt"].as_str().unwrap().contains("#001122"));
        assert_eq!(child["inputs"].as_array().unwrap().len(), 2);
        assert_eq!(child["inputs"][0]["path"], "first.png"); assert_eq!(child["inputs"][1]["role"], "style");
        assert_eq!(child["setup"]["styles"], json!(["ink"])); assert_eq!(child["setup"]["palette"], "mono");
        assert!(child["presets"][0].get("text").is_none());
    }
    #[test]
    fn reruns_frozen_presets_and_conversions_without_overwriting_or_spending_on_invalid_refs() {
        let dir = auth::tests::temp_dir("rerun").canonicalize().unwrap();
        std::fs::write(dir.join("codex-img.json"), r#"{"styles":{"ink":"Ink drawing"}}"#).unwrap();
        let reference = dir.join("ref.png"); std::fs::copy("tests/fixtures/sprite.png", &reference).unwrap();
        let first = dir.join("first.png");
        let extra = ["-i", reference.to_str().unwrap(), "--style", "ink", "--trim=2", "--resize=32x", "-b", "transparent"];
        let (backend, _) = serve(vec![(200, "application/json", direct_response())]);
        let nested = dir.join("nested"); std::fs::create_dir(&nested).unwrap();
        assert_eq!(execute(options(&first, &extra), backend, &|| Ok(creds()), &nested).unwrap(), 0);
        let original = manifest::read(&manifest::path_for(&first)).unwrap();
        assert_eq!(original["parent"], "ref.png"); assert!(original["conversion"].is_object());
        assert_eq!(original["setup"]["inputs"][0]["path"], "ref.png");
        std::fs::write(dir.join("codex-img.json"), "broken changed presets").unwrap();
        let second = dir.join("second.png");
        let rerun = rerun::Options { image: first.display().to_string(), output: Some(second.display().to_string()), count: 2, anyway: false, json: false, quiet: true };
        let prepared = rerun::prepare(&rerun, &dir).unwrap();
        let (backend, captured) = serve(vec![(200, "application/json", direct_response()), (200, "application/json", direct_response())]);
        assert_eq!(execute(prepared, backend, &|| Ok(creds()), &dir).unwrap(), 0);
        assert_eq!(captured.lock().unwrap().len(), 2);
        for index in 1..=2 {
            let note = manifest::read(&dir.join(format!("second-{index}.png.json"))).unwrap();
            assert_eq!(note["prompt"], original["prompt"]); assert_eq!(note["presets"], original["presets"]);
            assert_eq!(note["conversion"], original["conversion"]); assert_eq!(note["parent"], "first.png"); assert_eq!(note["kind"], "rerun");
            assert_eq!(note["setup"], original["setup"]);
        }
        assert_eq!(manifest::read(&manifest::path_for(&first)).unwrap(), original);
        let mut changed = std::fs::read(&reference).unwrap(); changed.push(0);
        std::fs::write(&reference, changed).unwrap();
        assert!(rerun::prepare(&rerun, &dir).unwrap_err().message.contains("Reference changed"));
        let anyway = rerun::Options { anyway: true, ..rerun.clone() }; assert!(rerun::prepare(&anyway, &dir).is_ok());
        std::fs::remove_file(&reference).unwrap(); assert!(rerun::prepare(&anyway, &dir).unwrap_err().message.contains("Missing"));
    }
    #[test]
    fn suppresses_inferred_parent_and_honors_explicit_parent() {
        let dir = auth::tests::temp_dir("parents"); let reference = dir.join("ref.png"); std::fs::copy("tests/fixtures/sprite.png", &reference).unwrap();
        for (name, flags, expected) in [("unlinked", vec!["--no-parent"], None), ("explicit", vec!["--parent", reference.to_str().unwrap()], Some(reference.display().to_string()))] {
            let target = dir.join(format!("{name}.png"));
            let mut extra = vec!["-i", reference.to_str().unwrap(), "--manifest"]; extra.extend(flags);
            let (backend, _) = serve(vec![(200, "application/json", direct_response())]);
            execute(options(&target, &extra), backend, &|| Ok(creds()), &dir).unwrap();
            assert_eq!(manifest::read(&manifest::path_for(&target)).unwrap()["parent"].as_str(), expected.as_deref());
        }
    }
    #[test]
    fn project_generations_get_manifests_even_with_logging_off() {
        let dir = auth::tests::temp_dir("plain-project").canonicalize().unwrap();
        std::fs::create_dir_all(dir.join("art")).unwrap();
        // A plain run discovers the project without parsing its unused presets.
        std::fs::write(dir.join("codex-img.json"), "broken unused presets").unwrap();
        let reference = dir.join("art/reference.png");
        std::fs::copy("tests/fixtures/sprite.png", &reference).unwrap();
        let output = dir.join("art/cat.png");
        let opts = options(&output, &["-i", reference.to_str().unwrap(), "-n", "2", "--trim"]);
        let mut response: serde_json::Value = serde_json::from_str(&direct_response()).unwrap();
        response["data"][0]["b64_json"] = json!(base64::Engine::encode(&base64::engine::general_purpose::STANDARD, std::fs::read(&reference).unwrap()));
        let (backend, captured) = serve(vec![(200, "application/json", response.to_string()), (200, "application/json", response.to_string())]);
        assert_eq!(execute(opts, backend, &|| Ok(creds()), &dir.join("art")).unwrap(), 0);
        assert_eq!(captured.lock().unwrap().len(), 2);
        for index in 1..=2 {
            let note = manifest::read(&dir.join(format!("art/cat-{index}.png.json"))).unwrap();
            assert_eq!(note["userPrompt"], "a cat");
            assert_eq!(note["inputs"][0]["path"], "art/reference.png");
            assert!(dir.join(format!("art/cat-{index}.raw.png.json")).exists());
        }
        assert!(!dir.join(".codex-img").exists(), "test logging stays disabled");
    }

    #[test]
    fn loose_generations_keep_manifests_opt_in_and_check_collisions_before_quota() {
        let dir = auth::tests::temp_dir("plain-loose");
        for (name, extra, manifest_expected) in [("plain", vec![], false), ("recorded", vec!["--manifest"], true)] {
            let out = dir.join(format!("{name}.png"));
            let (backend, captured) = serve(vec![(200, "application/json", direct_response())]);
            assert_eq!(execute(options(&out, &extra), backend, &|| Ok(creds()), &dir).unwrap(), 0);
            assert_eq!(captured.lock().unwrap().len(), 1);
            assert_eq!(manifest::path_for(&out).exists(), manifest_expected);
        }
        std::fs::write(dir.join("codex-img.json"), "{}").unwrap();
        let out = dir.join("taken.png");
        std::fs::write(manifest::path_for(&out), "my comment").unwrap();
        let no_login = || -> Result<auth::Credentials> { panic!("must fail before login or quota") };
        let error = execute(options(&out, &[]), Backend::new("http://127.0.0.1:9"), &no_login, &dir).unwrap_err();
        assert!(error.message.contains("taken.png.json"));
        assert_eq!(std::fs::read_to_string(manifest::path_for(&out)).unwrap(), "my comment");
        let out = dir.join("raw-taken.png");
        std::fs::write(dir.join("raw-taken.raw.png.json"), "raw comment").unwrap();
        let error = execute(options(&out, &["--trim"]), Backend::new("http://127.0.0.1:9"), &no_login, &dir).unwrap_err();
        assert!(error.message.contains("raw-taken.raw.png.json"));
    }
}
