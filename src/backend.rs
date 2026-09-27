//! Two routes to the same Codex image backend:
//! - direct (default): POST /images/generations or /images/edits, as the Codex CLI does.
//!   The prompt goes to the image model verbatim.
//! - responses: a Responses call where a routing model invokes the image_generation tool.
//!   Ported from pi-codex-image-gen (Apache-2.0); kept as a fallback.
use crate::auth::Credentials;
use crate::error::{Error, Kind, Result, RENEW_HINT};
use crate::images::{self, Format, InputImage};
use crate::util;
use serde_json::{json, Map, Value};
use std::io::{BufRead, BufReader, Read};
use std::time::{Duration, Instant};

pub const DEFAULT_ROUTING_MODEL: &str = "gpt-5.5";
/// What the Codex CLI requests; the backend decides which model actually serves it.
pub const IMAGE_MODEL: &str = "gpt-image-2";
const DEFAULT_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MAX_RETRIES: u32 = 3;
const BASE_DELAY_MS: f64 = 1000.0;
const MAX_RETRY_DELAY_MS: f64 = 30_000.0;
const MAX_RESPONSE_BYTES: u64 = 100 * 1024 * 1024;
const MAX_ERROR_BYTES: u64 = 16 * 1024;
const MAX_TEXT_CHARS: usize = 4000;
const QUOTA_CODES: [&str; 8] = [
    "insufficient_quota",
    "quota_exceeded",
    "usage_limit_reached",
    "usage_limit_exceeded",
    "billing_hard_limit_reached",
    "billing_not_active",
    "organization_usage_limit_exceeded",
    "workspace_member_usage_limit_reached",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Direct,
    Responses,
}

impl Transport {
    pub fn name(self) -> &'static str {
        match self {
            Transport::Direct => "direct",
            Transport::Responses => "responses",
        }
    }
}

pub struct Request<'a> {
    pub prompt: &'a str,
    pub transport: Transport,
    /// Routing model, responses transport only.
    pub model: Option<&'a str>,
    /// Requested format; only the responses route can deliver non-PNG natively.
    pub output_format: Format,
    pub size: Option<&'a str>,
    pub quality: Option<&'a str>,
    pub background: Option<&'a str>,
    pub input_images: &'a [InputImage],
    pub session_id: &'a str,
}

/// Backend-reported metadata, not independently verified.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reported {
    pub model: Option<String>,
    pub size: Option<String>,
    pub quality: Option<String>,
    pub background: Option<String>,
    pub output_format: Option<String>,
}

#[derive(Debug)]
pub struct Generated {
    pub bytes: Vec<u8>,
    /// Format of `bytes` as returned by the backend; may differ from the requested one.
    pub format: Format,
    pub transport: Transport,
    pub id: String,
    pub reported: Reported,
    pub routing_model: Option<String>,
    pub revised_prompt: Option<String>,
    pub response_id: Option<String>,
    pub usage: Option<Value>,
    pub duration: Duration,
}

struct RouteResult {
    result: String,
    id: String,
    reported: Reported,
    routing_model: Option<String>,
    revised_prompt: Option<String>,
    response_id: Option<String>,
    usage: Option<Value>,
}

pub struct Backend {
    base_url: String,
    agent: ureq::Agent,
}

impl Default for Backend {
    fn default() -> Self {
        Self::new(DEFAULT_BASE_URL)
    }
}

impl Backend {
    pub fn new(base_url: &str) -> Self {
        let agent = ureq::AgentBuilder::new().timeout(REQUEST_TIMEOUT).redirects(0).user_agent("codex-img").build();
        Self { base_url: base_url.trim_end_matches('/').to_string(), agent }
    }

