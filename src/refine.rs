//! Edit a saved image and transfer its comment only after a new manifest is safe on disk.
use crate::{cli::{self, Command}, error::{Error, Result}, manifest, review};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options { pub image: String, pub change: Option<String>, pub from_comment: bool, pub expected: Option<String>, pub output: Option<String>, pub json: bool, pub quiet: bool, pub batch: Option<String>, pub key: Option<String>, pub no_wait: bool }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transfer { pub path: PathBuf, pub comment: String }
pub fn help() -> &'static str {
    "Usage: codex-img refine <image> <change> [-o out] [--json] [--quiet]\n       codex-img refine <image> --from-comment [--expect-comment <text>] [-o out] [--json] [--quiet]\n\nChange only what the edit says, keeping the image's aspect, background and current named presets.\nCosts 1 image; creates a fresh image and manifest linked to the original as parent.\n--from-comment records fromComment on the new manifest and clears the original\ncomment only after success. Failed edits leave it intact. Batch edits use --batch <spec> --key <key> instead of an image, and apply the\ncurrent batch conversion after preserving the old raw in history. Locks are per spec; --no-wait fails immediately if busy. Current preset text/reference drift is warned with short recoverable old/new text excerpts;\nmissing recorded presets or current references stop before quota."
}
pub fn parse(args: &[String]) -> Result<Option<Options>> {
    let mut out = Options {image: String::new(), change: None, from_comment: false, expected: None, output: None, json: false, quiet: false, batch: None, key: None, no_wait: false};
    let mut iter = args.iter(); let mut positional = false;
    while let Some(arg) = iter.next() {
        if positional || !arg.starts_with('-') {
                if out.image.is_empty() { out.image = arg.clone(); } else if out.change.replace(arg.clone()).is_some() { return Err(Error::usage("refine takes one image and one change.")); }
            continue;
        }
        if arg == "--" { positional = true; continue; }
        let (flag, inline) = arg.split_once('=').map_or((arg.as_str(), None), |(f, v)| (f, Some(v)));
        let mut value = || inline.map(str::to_string).or_else(|| iter.next().cloned()).ok_or_else(|| Error::usage(format!("{flag} needs a value.")));
        match flag {
            "-o" | "--output" => out.output = Some(value()?),
            "--no-wait" if inline.is_none() => out.no_wait = true,
            "--batch" => out.batch = Some(value()?),
            "--key" => out.key = Some(value()?),
            "--expect-comment" => out.expected = Some(value()?),
            "--from-comment" if inline.is_none() => out.from_comment = true,
            "--json" if inline.is_none() => out.json = true,
            "--quiet" if inline.is_none() => out.quiet = true,
            "-h" | "--help" => return Ok(None),
            _ => return Err(Error::usage(format!("Unknown refine option: {arg}"))),
        }
    }
    if out.batch.is_some() {
        if out.key.is_none() || out.output.is_some() { return Err(Error::usage("Batch refine needs --key and uses the asset raw slot; omit -o.")); }
        if !out.image.is_empty() && out.change.is_some() { return Err(Error::usage("Batch refine takes one change, without an image positional argument.")); }
        if !out.image.is_empty() && out.change.is_none() { out.change = Some(std::mem::take(&mut out.image)); }
        if out.from_comment == out.change.is_some() || out.expected.is_some() && !out.from_comment { return Err(Error::usage("Batch refine needs a change or --from-comment.")); }
        return Ok(Some(out));
    }
    if out.no_wait { return Err(Error::usage("--no-wait applies to --batch edits only.")); }
    if out.key.is_some() { return Err(Error::usage("--key needs --batch.")); }
    if out.image.is_empty() || out.from_comment == out.change.is_some() || out.expected.is_some() && !out.from_comment { return Err(Error::usage("refine needs an image and a change, or --from-comment.")); }
    Ok(Some(out))
}
pub fn prepare(opts: &Options, cwd: &Path) -> Result<cli::Options> {
    prepare_in_library(opts, cwd, None)
}

