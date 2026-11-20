//! A Claude Code session log as a read-only conversation, read only when the user opens it:
//! the last few MB of the `.jsonl` (more on request, capped), never written, never sent
//! anywhere, never logged.
use crate::agents::{ErrKind, tail_lines, turn_entry};
use serde_json::Value;
use std::path::Path;

/// Bytes read from the end of a log at first, and the most "load earlier" goes to.
pub const WINDOW: u64 = 2 * 1024 * 1024;
pub const MAX_WINDOW: u64 = 64 * 1024 * 1024;
/// Longest message kept, in characters; longer ones are cut.
const TEXT_MAX: usize = 20_000;
/// Longest tool input / result kept for the expanded view.
const DETAIL_MAX: usize = 4_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tool {
    Edited(String),
    Wrote(String),
    Read(String),
    Ran(String),
    Searched(String),
    Fetched(String),
    Web(String),
    Agent(String),
    Asked,
    Todo,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    User(String),
    Assistant(String),
    Tool { tool: Tool, id: Option<String>, input: String, result: Option<String>, failed: bool },
    Error(ErrKind),
}

impl Item {
    /// Text searched by the find field.
    pub fn haystack(&self) -> String {
        match self {
            Item::User(t) | Item::Assistant(t) => t.to_lowercase(),
            Item::Tool { input, result, .. } => format!("{input}\n{}", result.as_deref().unwrap_or_default()).to_lowercase(),
            Item::Error(_) => String::new(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Transcript {
    pub items: Vec<Item>,
    /// Where the window starts in the file: > 0 means earlier messages weren't read.
    pub from: u64,
    pub len: u64,
}

pub fn load(path: &Path, window: u64) -> Option<Transcript> {
    let (text, from, len) = tail_lines(path, window.min(MAX_WINDOW))?;
    Some(Transcript { items: parse(&text), from, len })
}

fn cut(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]), // i18n-ok: ellipsis
        None => s.to_string(),
    }
}

/// `/a/b/c/d/e.rs` → `c/d/e.rs`.
fn short(p: &str) -> String {
    let parts: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() <= 3 { p.to_string() } else { parts[parts.len() - 3..].join("/") }
}

fn first_line(s: &str, max: usize) -> String {
    let l = s.lines().find(|l| !l.trim().is_empty()).unwrap_or_default().trim();
    let more = s.trim().lines().count() > 1;
    let c = cut(l, max);
    if more && !c.ends_with('…') { format!("{c} …") } else { c } // i18n-ok: ellipsis
}

pub fn summarize(name: &str, input: &Value) -> Tool {
    let s = |k: &str| input.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();
    match name {
        "Edit" | "MultiEdit" | "NotebookEdit" => Tool::Edited(short(&s(if name == "NotebookEdit" { "notebook_path" } else { "file_path" }))),
        "Write" => Tool::Wrote(short(&s("file_path"))),
        "Read" => Tool::Read(short(&s("file_path"))),
        "Bash" => Tool::Ran(first_line(&s("command"), 80)),
        "Grep" | "Glob" => Tool::Searched(cut(&s("pattern"), 60)),
        "WebFetch" => Tool::Fetched(cut(&s("url"), 80)),
        "WebSearch" => Tool::Web(cut(&s("query"), 80)),
        "Agent" | "Task" => Tool::Agent(cut(&s("description"), 80)),
        "AskUserQuestion" => Tool::Asked,
        "TodoWrite" => Tool::Todo,
        other => Tool::Other(other.to_string()),
    }
}

fn block_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().filter_map(|b| b.get("text").and_then(|t| t.as_str())).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

/// The conversation in `text` (complete JSONL lines), oldest first.
pub fn parse(text: &str) -> Vec<Item> {
    let mut items: Vec<Item> = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let Some(ty) = turn_entry(&v) else { continue };
        let content = v.pointer("/message/content");
        if ty == "assistant" && (v.get("isApiErrorMessage").and_then(|x| x.as_bool()).unwrap_or(false) || v.get("error").is_some_and(|e| e.is_string())) {
            items.push(Item::Error(ErrKind::parse(v.get("error").and_then(|e| e.as_str()).unwrap_or_default())));
            continue;
        }
        let blocks: Vec<&Value> = match content {
            Some(Value::Array(a)) => a.iter().collect(),
            Some(Value::String(s)) if !s.trim().is_empty() => {
                let t = cut(s.trim(), TEXT_MAX);
                items.push(if ty == "user" { Item::User(t) } else { Item::Assistant(t) });
                continue;
            }
            _ => continue,
        };
        for b in blocks {
            match b.get("type").and_then(|t| t.as_str()).unwrap_or_default() {
                "text" => {
                    let t = b.get("text").and_then(|t| t.as_str()).unwrap_or_default().trim();
                    if !t.is_empty() {
                        let t = cut(t, TEXT_MAX);
                        items.push(if ty == "user" { Item::User(t) } else { Item::Assistant(t) });
                    }
                }
                "tool_use" => {
                    let name = b.get("name").and_then(|n| n.as_str()).unwrap_or_default();
                    let input = b.get("input").cloned().unwrap_or(Value::Null);
                    let pretty = serde_json::to_string_pretty(&input).unwrap_or_default();
                    items.push(Item::Tool { tool: summarize(name, &input), id: b.get("id").and_then(|i| i.as_str()).map(str::to_string), input: cut(&pretty, DETAIL_MAX), result: None, failed: false });
                }
                "tool_result" => {
                    let id = b.get("tool_use_id").and_then(|i| i.as_str());
                    let out = cut(block_text(b.get("content").unwrap_or(&Value::Null)).trim(), DETAIL_MAX);
                    let err = b.get("is_error").and_then(|e| e.as_bool()).unwrap_or(false);
                    if let Some(Item::Tool { result, failed, .. }) = items.iter_mut().rev().find(|i| matches!(i, Item::Tool { id: tid, .. } if tid.as_deref() == id)) {
                        *result = Some(out);
                        *failed = err;
                    }
                }
                _ => {}
            }
        }
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = r#"{"type":"user","message":{"role":"user","content":"Please fix the **parser**"}}
{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"hidden"}]}}
{"type":"assistant","message":{"content":[{"type":"text","text":"Looking."},{"type":"tool_use","id":"a","name":"Edit","input":{"file_path":"/home/u/p/src/parse/mod.rs","old_string":"x","new_string":"y"}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"a","content":"ok"}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"b","name":"Bash","input":{"command":"cargo test\necho done"}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"b","is_error":true,"content":[{"type":"text","text":"1 failed"}]}]}}
{"type":"user","isSidechain":true,"message":{"content":"subagent prompt"}}
{"type":"user","isMeta":true,"message":{"content":"caveat"}}
{"type":"attachment","attachment":{"type":"date"}}
{"type":"assistant","isApiErrorMessage":true,"error":"overloaded_error","message":{"content":[{"type":"text","text":"API Error"}]}}
{"type":"assistant","message":{"content":[{"type":"text","text":"Fixed it. Anything else?"}]}}
not json
"#;

    #[test]
    fn parses_turns_and_tools() {
        let items = parse(LOG);
        assert_eq!(items.len(), 6, "{items:#?}");
        assert_eq!(items[0], Item::User("Please fix the **parser**".into()));
        assert_eq!(items[1], Item::Assistant("Looking.".into()));
        match &items[2] {
            Item::Tool { tool, result, failed, input, .. } => {
                assert_eq!(tool, &Tool::Edited("src/parse/mod.rs".into()));
                assert_eq!((result.as_deref(), *failed), (Some("ok"), false));
                assert!(input.contains("old_string"));
            }
            other => panic!("{other:?}"),
        }
        match &items[3] {
            Item::Tool { tool, result, failed, .. } => {
                assert_eq!(tool, &Tool::Ran("cargo test …".into()));
                assert_eq!((result.as_deref(), *failed), (Some("1 failed"), true));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(items[4], Item::Error(ErrKind::Overloaded));
        assert_eq!(items[5], Item::Assistant("Fixed it. Anything else?".into()));
        assert!(items[3].haystack().contains("cargo test"));
    }

    #[test]
    fn summaries() {
        let j = |s: &str| serde_json::from_str::<Value>(s).unwrap();
        assert_eq!(summarize("Write", &j(r#"{"file_path":"a.rs"}"#)), Tool::Wrote("a.rs".into()));
        assert_eq!(summarize("Grep", &j(r#"{"pattern":"fn main"}"#)), Tool::Searched("fn main".into()));
        assert_eq!(summarize("AskUserQuestion", &j("{}")), Tool::Asked);
        assert_eq!(summarize("mcp__x__y", &j("{}")), Tool::Other("mcp__x__y".into()));
        assert_eq!(cut("abcdef", 3), "abc…");
    }

    #[test]
    fn loads_the_tail() {
        let dir = std::env::temp_dir().join(format!("rama-transcript-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("s.jsonl");
        let line = |i: usize| format!(r#"{{"type":"user","message":{{"content":"message {i}"}}}}"#);
        let text: String = (0..2000).map(|i| line(i) + "\n").collect();
        std::fs::write(&f, &text).unwrap();
        let t = load(&f, 4096).unwrap();
        assert!(t.from > 0 && t.len == text.len() as u64);
        assert_eq!(t.items.last(), Some(&Item::User("message 1999".into())));
        // the window starts mid-line: that line is dropped, the rest is whole
        assert!(t.items.iter().all(|i| matches!(i, Item::User(s) if s.starts_with("message "))));
        assert!(t.items.len() < 2000);
        let all = load(&f, MAX_WINDOW).unwrap();
        assert_eq!((all.from, all.items.len()), (0, 2000));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