    pub fn generate(&self, req: &Request, creds: &Credentials, progress: &dyn Fn(&str)) -> Result<Generated> {
        let started = Instant::now();
        let route = match req.transport {
            Transport::Direct => self.request_direct(req, creds, progress)?,
            Transport::Responses => self.request_responses(req, creds, progress)?,
        };
        // Never throw away a generated image over a format mismatch: quota is already spent.
        let (bytes, format) = images::decode_image_data(&route.result)?;
        Ok(Generated {
            bytes,
            format,
            transport: req.transport,
            id: route.id,
            reported: route.reported,
            routing_model: route.routing_model,
            revised_prompt: route.revised_prompt,
            response_id: route.response_id,
            usage: route.usage,
            duration: started.elapsed(),
        })
    }

    /// POST with bounded retries on 429/5xx.
    fn post(&self, path: &str, body: &Value, accept: &str, creds: &Credentials) -> Result<ureq::Response> {
        let url = format!("{}/{path}", self.base_url);
        let body = body.to_string();
        let mut attempt = 1;
        loop {
            let result = self
                .agent
                .post(&url)
                .set("Authorization", &format!("Bearer {}", creds.access_token))
                .set("chatgpt-account-id", &creds.account_id)
                .set("originator", "codex_cli_rs")
                .set("OpenAI-Beta", "responses=experimental")
                .set("accept", accept)
                .set("content-type", "application/json")
                .send_string(&body);
            match result {
                Ok(response) => return Ok(response),
                Err(ureq::Error::Status(status, response)) => {
                    let retry_after = response.header("retry-after").map(str::to_string);
                    let failure = http_failure(status, response);
                    if attempt <= MAX_RETRIES && failure.retry {
                        std::thread::sleep(retry_delay(attempt, retry_after.as_deref()));
                        attempt += 1;
                        continue;
                    }
                    return Err(Error::new(failure.kind, failure.message));
                }
                Err(ureq::Error::Transport(transport)) => {
                    return Err(Error::other(format!(
                        "Codex connection failed ({transport}). No automatic retry was made; check connectivity before trying again."
                    )));
                }
            }
        }
    }

    fn request_direct(&self, req: &Request, creds: &Credentials, progress: &dyn Fn(&str)) -> Result<RouteResult> {
        let edit = !req.input_images.is_empty();
        progress(if edit { "editing" } else { "generating" });
        let path = if edit { "images/edits" } else { "images/generations" };
        let response = self.post(path, &build_direct_body(req), "application/json", creds)?;
        let raw = read_body(response)?;
        debug_dump(&raw);
        let json: Value = serde_json::from_str(&raw)
            .map_err(|_| Error::other("Codex returned an invalid JSON response. No automatic retry was made."))?;
        let first = &json["data"][0];
        let result = first["b64_json"].as_str().filter(|s| !s.is_empty()).ok_or_else(|| Error::other("Codex did not return an image."))?;
        let secrets = [creds.access_token.as_str(), creds.account_id.as_str()];
        progress("completed");
        Ok(RouteResult {
            result: result.to_string(),
            id: identifier(&first["generation_id"], &secrets).unwrap_or_else(util::random_id),
            reported: reported_image(&json, &secrets),
            routing_model: None,
            revised_prompt: None,
            response_id: None,
            usage: sanitize_usage(&json["usage"]),
        })
    }

    fn request_responses(&self, req: &Request, creds: &Credentials, progress: &dyn Fn(&str)) -> Result<RouteResult> {
        let model = req.model.unwrap_or(DEFAULT_ROUTING_MODEL);
        if model.starts_with("gpt-image-") {
            return Err(Error::usage(
                "--model selects the Codex routing model (e.g. gpt-5.5), not the image model; the backend picks the image model.",
            ));
        }
        let body = build_responses_body(req, model);
        let response = self.post("responses", &body, "text/event-stream", creds)?;
        let secrets = [creds.access_token.as_str(), creds.account_id.as_str()];
        let reader = BufReader::new(response.into_reader().take(MAX_RESPONSE_BYTES));
        let parsed = parse_sse(reader, &secrets, progress)?;
        let Some(image) = parsed.image else {
            let text = parsed.text.trim();
            return Err(Error::other(if text.is_empty() {
                "Codex did not return an image.".to_string()
            } else {
                format!("Codex did not return an image. Response text: {text}")
            }));
        };
        Ok(RouteResult {
            result: image.result,
            id: image.id,
            reported: image.reported,
            routing_model: Some(model.to_string()),
            revised_prompt: image.revised_prompt,
            response_id: parsed.response_id,
            usage: parsed.usage,
        })
    }
}

