mod auth;
mod backend;
mod cli;
mod convert;
mod error;
mod images;
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
        Command::Run(options) => generate(options),
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
    let chars = opts.prompt.chars().count();
    if opts.prompt.trim().is_empty() || chars > cli::MAX_PROMPT_CHARS {
        return Err(Error::usage("Image prompt must contain 1 to 32,000 characters."));
    }
    let format = opts.format.unwrap_or(Format::Png);
    let quiet = opts.quiet;
    let log = move |message: &str| {
        if !quiet {
            eprintln!("{message}");
        }
    };

    let credentials = Arc::new(auth::load_credentials()?);
    let inputs = Arc::new(images::load_input_images(&opts.images)?);
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
            let (opts, inputs, credentials, backend, session_id) =
                (opts.clone(), inputs.clone(), credentials.clone(), backend.clone(), session_id.clone());
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
                let target = cli::output_path(opts.output.as_deref(), format, &image.id, index, count, util::now_secs());
                let (path, warning) = cli::save_image(&image.bytes, image.format, format, opts.colors, opts.dither, &target)?;
                if let Some(warning) = warning {
                    eprintln!("codex-img: {tag}warning: {warning}");
                }
                let info = cli::describe(&path, &image);
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