pub fn prepare_in_library(opts: &Options, cwd: &Path, library: Option<crate::presets::Library>) -> Result<cli::Options> {
    let image = crate::batch::resolved(&cwd.join(&opts.image));
    let path = manifest::path_for(&image);
    let note = review::read(&path)?;
    let change = if opts.from_comment {
        let text = note["comment"].as_str().ok_or_else(|| Error::usage("The image has no comment to send as an edit."))?;
        if opts.expected.as_deref().is_some_and(|expected| expected != text) { return Err(Error::usage("The comment changed on disk. Reload it before sending the edit.")); }
        text.to_string()
    } else { opts.change.clone().unwrap() };
    if change.trim().is_empty() { return Err(Error::usage("The edit must not be empty.")); }
    manifest::fingerprint(&image).ok_or_else(|| Error::usage("The image is missing or unreadable. Refine stopped before quota."))?;
    let prompt = edit_prompt(&change);
    let mut args = vec![prompt, "--manifest".into(), format!("--image={}", image.display()), format!("--parent={}", image.display())];
    for key in ["aspect", "background", "quality", "size"] {
        if let Some(value) = note["request"].get(key).filter(|v| !v.is_null()) {
            let value = value.as_str().ok_or_else(|| Error::usage(format!("request.{key} must be a string.")))?;
            args.push(format!("--{key}={value}"));
        }
    }
    if let Some(conversion) = note.get("conversion") {
        let format = conversion["format"].as_str().ok_or_else(|| Error::usage("The conversion needs a format."))?;
        args.push(format!("--format={format}"));
        for arg in conversion["args"].as_array().ok_or_else(|| Error::usage("The conversion needs an args list."))? {
            let arg = arg.as_str().filter(|arg| crate::conversion_record::allowed(arg)).ok_or_else(|| Error::usage("Invalid conversion option."))?;
            args.push(arg.into());
        }
    }
    match note["transport"].as_str() {
        Some("responses") => { args.push("--via-responses".into()); if let Some(model) = note["routingModel"].as_str() { args.push(format!("--model={model}")); } },
        Some("direct") | None => {},
        _ => return Err(Error::usage("Unknown recorded transport.")),
    }
    if let Some(output) = &opts.output { args.push(format!("--output={}", cwd.join(output).display())); }
    if opts.json { args.push("--json".into()); }
    if opts.quiet { args.push("--quiet".into()); }
    let Command::Run(options) = cli::parse(&args)? else { return Err(Error::usage("Invalid refine request.")); };
    let mut options = *options;
    let (_, warnings) = crate::refine_presets::prepare_in_library(&note, &image, cwd, &mut options, library)?;
    for warning in warnings { eprintln!("codex-img: warning: {warning}"); }
    options.transfer = opts.from_comment.then_some(Transfer {path, comment: change});
    options.kind = Some("edit");
    Ok(options)
}
pub fn finish(transfer: &Transfer) {
    if let Err(error) = review::edit(&transfer.path, None, "comment", &json!(""), Some(&Value::String(transfer.comment.clone()))) {
        eprintln!("codex-img: warning: the new version is saved, but the original comment was preserved: {}", error.message);
    }
}

const KEEP_REST: &str = "Keep everything else exactly as it is";

/// The prompt an edit submits. A comment that already ends with the closing sentence, or with a
/// full stop, is not given a second one.
fn edit_prompt(change: &str) -> String {
    let mut change = change.trim();
    loop {
        change = change.trim_end_matches(|c: char| c == '.' || c.is_whitespace());
        let Some(start) = change.len().checked_sub(KEEP_REST.len()) else { break };
        if !change.is_char_boundary(start) || !change[start..].eq_ignore_ascii_case(KEEP_REST) { break; }
        change = &change[..start];
    }
    let stop = if change.ends_with(['!', '?']) { "" } else { "." };
    format!("Change only: {change}{stop} {KEEP_REST}.")
}

#[cfg(test)]
mod tests {
    use super::edit_prompt;

    #[test]
    fn edit_prompt_does_not_repeat_its_closing_sentence() {
        assert_eq!(edit_prompt("Make the hat blue"), "Change only: Make the hat blue. Keep everything else exactly as it is.");
        assert_eq!(edit_prompt("Make the hat blue. "), "Change only: Make the hat blue. Keep everything else exactly as it is.");
        assert_eq!(edit_prompt("Add a crown. Keep everything else exactly as it is."), "Change only: Add a crown. Keep everything else exactly as it is.");
        assert_eq!(edit_prompt("Add a crown. keep everything else exactly as it is"), "Change only: Add a crown. Keep everything else exactly as it is.");
        assert_eq!(edit_prompt("Thinner stripe. Keep everything else the same."), "Change only: Thinner stripe. Keep everything else the same. Keep everything else exactly as it is.");
        assert_eq!(edit_prompt("Is the nose red?"), "Change only: Is the nose red? Keep everything else exactly as it is.");
        assert_eq!(edit_prompt("Ändere die Farbe…"), "Change only: Ändere die Farbe…. Keep everything else exactly as it is.");
    }
}