pub fn build_direct_body(req: &Request) -> Value {
    // The images endpoint ignores output_format and always returns PNG; the CLI converts afterwards.
    let mut body = Map::new();
    body.insert("prompt".into(), json!(req.prompt));
    body.insert("model".into(), json!(IMAGE_MODEL));
    for (key, value) in [("size", req.size), ("quality", req.quality), ("background", req.background)] {
        if let Some(value) = value {
            body.insert(key.into(), json!(value));
        }
    }
    if !req.input_images.is_empty() {
        let images: Vec<Value> = req.input_images.iter().map(|image| json!({ "image_url": image.data_url() })).collect();
        body.insert("images".into(), Value::Array(images));
    }
    Value::Object(body)
}

pub fn build_responses_body(req: &Request, model: &str) -> Value {
    let mut tool = Map::new();
    tool.insert("type".into(), json!("image_generation"));
    tool.insert("output_format".into(), json!(req.output_format.name()));
    for (key, value) in [("size", req.size), ("quality", req.quality), ("background", req.background)] {
        if let Some(value) = value {
            tool.insert(key.into(), json!(value));
        }
    }
    let mut content = vec![json!({ "type": "input_text", "text": req.prompt })];
    content.extend(req.input_images.iter().map(|image| json!({ "type": "input_image", "image_url": image.data_url() })));
    json!({
        "model": model,
        "store": false,
        "stream": true,
        "prompt_cache_key": req.session_id,
        "instructions": "You are generating bitmap image assets. For this request, call the image_generation tool exactly once. Do not answer with only text unless image generation is unavailable.",
        "input": [{ "role": "user", "content": content }],
        "tools": [Value::Object(tool)],
        "tool_choice": "auto",
        "parallel_tool_calls": false,
        "text": { "verbosity": "low" },
    })
}

fn read_body(response: ureq::Response) -> Result<String> {
    let mut raw = String::new();
    response.into_reader().take(MAX_RESPONSE_BYTES + 1).read_to_string(&mut raw).map_err(|_| {
        Error::other("Codex response stream was interrupted. The backend may still finish; no automatic retry was made.")
    })?;
    if raw.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(Error::other("Codex response exceeded the size limit."));
    }
    Ok(raw)
}

/// Debug aid (CODEX_IMG_DEBUG_RAW=<path>): save the raw response body.
fn debug_dump(raw: &str) {
    if let Some(path) = std::env::var_os("CODEX_IMG_DEBUG_RAW").filter(|p| !p.is_empty()) {
        let _ = std::fs::write(path, raw);
    }
}

// --- Errors and retries ---

struct Failure {
    message: String,
    retry: bool,
    kind: Kind,
}

fn is_quota(error: &Value) -> bool {
    [&error["code"], &error["type"]].iter().any(|v| v.as_str().is_some_and(|s| QUOTA_CODES.contains(&s)))
}

fn is_moderation(error: &Value) -> bool {
    error["code"] == "moderation_blocked" || error["type"] == "image_generation_user_error"
}

fn error_kind(error: &Value) -> Kind {
    if is_quota(error) {
        Kind::Quota
    } else if is_moderation(error) {
        Kind::Moderation
    } else {
        Kind::Other
    }
}

fn error_hint(error: &Value) -> &'static str {
    match error_kind(error) {
        Kind::Quota => "Codex subscription quota is unavailable or exhausted. Check your plan or wait for its reset.",
        Kind::Moderation => "Codex could not generate this image. Review the prompt and input images before trying again.",
        _ => "Codex could not complete the image request.",
    }
}

