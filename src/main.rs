mod auth;
mod backend;
mod batch;
mod cli;
mod convert;
mod error;
mod images;
mod manifest;
mod presets;
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

fn generate(mut opts: Options) -> Result<i32> {
    if opts.prompt == "-" {
        let mut prompt = String::new();
        std::io::stdin().read_to_string(&mut prompt).map_err(|e| Error::other(format!("Could not read prompt from stdin: {e}")))?;
        opts.prompt = prompt.trim().to_string();
    }
    if opts.prompt.trim().is_empty() {
        return Err(Error::usage("Image prompt must contain 1 to 32,000 characters."));
    }
    // Preset files are read only when a preset is named, so a broken one can't stop plain runs.
    let library = if opts.setup.names_presets() {
        let cwd = std::env::current_dir().map_err(|e| Error::other(format!("No current folder: {e}")))?;
        presets::Places { cwd, global_dir: presets::global_dir() }.library()?
    } else {
        presets::Library::builtin()
    };
    let composed = presets::compose(&opts.prompt, None, &opts.setup, &library, opts.images.len())?;
    let user_prompt = std::mem::replace(&mut opts.prompt, composed.prompt.clone());
    if opts.prompt.chars().count() > cli::MAX_PROMPT_CHARS {
        return Err(Error::usage("Image prompt must contain 1 to 32,000 characters (with the presets and labels added)."));
    }
    let format = opts.format.unwrap_or(Format::Png);
    // parse() couldn't check against the default format; do it before spending quota.
    opts.encoding.check(Some(format))?;
    cli::check_output(opts.output.as_deref(), format, opts.count, &opts.transform, opts.manifest)?;
    let quiet = opts.quiet;
    let log = move |message: &str| {
        if !quiet {
            eprintln!("{message}");
        }
    };

    let credentials = Arc::new(auth::load_credentials()?);
    let mut paths = opts.images.clone();
    paths.extend(composed.images.iter().map(|i| i.path.display().to_string()));
    let inputs = Arc::new(images::load_input_images(&paths)?);
    let mut sent: Vec<manifest::Input> = opts.images.iter().map(|p| manifest::Input { path: p.into(), role: "input", character: None }).collect();
    sent.extend(composed.images.iter().map(|i| manifest::Input { path: i.path.clone(), role: i.role.name(), character: i.character.clone() }));
    let record = Arc::new((user_prompt, sent, composed.used));
    let session_id = util::random_id();
    let count = opts.count;
    log(&format!(
        "Requesting {}{}...",
        if count > 1 { format!("{count} images") } else { "image".into() },
        if inputs.is_empty() { String::new() } else { format!(" with {} reference(s)", inputs.len()) }
    ));

    let opts = Arc::new(opts);
    let backend = Arc::new(Backend::default());
    let handles: Vec<_> = (0..count)
        .map(|index| {
            let (opts, inputs, credentials, backend, session_id, record) =
                (opts.clone(), inputs.clone(), credentials.clone(), backend.clone(), session_id.clone(), record.clone());
            std::thread::spawn(move || -> Result<()> {
                let tag = if count > 1 { format!("[{}] ", index + 1) } else { String::new() };
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
                let image = backend.generate(&request, &credentials, &|stage| log(&format!("{tag}{stage}")))?;
                if let Some(warning) = opts.setup.aspect.zip(images::dimensions(&image.bytes)).and_then(|(a, size)| a.mismatch(size)) {
                    eprintln!("codex-img: {tag}warning: {warning}");
                }
                let target = cli::output_path(opts.output.as_deref(), format, &image.id, index, count, util::now_secs());
                let saved = cli::save_image(&image.bytes, image.format, format, &opts.encoding, &opts.transform, &target)?;
                if let Some(warning) = &saved.warning {
                    eprintln!("codex-img: {tag}warning: {warning}");
                }
                let mut info = cli::describe(&saved, &image);
                let (user_prompt, sent, used) = &*record;
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
                    };
                    let note = manifest::path_for(&saved.path);
                    match manifest::write(&note, &manifest::build(&request, &image, &|p| p.display().to_string())) {
                        Ok(()) => info["manifestPath"] = json!(note.display().to_string()),
                        Err(error) => eprintln!("codex-img: {tag}warning: could not write the manifest: {}", error.message),
                    }
                }
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
}
