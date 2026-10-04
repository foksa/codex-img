//! Change one member of a JSON object in place, keeping the rest of the file's text as written.
//! Hand-written specs keep their layout (one asset per line, inline arrays) when a review field
//! changes. Callers check the result parses to the value they expect and fall back otherwise.
use serde_json::Value;

/// One `"key": value` member: where its key starts and where its value starts and ends.
struct Member { key: String, key_start: usize, value_start: usize, value_end: usize }

/// Sets (`Some`) or removes (`None`) `name` in the object reached by `path` from the root object.
/// Returns `None` when the text isn't laid out as expected; the caller then rewrites the file.
pub fn set_member(text: &str, path: &[&str], name: &str, value: Option<&Value>) -> Option<String> {
    let bytes = text.as_bytes();
    let mut open = skip_ws(bytes, 0);
    for key in path {
        let members = members(bytes, open)?.1;
        let member = members.iter().rev().find(|m| m.key == *key)?;
        open = member.value_start;
    }
    let (close, members) = members(bytes, open)?;
    let existing = members.iter().rposition(|m| m.key == name);
    let rendered = value.map(serde_json::to_string).transpose().ok()?;
    let mut out = String::with_capacity(text.len() + 64);
    match (existing, rendered) {
        (Some(i), Some(rendered)) => {
            let m = &members[i];
            out.push_str(&text[..m.value_start]); out.push_str(&rendered); out.push_str(&text[m.value_end..]);
        }
        (Some(i), None) => {
            let m = &members[i];
            // Drop the member with one neighbouring comma: the one after it, or before it when it's last.
            let (start, end) = if let Some(next) = members.get(i + 1) { (m.key_start, next.key_start) }
                else if i > 0 { (members[i - 1].value_end, m.value_end) }
                else { (open + 1, close) };
            out.push_str(&text[..start]); out.push_str(&text[end..]);
        }
        (None, Some(rendered)) => {
            let key = serde_json::to_string(name).ok()?;
            let Some(last) = members.last() else {
                out.push_str(&text[..=open]); out.push_str(&format!("{key}: {rendered}")); out.push_str(&text[close..]);
                return Some(out);
            };
            // Copy the file's own spacing: what separates members, and what follows each key.
            let gap = if members.len() > 1 { &text[comma_after(bytes, members[0].value_end)? + 1..members[1].key_start] }
                else { &text[open + 1..last.key_start] };
            let colon = &text[key_end(bytes, last.key_start)?..last.value_start];
            out.push_str(&text[..last.value_end]);
            out.push_str(&format!(",{gap}{key}{colon}{rendered}"));
            out.push_str(&text[last.value_end..]);
        }
        (None, None) => return Some(text.to_string()),
    }
    Some(out)
}

fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while bytes.get(i).is_some_and(|b| b.is_ascii_whitespace()) { i += 1; }
    i
}
/// The end of the string starting at `i` (just past its closing quote).
fn string_end(bytes: &[u8], i: usize) -> Option<usize> {
    if bytes.get(i) != Some(&b'"') { return None; }
    let mut j = i + 1;
    loop {
        match *bytes.get(j)? { b'\\' => j += 2, b'"' => return Some(j + 1), _ => j += 1 }
    }
}
fn key_end(bytes: &[u8], key_start: usize) -> Option<usize> { string_end(bytes, key_start) }
/// The end of the value starting at `i`.
fn value_end(bytes: &[u8], i: usize) -> Option<usize> {
    match *bytes.get(i)? {
        b'"' => string_end(bytes, i),
        b'{' | b'[' => {
            let mut depth = 0usize;
            let mut j = i;
            loop {
                match *bytes.get(j)? {
                    b'"' => { j = string_end(bytes, j)?; continue; }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => { depth -= 1; if depth == 0 { return Some(j + 1); } }
                    _ => {}
                }
                j += 1;
            }
        }
        _ => {
            let mut j = i;
            while bytes.get(j).is_some_and(|b| !matches!(b, b',' | b'}' | b']') && !b.is_ascii_whitespace()) { j += 1; }
            (j > i).then_some(j)
        }
    }
}
fn comma_after(bytes: &[u8], i: usize) -> Option<usize> {
    let j = skip_ws(bytes, i);
    (bytes.get(j) == Some(&b',')).then_some(j)
}
/// The members of the object whose `{` is at `open`, and the position of its `}`.
fn members(bytes: &[u8], open: usize) -> Option<(usize, Vec<Member>)> {
    if bytes.get(open) != Some(&b'{') { return None; }
    let mut out = Vec::new();
    let mut i = skip_ws(bytes, open + 1);
    if bytes.get(i) == Some(&b'}') { return Some((i, out)); }
    loop {
        let key_start = i;
        let key_end = string_end(bytes, key_start)?;
        let key: String = serde_json::from_slice(&bytes[key_start..key_end]).ok()?;
        i = skip_ws(bytes, key_end);
        if bytes.get(i) != Some(&b':') { return None; }
        let value_start = skip_ws(bytes, i + 1);
        let value_end = value_end(bytes, value_start)?;
        out.push(Member { key, key_start, value_start, value_end });
        i = skip_ws(bytes, value_end);
        match *bytes.get(i)? {
            b',' => i = skip_ws(bytes, i + 1),
            b'}' => return Some((i, out)),
            _ => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::set_member;
    use serde_json::{json, Value};

    const SPEC: &str = r#"{
  "style": "pixel",
  "assets": {
    "hero":  {"prompt": "A hero, \"brave\" {x}", "max": [256, 256]},
    "cone": {"prompt": "A cone", "comment": "old"}
  }
}
"#;
    fn edit(text: &str, path: &[&str], name: &str, value: Option<Value>) -> String {
        let out = set_member(text, path, name, value.as_ref()).unwrap();
        serde_json::from_str::<Value>(&out).unwrap();
        out
    }

    #[test]
    fn adds_replaces_and_removes_a_field_without_reformatting() {
        let added = edit(SPEC, &["assets", "hero"], "comment", Some(json!("Make it blue")));
        assert_eq!(added, SPEC.replace(r#""max": [256, 256]}"#, r#""max": [256, 256], "comment": "Make it blue"}"#));
        let replaced = edit(SPEC, &["assets", "cone"], "comment", Some(json!("new")));
        assert_eq!(replaced, SPEC.replace(r#""comment": "old""#, r#""comment": "new""#));
        let removed = edit(SPEC, &["assets", "cone"], "comment", None);
        assert_eq!(removed, SPEC.replace(r#", "comment": "old""#, ""));
        assert_eq!(edit(SPEC, &["assets", "hero"], "star", None), SPEC, "removing a missing field changes nothing");
    }

    #[test]
    fn follows_the_files_own_spacing() {
        let pretty = "{\n  \"prompt\": \"A fox\",\n  \"request\": {}\n}\n";
        assert_eq!(edit(pretty, &[], "star", Some(json!(true))), "{\n  \"prompt\": \"A fox\",\n  \"request\": {},\n  \"star\": true\n}\n");
        assert_eq!(edit(pretty, &[], "prompt", None), "{\n  \"request\": {}\n}\n");
        assert_eq!(edit(pretty, &[], "request", None), "{\n  \"prompt\": \"A fox\"\n}\n");
        assert_eq!(edit(r#"{"a":1}"#, &[], "b", Some(json!(2))), r#"{"a":1,"b":2}"#);
        assert_eq!(edit(r#"{"a": {}}"#, &["a"], "b", Some(json!("x"))), r#"{"a": {"b": "x"}}"#);
        assert_eq!(edit(r#"{"a": {"b": 1}}"#, &["a"], "b", None), r#"{"a": {}}"#);
    }

    #[test]
    fn gives_up_on_text_it_does_not_understand() {
        assert!(set_member("[1, 2]", &[], "a", Some(&json!(1))).is_none());
        assert!(set_member(r#"{"assets": {}}"#, &["assets", "missing"], "a", Some(&json!(1))).is_none());
        assert!(set_member(r#"{"a": 1 // note
}"#, &[], "b", Some(&json!(1))).is_none());
    }
}