fn http_failure(status: u16, response: ureq::Response) -> Failure {
    if response.header("cf-mitigated") == Some("challenge") {
        return Failure {
            message: "Codex connection was challenged by Cloudflare. This does not establish model or subscription availability.".into(),
            retry: false,
            kind: Kind::Other,
        };
    }
    let mut body = String::new();
    // A large or unreadable error body is deliberately not exposed.
    let _ = response.into_reader().take(MAX_ERROR_BYTES).read_to_string(&mut body);
    let error = serde_json::from_str::<Value>(&body).map(|v| v["error"].clone()).unwrap_or(Value::Null);
    let terminal = is_quota(&error) || is_moderation(&error);
    let hint = match status {
        401 => format!("Codex login was rejected or has expired. {RENEW_HINT}"),
        403 => "Codex access was denied. This can be a connection or account restriction; it does not identify the image model.".into(),
        _ => error_hint(&error).into(),
    };
    Failure {
        message: format!("Codex image request failed (HTTP {status}). {hint}"),
        retry: !terminal && matches!(status, 429 | 500 | 502 | 503 | 504),
        kind: if status == 401 { Kind::Auth } else { error_kind(&error) },
    }
}

fn retry_delay(attempt: u32, retry_after: Option<&str>) -> Duration {
    let server = retry_after.and_then(|v| v.trim().parse::<f64>().ok()).filter(|s| s.is_finite() && *s >= 0.0);
    let ms = match server {
        Some(secs) => (secs * 1000.0).min(MAX_RETRY_DELAY_MS) * (1.0 + util::random_unit() * 0.1),
        None => (BASE_DELAY_MS * 2f64.powi(attempt as i32 - 1)).min(MAX_RETRY_DELAY_MS) * (0.9 + util::random_unit() * 0.2),
    };
    Duration::from_millis(ms.min(MAX_RETRY_DELAY_MS) as u64)
}

// --- Sanitizers: never pass arbitrary backend strings or objects through unchecked ---

fn is_identifier(value: &str) -> bool {
    (1..=128).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn identifier(value: &Value, secrets: &[&str]) -> Option<String> {
    value
        .as_str()
        .filter(|v| is_identifier(v) && !secrets.iter().any(|s| !s.is_empty() && v.contains(s)))
        .map(str::to_string)
}

/// Redact secrets and JWT-like strings, strip control characters, and bound the length.
fn clean_text(value: &str, secrets: &[&str]) -> String {
    let mut text = value.to_string();
    for secret in secrets.iter().filter(|s| !s.is_empty()) {
        text = text.replace(secret, "[redacted]");
    }
    // A JWT cut short by a length bound can't be matched exactly, so hide anything JWT-shaped.
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(pos) = rest.find("eyJ") {
        let boundary = rest[..pos].chars().next_back().is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_'));
        let end = pos + rest[pos..].find(|c: char| !(c.is_ascii_alphanumeric() || "_.-".contains(c))).unwrap_or(rest.len() - pos);
        out.push_str(&rest[..pos]);
        out.push_str(if boundary { "[redacted]" } else { &rest[pos..end] });
        rest = &rest[end..];
    }
    out.push_str(rest);
    out.chars().map(|c| if c.is_control() { ' ' } else { c }).take(MAX_TEXT_CHARS).collect()
}

fn is_size(value: &str) -> bool {
    let valid = |part: &str| (1..=5).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_digit()) && !part.starts_with('0');
    value.split_once('x').is_some_and(|(w, h)| valid(w) && valid(h))
}

fn is_image_model(value: &str) -> bool {
    value.strip_prefix("gpt-image-").is_some_and(|rest| {
        (1..=80).contains(&rest.len()) && rest.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
    })
}

pub fn reported_image(item: &Value, secrets: &[&str]) -> Reported {
    let pick = |key: &str, valid: &dyn Fn(&str) -> bool| item[key].as_str().filter(|v| valid(v)).map(str::to_string);
    Reported {
        model: pick("model", &|v| is_image_model(v) && !secrets.iter().any(|s| !s.is_empty() && v.contains(s))),
        size: pick("size", &is_size),
        quality: pick("quality", &|v| ["low", "medium", "high", "xhigh", "max", "auto"].contains(&v)),
        background: pick("background", &|v| ["transparent", "opaque", "auto"].contains(&v)),
        output_format: pick("output_format", &|v| ["png", "jpeg", "webp"].contains(&v)),
    }
}

/// Keep only non-negative numeric usage counters; never persist arbitrary response objects.
pub fn sanitize_usage(usage: &Value) -> Option<Value> {
    let counter = |v: &Value| v.as_f64().filter(|n| n.is_finite() && *n >= 0.0).map(|_| v.clone());
    let mut out = Map::new();
    for key in ["input_tokens", "output_tokens", "total_tokens"] {
        if let Some(v) = counter(&usage[key]) {
            out.insert(key.into(), v);
        }
    }
    for (key, fields) in [
        ("input_tokens_details", ["cached_tokens", "image_tokens", "text_tokens"]),
        ("output_tokens_details", ["reasoning_tokens", "image_tokens", "text_tokens"]),
    ] {
        let details: Map<String, Value> =
            fields.iter().filter_map(|f| counter(&usage[key][*f]).map(|v| (f.to_string(), v))).collect();
        if !details.is_empty() {
            out.insert(key.into(), Value::Object(details));
        }
    }
    (!out.is_empty()).then_some(Value::Object(out))
}

// --- Responses SSE stream ---

pub struct SseImage {
    pub id: String,
    pub result: String,
    pub revised_prompt: Option<String>,
    pub reported: Reported,
}

#[derive(Default)]
pub struct ParsedSse {
    pub image: Option<SseImage>,
    pub text: String,
    pub response_id: Option<String>,
    pub usage: Option<Value>,
}

fn take_image(item: &Value, parsed: &mut ParsedSse, secrets: &[&str]) -> Result<()> {
    if item["type"] != "image_generation_call" {
        return Ok(());
    }
    if parsed.image.is_some() {
        return Err(Error::other("Codex returned more than one image. No automatic retry was made."));
    }
    if item["status"] != "completed" {
        return Err(Error::other("Codex image generation did not complete."));
    }
    let result = item["result"].as_str().filter(|r| !r.is_empty()).ok_or_else(|| Error::other("Codex image_generation_call did not contain image data."))?;
    if result.len() > images::MAX_IMAGE_BYTES.div_ceil(3) * 4 {
        return Err(Error::other("Codex image exceeded the 32 MiB size limit."));
    }
    parsed.image = Some(SseImage {
        id: identifier(&item["id"], secrets).unwrap_or_else(|| "image_generation".into()),
        result: result.to_string(),
        revised_prompt: item["revised_prompt"].as_str().map(|t| clean_text(t, secrets)).filter(|t| !t.is_empty()),
        reported: reported_image(item, secrets),
    });
    Ok(())
}

/// Handle one SSE event; returns true once the response is complete.
fn handle_event(data: &str, parsed: &mut ParsedSse, last_stage: &mut String, secrets: &[&str], progress: &dyn Fn(&str)) -> Result<bool> {
    if data.is_empty() || data == "[DONE]" {
        return Ok(false);
    }
    let event: Value =
        serde_json::from_str(data).map_err(|_| Error::other("Codex returned an invalid stream event. No automatic retry was made."))?;
    let kind = event["type"].as_str().unwrap_or_default();
    match kind {
        "error" | "response.failed" => {
            let error = [&event["response"]["error"], &event["error"]].into_iter().find(|v| !v.is_null()).unwrap_or(&event);
            return Err(Error::new(error_kind(error), error_hint(error)));
        }
        "response.incomplete" => return Err(Error::other("Codex response was incomplete. No automatic retry was made.")),
        "response.created" => parsed.response_id = identifier(&event["response"]["id"], secrets),
        "response.output_text.delta" => {
            if let Some(delta) = event["delta"].as_str() {
                let room = MAX_TEXT_CHARS.saturating_sub(parsed.text.chars().count());
                parsed.text.extend(delta.chars().take(room));
            }
        }
        "response.output_item.done" => take_image(&event["item"], parsed, secrets)?,
        "response.completed" => {
            let done = &event["response"];
            parsed.response_id = identifier(&done["id"], secrets).or(parsed.response_id.take());
            parsed.usage = sanitize_usage(&done["usage"]);
            if parsed.image.is_none() {
                for item in done["output"].as_array().into_iter().flatten() {
                    take_image(item, parsed, secrets)?;
                }
            }
            // The image item omits the model; the echoed tool config carries it.
            if let Some(image) = parsed.image.as_mut().filter(|i| i.reported.model.is_none()) {
                if let Some(tool) = done["tools"].as_array().into_iter().flatten().find(|t| t["type"] == "image_generation") {
                    image.reported.model = reported_image(tool, secrets).model;
                }
            }
            return Ok(true);
        }
        "response.image_generation_call.in_progress" | "response.image_generation_call.generating" | "response.image_generation_call.completed"
            if kind != last_stage =>
        {
            *last_stage = kind.to_string();
            progress(kind.rsplit('.').next().unwrap_or(kind));
        }
        _ => {}
    }
    Ok(false)
}

pub fn parse_sse(reader: impl BufRead, secrets: &[&str], progress: &dyn Fn(&str)) -> Result<ParsedSse> {
    let mut parsed = ParsedSse::default();
    let mut last_stage = String::new();
    let mut data = String::new();
    let mut raw = std::env::var_os("CODEX_IMG_DEBUG_RAW").map(|_| String::new());
    let mut completed = false;
    for line in reader.lines() {
        let line = line.map_err(|_| {
            Error::other("Codex response stream was interrupted. The backend may still finish; no automatic retry was made.")
        })?;
        if let Some(raw) = raw.as_mut() {
            raw.push_str(&line);
            raw.push('\n');
        }
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            completed = handle_event(data.trim(), &mut parsed, &mut last_stage, secrets, progress)?;
            data.clear();
            if completed {
                break;
            }
        } else if let Some(value) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(value.trim());
        }
    }
    if !completed && !data.trim().is_empty() {
        completed = handle_event(data.trim(), &mut parsed, &mut last_stage, secrets, progress)?;
    }
    if let Some(raw) = raw {
        debug_dump(&raw);
    }
    if !completed {
        return Err(Error::other("Codex stream ended before completion. The backend may still finish; no automatic retry was made."));
    }
    parsed.text = clean_text(&parsed.text, secrets);
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::images::tests::PNG_B64;
    use std::io::Write;
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    fn sse(events: &[Value]) -> String {
        events.iter().map(|e| format!("data: {e}\n\n")).collect()
    }

    fn image_events() -> Vec<Value> {
        vec![
            json!({"type": "response.created", "response": {"id": "resp_1"}}),
            json!({"type": "response.image_generation_call.generating"}),
            json!({"type": "response.output_item.done", "item": {
                "type": "image_generation_call", "id": "ig_1", "status": "completed", "result": PNG_B64,
                "revised_prompt": "a fox", "size": "1024x1024", "quality": "high"}}),
            json!({"type": "response.completed", "response": {"id": "resp_1",
                "tools": [{"type": "image_generation", "model": "gpt-image-2-codex"}],
                "usage": {"input_tokens": 10, "output_tokens": 2, "total_tokens": 12}}}),
        ]
    }

    #[test]
    fn parses_sse_image_metadata_usage_and_progress() {
        let stages = Mutex::new(Vec::new());
        let parsed = parse_sse(sse(&image_events()).as_bytes(), &[], &|s| stages.lock().unwrap().push(s.to_string())).unwrap();
        let image = parsed.image.unwrap();
        assert_eq!(image.id, "ig_1");
        assert_eq!(image.result, PNG_B64);
        assert_eq!(image.revised_prompt.as_deref(), Some("a fox"));
        assert_eq!(image.reported, Reported {
            model: Some("gpt-image-2-codex".into()), size: Some("1024x1024".into()), quality: Some("high".into()), ..Default::default()
        });
        assert_eq!(parsed.response_id.as_deref(), Some("resp_1"));
        assert_eq!(parsed.usage, Some(json!({"input_tokens": 10, "output_tokens": 2, "total_tokens": 12})));
        assert_eq!(*stages.lock().unwrap(), vec!["generating"]);
    }

    #[test]
    fn classifies_quota_failures_in_stream() {
        let failed = sse(&[json!({"type": "response.failed", "response": {"error": {"code": "usage_limit_reached"}}})]);
        assert_eq!(parse_sse(failed.as_bytes(), &[], &|_| {}).err().unwrap().kind, Kind::Quota);
    }

    #[test]
    fn rejects_stream_that_never_completes() {
        let events = image_events();
        let error = parse_sse(sse(&events[..3]).as_bytes(), &[], &|_| {}).err().unwrap();
        assert!(error.message.contains("ended before completion"));
    }

    #[test]
    fn redacts_secrets_and_jwts() {
        let text = clean_text("token abc123 and eyJhbGciOi.payload.sig here\u{7}", &["abc123"]);
        assert_eq!(text, "token [redacted] and [redacted] here ");
        assert!(clean_text("keyJson stays", &[]).contains("keyJson"));
    }

    #[test]
    fn sanitizes_reported_fields() {
        let reported = reported_image(&json!({"model": "gpt-image-2", "size": "0x5", "quality": "bogus", "background": "transparent"}), &[]);
        assert_eq!(reported, Reported { model: Some("gpt-image-2".into()), background: Some("transparent".into()), ..Default::default() });
    }

    // --- HTTP flow against a local server ---

    struct Captured {
        path: String,
        headers: Vec<(String, String)>,
        body: Value,
    }

    impl Captured {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
        }
    }

    /// Serve the given (status, content-type, body) responses in order and record each request.
    fn serve(responses: Vec<(u16, &'static str, String)>) -> (Backend, Arc<Mutex<Vec<Captured>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let captured = Arc::new(Mutex::new(Vec::new()));
        let sink = captured.clone();
        std::thread::spawn(move || {
            for (status, content_type, body) in responses {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request_line = String::new();
                reader.read_line(&mut request_line).unwrap();
                let path = request_line.split_whitespace().nth(1).unwrap_or_default().to_string();
                let mut headers = Vec::new();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let line = line.trim_end();
                    if line.is_empty() {
                        break;
                    }
                    if let Some((k, v)) = line.split_once(':') {
                        headers.push((k.trim().to_string(), v.trim().to_string()));
                    }
                }
                let length: usize = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length")).map_or(0, |(_, v)| v.parse().unwrap());
                let mut buf = vec![0; length];
                reader.read_exact(&mut buf).unwrap();
                sink.lock().unwrap().push(Captured { path, headers, body: serde_json::from_slice(&buf).unwrap_or(Value::Null) });
                let mut stream = stream;
                write!(stream, "HTTP/1.1 {status} X\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nretry-after: 0\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        (Backend::new(&base), captured)
    }

    fn creds() -> Credentials {
        Credentials { access_token: "tok_abc".into(), account_id: "acct_123".into() }
    }

    fn request<'a>(transport: Transport, images: &'a [InputImage]) -> Request<'a> {
        Request {
            prompt: "a fox",
            transport,
            model: None,
            output_format: Format::Png,
            size: Some("1536x1024"),
            quality: None,
            background: Some("transparent"),
            input_images: images,
            session_id: "session-1",
        }
    }

    fn direct_response() -> String {
        json!({"created": 1, "background": "transparent", "output_format": "png", "quality": "medium", "size": "1536x1024",
            "data": [{"b64_json": PNG_B64, "generation_id": "gen-1"}],
            "usage": {"input_tokens": 16, "output_tokens": 1372, "total_tokens": 1388, "output_tokens_details": {"image_tokens": 1372}}})
        .to_string()
    }

    #[test]
    fn direct_route_posts_prompt_verbatim_to_generations() {
        let (backend, captured) = serve(vec![(200, "application/json", direct_response())]);
        let image = backend.generate(&request(Transport::Direct, &[]), &creds(), &|_| {}).unwrap();
        let calls = captured.lock().unwrap();
        assert_eq!(calls[0].path, "/images/generations");
        assert_eq!(calls[0].header("authorization"), Some("Bearer tok_abc"));
        assert_eq!(calls[0].header("chatgpt-account-id"), Some("acct_123"));
        assert_eq!(calls[0].body, json!({"prompt": "a fox", "model": "gpt-image-2", "size": "1536x1024", "background": "transparent"}));
        assert_eq!(image.id, "gen-1");
        assert_eq!(image.format, Format::Png);
        assert_eq!(image.transport, Transport::Direct);
        assert_eq!(image.reported.size.as_deref(), Some("1536x1024"));
        assert_eq!(image.usage.unwrap()["output_tokens_details"]["image_tokens"], 1372);
        assert!(image.revised_prompt.is_none());
    }

    #[test]
    fn direct_route_sends_references_to_edits() {
        let (backend, captured) = serve(vec![(200, "application/json", direct_response())]);
        let refs = [InputImage { data_b64: PNG_B64.into(), format: Format::Png }];
        backend.generate(&request(Transport::Direct, &refs), &creds(), &|_| {}).unwrap();
        let calls = captured.lock().unwrap();
        assert_eq!(calls[0].path, "/images/edits");
        assert_eq!(calls[0].body["images"], json!([{"image_url": format!("data:image/png;base64,{PNG_B64}")}]));
    }

    #[test]
    fn direct_route_rejects_response_without_image() {
        let (backend, _) = serve(vec![(200, "application/json", json!({"created": 1, "data": []}).to_string())]);
        let error = backend.generate(&request(Transport::Direct, &[]), &creds(), &|_| {}).unwrap_err();
        assert!(error.message.contains("did not return an image"));
    }

    #[test]
    fn responses_route_streams_through_routing_model() {
        let (backend, captured) = serve(vec![(200, "text/event-stream", sse(&image_events()))]);
        let image = backend.generate(&request(Transport::Responses, &[]), &creds(), &|_| {}).unwrap();
        let calls = captured.lock().unwrap();
        assert_eq!(calls[0].path, "/responses");
        assert_eq!(calls[0].body["model"], "gpt-5.5");
        assert_eq!(calls[0].body["tools"][0]["size"], "1536x1024");
        assert_eq!(image.id, "ig_1");
        assert_eq!(image.routing_model.as_deref(), Some("gpt-5.5"));
        assert_eq!(image.revised_prompt.as_deref(), Some("a fox"));
        assert_eq!(image.reported.model.as_deref(), Some("gpt-image-2-codex"));
    }

    #[test]
    fn unauthorized_is_an_auth_error_without_retry() {
        let (backend, captured) = serve(vec![(401, "application/json", "{}".into())]);
        let error = backend.generate(&request(Transport::Direct, &[]), &creds(), &|_| {}).unwrap_err();
        assert_eq!(error.kind, Kind::Auth);
        assert!(error.message.contains("Open Codex"));
        assert_eq!(captured.lock().unwrap().len(), 1);
    }

    #[test]
    fn retries_transient_errors_but_not_quota() {
        let (backend, captured) = serve(vec![(503, "application/json", "{}".into()), (200, "application/json", direct_response())]);
        assert!(backend.generate(&request(Transport::Direct, &[]), &creds(), &|_| {}).is_ok());
        assert_eq!(captured.lock().unwrap().len(), 2);

        let quota = json!({"error": {"code": "insufficient_quota"}}).to_string();
        let (backend, captured) = serve(vec![(429, "application/json", quota)]);
        let error = backend.generate(&request(Transport::Direct, &[]), &creds(), &|_| {}).unwrap_err();
        assert_eq!(error.kind, Kind::Quota);
        assert_eq!(captured.lock().unwrap().len(), 1);
    }

    #[test]
    fn rejects_image_model_as_routing_model() {
        let backend = Backend::new("http://127.0.0.1:9");
        let mut req = request(Transport::Responses, &[]);
        req.model = Some("gpt-image-1");
        assert!(backend.generate(&req, &creds(), &|_| {}).unwrap_err().message.contains("routing model"));
    }
}
