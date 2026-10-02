//! coucou-hook — the relay Claude Code runs on every hook event.
//!
//! Reads the hook JSON on stdin, adds a little terminal context, and hands it to
//! Coucou over the named pipe `\\.\pipe\coucou-<sid>`.
//!
//! Hard rule (docs/CLAUDE.md): **never block Claude Code.**
//! * If the pipe does not exist — Coucou is closed — we exit 0 immediately with
//!   nothing on stdout, and the session carries on untouched.
//! * Every step runs under a deadline enforced by the main thread, so a pipe that
//!   accepts the connection and then stops reading cannot wedge the session
//!   either: we abandon the worker and exit.
//! * Only `PermissionRequest` waits for an answer, because approving from the
//!   island is the whole point. No answer means empty stdout, and Claude Code
//!   asks in the terminal exactly as if Coucou were not installed.
//!
//! Usage: `coucou-hook <EventName> [--agent claude|codex]` (the event name is also
//! read from the JSON; the agent defaults to `claude`). The agent travels in the
//! payload as `agent`, so the island knows whose session an event belongs to.

use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Budget for getting a pipe connection. Beyond this Claude Code wins, always.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(300);
/// Whole-run budget for an event nobody waits on: connect and write, no more.
const FIRE_AND_FORGET_BUDGET: Duration = Duration::from_secs(2);
/// How long a permission prompt may stay on screen before the terminal takes over.
const DECISION_BUDGET: Duration = Duration::from_secs(110);

/// `ERROR_PIPE_BUSY` — every instance is serving someone else right now. This is
/// the one error worth retrying: the server exists and a slot will free up.
const ERROR_PIPE_BUSY: i32 = 231;

/// Fields that are pointless to forward and can be enormous (a whole file read,
/// a full command output). The island never shows them.
const DROPPED_FIELDS: &[&str] = &["tool_response", "transcript_path"];
/// Longest string forwarded for any single field; the island truncates to far
/// less than this anyway.
const MAX_FIELD_LEN: usize = 2_000;

mod win;

/// `\\.\pipe\coucou-<sid>`. The SID keeps two accounts on the same machine from
/// ever meeting on the same pipe; the name falls back to the user name only if
/// the SID cannot be read at all, which should not happen.
fn pipe_path() -> String {
    let key = win::current_user_sid()
        .unwrap_or_else(|| std::env::var("USERNAME").unwrap_or_else(|_| "user".into()));
    format!(r"\\.\pipe\coucou-{key}")
}

/// Opens the pipe. Retries only while the server is busy: any other error means
/// there is nothing to talk to, and waiting would only delay Claude Code.
fn connect() -> Option<std::fs::File> {
    use std::os::windows::io::AsRawHandle;
    let path = pipe_path();
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    loop {
        match std::fs::OpenOptions::new().read(true).write(true).open(&path) {
            Ok(file) => {
                let handle = windows::Win32::Foundation::HANDLE(file.as_raw_handle());
                // Somebody else's server on our pipe name gets nothing from us.
                return win::pipe_server_is_same_user(handle).then_some(file);
            }
            Err(err) => {
                if err.raw_os_error() != Some(ERROR_PIPE_BUSY) || Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(15));
            }
        }
    }
}

fn main() {
    let Some((payload, event, agent)) = read_event() else {
        // agy reads every hook's stdout as JSON and a hook that fails blocks its tool,
        // so even a payload we cannot parse gets the answer that changes nothing.
        if parse_args(std::env::args().skip(1)).1 == "antigravity" {
            println!("{AGY_ASK}");
        }
        std::process::exit(0)
    };

    let waits_for_answer = event == "PermissionRequest";
    let command = serde_json::from_str::<serde_json::Value>(&payload)
        .ok()
        .and_then(|v| v["tool_input"]["command"].as_str().map(str::to_string))
        .unwrap_or_default();
    let budget = if waits_for_answer { DECISION_BUDGET } else { FIRE_AND_FORGET_BUDGET };

    // The worker owns every blocking call. If it overruns the budget we simply
    // stop listening and exit: the process dying takes the pipe handle with it.
    // (No catch_unwind here — the release profile is panic = "abort", so it would
    // be dead code. `talk` is written to have nothing to panic on instead.)
    let (tx, rx) = mpsc::channel::<Option<String>>();
    std::thread::spawn(move || {
        let _ = tx.send(talk(&payload, waits_for_answer));
    });

    let decided = match rx.recv_timeout(budget) {
        Ok(Some(decision)) => reply_for(&agent, &decision, &command),
        _ => None,
    };
    if let Some(json) = decided.or_else(|| silent_reply(&agent, &event)) {
        let mut out = std::io::stdout();
        let _ = writeln!(out, "{json}");
        let _ = out.flush();
    }
    // Nothing printed: Claude Code asks in the terminal, as if we were not here.
    std::process::exit(0);
}

/// What an event with nothing to decide still has to print. Codex rejects a
/// `Stop` hook that exits 0 without JSON ("plain text output is invalid for this
/// event"), so it gets an empty object. Everything else stays silent.
/// See https://developers.openai.com/codex/hooks
///
/// Antigravity (`agy`) is the strictest: every hook must print JSON, and a hook
/// that fails *blocks the tool*. A tool check we have nothing to say about gets
/// `ask` (= "carry on as if you had no hook": its own allow-list, then its own
/// prompt); every other event gets `{}`.
fn silent_reply(agent: &str, event: &str) -> Option<String> {
    match (agent, event) {
        ("codex", "Stop") => Some("{}".to_string()),
        ("antigravity", "PreToolUse" | "PermissionRequest") => Some(AGY_ASK.to_string()),
        ("antigravity", _) => Some("{}".to_string()),
        _ => None,
    }
}

/// Antigravity's "no opinion": its own permission rules and prompt decide.
const AGY_ASK: &str = r#"{"decision":"ask"}"#;

/// What goes back to the agent for a click on the island.
fn reply_for(agent: &str, decision: &str, command: &str) -> Option<String> {
    if agent == "antigravity" {
        agy_decision_json(decision, command)
    } else {
        decision_json(decision)
    }
}

/// Antigravity's PreToolUse answer: `allow` runs the tool without asking, `deny`
/// blocks it. Anything we do not recognise says nothing (the caller then falls
/// back to `ask`), never a guess.
/// See ~/.gemini/antigravity-cli/builtin/skills/agy-customizations/docs/hooks.md
fn agy_decision_json(decision: &str, command: &str) -> Option<String> {
    match decision.trim() {
        // A bare `allow` was not enough for agy to skip its own prompt; the same
        // command also goes in as a one-off permission grant.
        "allow" | "always" if !command.is_empty() => Some(
            serde_json::json!({
                "decision": "allow",
                "permissionOverrides": [format!("command({command})")]
            })
            .to_string(),
        ),
        "allow" | "always" => Some(r#"{"decision":"allow"}"#.to_string()),
        "deny" => Some(r#"{"decision":"deny","reason":"Denied from Coucou"}"#.to_string()),
        _ => None,
    }
}

/// `<Event> [--agent <name>]` from argv (program name already skipped).
/// The event is the first argument that is not a flag; the agent is sanitised so
/// a stray value can never become anything but a short lowercase word.
fn parse_args(args: impl Iterator<Item = String>) -> (String, String) {
    let mut event = String::new();
    let mut agent = String::new();
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        if arg == "--agent" {
            agent = args.next().unwrap_or_default();
        } else if !arg.starts_with("--") && event.is_empty() {
            event = arg;
        }
    }
    let agent: String = agent
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(24)
        .collect();
    (event, if agent.is_empty() { "claude".into() } else { agent })
}

/// Gemini CLI names its events differently; the island only speaks Claude's.
/// Gemini has no PermissionRequest, so nothing here ever waits for a human.
/// See https://geminicli.com/docs/hooks/reference/
fn canonical_event(agent: &str, event: &str) -> String {
    match (agent, event) {
        ("gemini", "BeforeTool") => "PreToolUse",
        ("gemini", "AfterTool") => "PostToolUse",
        ("gemini", "BeforeAgent") => "UserPromptSubmit",
        ("gemini", "AfterAgent") => "Stop",
        // Antigravity: a model call starting is the nearest thing to "prompt
        // submitted", and the end of one is just more work happening.
        ("antigravity", "PreInvocation") => "UserPromptSubmit",
        ("antigravity", "PostInvocation") => "PostToolUse",
        (_, other) => other,
    }
    .to_string()
}

/// Tools whose use Antigravity asks the user about. Others (reading files, viewing
/// a page…) are only watched. `run_command` is the one that matters.
const AGY_GATED_TOOLS: &[&str] = &["run_command"];

type JsonMap = serde_json::Map<String, serde_json::Value>;

/// Antigravity speaks camelCase and nests the tool call; the island speaks Claude's
/// flat snake_case. Translate the fields it reads, and keep everything else.
fn agy_normalize(map: &mut JsonMap) {
    use serde_json::Value;
    let text = |v: Option<&Value>| v.and_then(Value::as_str).map(str::to_string);

    if let Some(id) = text(map.get("conversationId")) {
        map.insert("session_id".into(), Value::String(id));
    }
    // The workspace is both where the session is and what it is called.
    if let Some(ws) = map
        .get("workspacePaths")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_str)
        .map(str::to_string)
    {
        map.entry("cwd").or_insert_with(|| Value::String(ws.clone()));
        map.entry("project_dir").or_insert(Value::String(ws));
    }
    if let Some(call) = map.get("toolCall").and_then(Value::as_object).cloned() {
        if let Some(name) = text(call.get("name")) {
            map.insert("tool_name".into(), Value::String(name));
        }
        let mut args = call.get("args").and_then(Value::as_object).cloned().unwrap_or_default();
        // The names the island already knows how to show.
        for (from, to) in [
            ("CommandLine", "command"),
            ("AbsolutePath", "file_path"),
            ("TargetFile", "file_path"),
            ("DirectoryPath", "path"),
            ("SearchPath", "path"),
            ("Query", "query"),
            ("Url", "url"),
        ] {
            if let Some(v) = args.get(from).cloned() {
                args.entry(to).or_insert(v);
            }
        }
        map.insert("tool_input".into(), Value::Object(args));
    }
}

/// `command(<exact command line>)` is how Antigravity stores a command you have
/// told it to always allow. An exact match is the only thing we trust.
fn is_allow_listed(rules: &[String], tool: &str, command: &str) -> bool {
    let kind = if tool == "run_command" { "command" } else { tool };
    let want = format!("{kind}({command})");
    rules.iter().any(|r| *r == want)
}

/// `permissions.allow` from agy's own settings; empty if it cannot be read, which
/// only means more cards, never fewer.
fn agy_allow_rules() -> Vec<String> {
    let Some(home) = std::env::var_os("USERPROFILE") else { return Vec::new() };
    let path = std::path::Path::new(&home).join(".gemini/antigravity-cli/settings.json");
    let Ok(bytes) = std::fs::read(path) else { return Vec::new() };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else { return Vec::new() };
    v["permissions"]["allow"]
        .as_array()
        .map(|a| a.iter().filter_map(|r| r.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

/// Whether this (already normalised) tool check should go to the island and wait:
/// a gated tool that Antigravity would otherwise ask the user about.
fn agy_needs_island(map: &JsonMap, rules: &[String]) -> bool {
    let tool = map.get("tool_name").and_then(|v| v.as_str()).unwrap_or("");
    if !AGY_GATED_TOOLS.contains(&tool) {
        return false;
    }
    let command = map
        .get("tool_input")
        .and_then(|i| i.get("command"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    !is_allow_listed(rules, tool, command)
}

/// The documented PermissionRequest output. Anything we do not recognise prints
/// nothing at all rather than guessing — silence is the safe answer.
/// See https://code.claude.com/docs/en/hooks
fn decision_json(decision: &str) -> Option<String> {
    let behavior = match decision.trim() {
        // "always" still answers a plain allow; remembering it is the island's
        // business, not Claude Code's.
        "allow" | "always" => r#"{"behavior":"allow"}"#.to_string(),
        "deny" => r#"{"behavior":"deny","message":"Denied from Coucou"}"#.to_string(),
        _ => return None,
    };
    Some(format!(
        r#"{{"hookSpecificOutput":{{"hookEventName":"PermissionRequest","decision":{behavior}}}}}"#
    ))
}

/// Reads stdin and returns the payload to forward, the event name and the agent.
fn read_event() -> Option<(String, String, String)> {
    let mut raw = Vec::new();
    if std::io::stdin().read_to_end(&mut raw).is_err() || raw.is_empty() {
        return None;
    }
    // Some shells hand us a UTF-8 BOM; serde_json would choke on it.
    if raw.starts_with(&[0xEF, 0xBB, 0xBF]) {
        raw.drain(..3);
    }

    let mut payload = serde_json::from_slice::<serde_json::Value>(&raw).ok()?;
    let map = payload.as_object_mut()?;

    // The event name is passed as argv[1] by the hook command; the JSON usually
    // carries it too. Trust argv when the JSON is missing it.
    let (arg_event, agent) = parse_args(std::env::args().skip(1));
    // Also tag the payload the way upstream's third-party agent route expects,
    // so a bare `--agent my-tool` keeps working; claude/codex are routed by
    // the `agent` field below instead.
    map.insert("coucou_agent".into(), serde_json::Value::String(agent.clone()));
    let event = map
        .get("hook_event_name")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .unwrap_or(arg_event);
    let mut event = canonical_event(&agent, &event);
    if agent == "antigravity" {
        agy_normalize(map);
        // A command it would ask about waits for the island, like a PermissionRequest.
        if event == "PreToolUse" && agy_needs_island(map, &agy_allow_rules()) {
            event = "PermissionRequest".to_string();
        }
    }
    map.insert("hook_event_name".into(), serde_json::Value::String(event.clone()));
    map.insert("agent".into(), serde_json::Value::String(agent.clone()));

    for field in DROPPED_FIELDS {
        map.remove(*field);
    }

    let cwd_missing = map
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(str::is_empty)
        .unwrap_or(true);
    if cwd_missing {
        if let Ok(cwd) = std::env::current_dir() {
            map.insert(
                "cwd".into(),
                serde_json::Value::String(cwd.to_string_lossy().to_string()),
            );
        }
    }

    // Which terminal the session runs in. Unlike macOS, Coucou on Windows accepts
    // events from every terminal, so this is context only — never a filter.
    for (key, var) in [
        ("term_program", "TERM_PROGRAM"),
        ("wt_session", "WT_SESSION"),
        ("term_session_id", "TERM_SESSION_ID"),
        ("vscode_pid", "VSCODE_PID"),
        ("session_pid", "CLAUDE_CODE_SSE_PORT"),
        // The project root Claude Code was started in. `cwd` follows every `cd`
        // inside the session; this does not, so it can name the session's pill.
        ("project_dir", "CLAUDE_PROJECT_DIR"),
    ] {
        if !map.contains_key(key) {
            let value = std::env::var(var).unwrap_or_default();
            map.insert(key.into(), serde_json::Value::String(value));
        }
    }

    // The window that terminal lives in, so the island can bring it to the front
    // and tell sessions apart. Looked up on every event, not just the first of a
    // session: the relay is a fresh process each time and cannot know which event
    // is the first, and a session that only ever fires tool events (Codex) would
    // otherwise never get one. Measured at well under the cost of starting the
    // process itself. If there is no window to be found the fields are simply absent.
    if let Some(t) = win::find_terminal() {
        map.insert("terminal_hwnd".into(), serde_json::json!(t.hwnd as i64));
        map.insert("terminal_pid".into(), serde_json::json!(t.pid));
        map.insert("terminal_exe".into(), serde_json::json!(t.exe));
    }

    truncate_strings(&mut payload);

    let mut line = payload.to_string();
    line.push('\n');
    Some((line, event, agent))
}

/// Caps every string in the payload. A single Write can carry a whole file.
fn truncate_strings(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(s) => {
            if s.len() > MAX_FIELD_LEN {
                // Cut on a char boundary; a lone byte index can split UTF-8.
                let mut end = MAX_FIELD_LEN;
                while end > 0 && !s.is_char_boundary(end) {
                    end -= 1;
                }
                s.truncate(end);
                s.push('…');
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(truncate_strings),
        serde_json::Value::Object(map) => map.values_mut().for_each(truncate_strings),
        _ => {}
    }
}

/// Connect, send, and — for a permission request — wait for the island's word.
fn talk(payload: &str, waits_for_answer: bool) -> Option<String> {
    let mut pipe = connect()?;

    if pipe.write_all(payload.as_bytes()).is_err() {
        return None;
    }
    let _ = pipe.flush();

    if !waits_for_answer {
        return None;
    }

    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.contains(&b'\n') {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let answer = String::from_utf8_lossy(&buf).trim().to_string();
    (!answer.is_empty()).then_some(answer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_json_matches_the_documented_shape() {
        assert_eq!(
            decision_json("allow").unwrap(),
            r#"{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}"#
        );
        assert_eq!(
            decision_json("deny").unwrap(),
            r#"{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"deny","message":"Denied from Coucou"}}}"#
        );
        // "always" is an island concept; Claude Code just gets an allow.
        assert!(decision_json("always").unwrap().contains(r#""behavior":"allow""#));
    }

    #[test]
    fn anything_unrecognised_prints_nothing() {
        assert!(decision_json("").is_none());
        assert!(decision_json("maybe").is_none());
        // The shape the app used to send must not be mistaken for a decision.
        assert!(decision_json(r#"{"permissionDecision":"allow"}"#).is_none());
    }

    fn args(list: &[&str]) -> (String, String) {
        parse_args(list.iter().map(|s| s.to_string()))
    }

    #[test]
    fn the_agent_defaults_to_claude_and_the_event_is_the_first_plain_argument() {
        assert_eq!(args(&["Stop"]), ("Stop".into(), "claude".into()));
        assert_eq!(args(&[]), ("".into(), "claude".into()));
        assert_eq!(
            args(&["PermissionRequest", "--agent", "codex"]),
            ("PermissionRequest".into(), "codex".into())
        );
        // Flag first is just as good.
        assert_eq!(args(&["--agent", "codex", "Stop"]), ("Stop".into(), "codex".into()));
    }

    #[test]
    fn a_stray_agent_value_can_only_become_a_short_lowercase_slug() {
        assert_eq!(args(&["Stop", "--agent"]).1, "claude");
        assert_eq!(args(&["Stop", "--agent", "Co-dex!!"]).1, "co-dex");
        assert_eq!(args(&["Stop", "--agent", "my_tool"]).1, "mytool");
        assert_eq!(args(&["Stop", "--agent", &"x".repeat(40)]).1.len(), 24);
    }

    #[test]
    fn gemini_events_become_the_ones_the_island_knows() {
        assert_eq!(canonical_event("gemini", "BeforeTool"), "PreToolUse");
        assert_eq!(canonical_event("gemini", "AfterTool"), "PostToolUse");
        assert_eq!(canonical_event("gemini", "BeforeAgent"), "UserPromptSubmit");
        assert_eq!(canonical_event("gemini", "AfterAgent"), "Stop");
        // Already canonical, or lifecycle events both agents share: untouched.
        assert_eq!(canonical_event("gemini", "SessionStart"), "SessionStart");
        assert_eq!(canonical_event("gemini", "PreToolUse"), "PreToolUse");
        // Only Gemini is translated: another agent's "BeforeTool" is not ours to guess.
        assert_eq!(canonical_event("codex", "BeforeTool"), "BeforeTool");
    }

    fn agy_payload(tool: &str, args: serde_json::Value) -> JsonMap {
        let mut v = serde_json::json!({
            "conversationId": "4c26bc5a-3352",
            "workspacePaths": ["C:/Users/lucas/proj"],
            "stepIdx": 2,
            "toolCall": { "name": tool, "args": args }
        });
        let map = v.as_object_mut().unwrap();
        agy_normalize(map);
        map.clone()
    }

    #[test]
    fn an_antigravity_payload_becomes_the_flat_one_the_island_reads() {
        let m = agy_payload("run_command", serde_json::json!({"CommandLine": "npm test", "Cwd": "C:/x"}));
        assert_eq!(m["session_id"], "4c26bc5a-3352");
        assert_eq!(m["cwd"], "C:/Users/lucas/proj");
        assert_eq!(m["project_dir"], "C:/Users/lucas/proj");
        assert_eq!(m["tool_name"], "run_command");
        assert_eq!(m["tool_input"]["command"], "npm test");
        // The original argument is still there for anyone who wants it.
        assert_eq!(m["tool_input"]["CommandLine"], "npm test");
    }

    #[test]
    fn antigravity_events_are_translated() {
        assert_eq!(canonical_event("antigravity", "PreInvocation"), "UserPromptSubmit");
        assert_eq!(canonical_event("antigravity", "PostInvocation"), "PostToolUse");
        assert_eq!(canonical_event("antigravity", "PreToolUse"), "PreToolUse");
        assert_eq!(canonical_event("antigravity", "Stop"), "Stop");
        // Not another agent's business.
        assert_eq!(canonical_event("codex", "PreInvocation"), "PreInvocation");
    }

    #[test]
    fn only_commands_agy_would_ask_about_go_to_the_island() {
        let rules = vec!["command(git status)".to_string(), "read_url(gstatic.com)".to_string()];
        let ask = |tool: &str, cmd: &str| {
            agy_needs_island(&agy_payload(tool, serde_json::json!({"CommandLine": cmd})), &rules)
        };
        // Already allowed: agy will not ask, so neither do we.
        assert!(!ask("run_command", "git status"));
        // New, or only *similar* to an allowed one: exact match or nothing.
        assert!(ask("run_command", "git push --force"));
        assert!(ask("run_command", "git status --short"));
        assert!(ask("run_command", ""));
        // Tools we only watch.
        assert!(!ask("view_file", "anything"));
    }

    #[test]
    fn agy_answers_are_the_documented_ones_and_silence_is_ask() {
        assert_eq!(agy_decision_json("allow", "").unwrap(), r#"{"decision":"allow"}"#);
        assert_eq!(agy_decision_json("always", "").unwrap(), r#"{"decision":"allow"}"#);
        assert_eq!(
            agy_decision_json("deny", "").unwrap(),
            r#"{"decision":"deny","reason":"Denied from Coucou"}"#
        );
        assert!(agy_decision_json("maybe", "").is_none());
        // The same click means a different JSON per agent.
        assert!(reply_for("claude", "allow", "").unwrap().contains("hookSpecificOutput"));
        assert_eq!(reply_for("antigravity", "allow", "").unwrap(), r#"{"decision":"allow"}"#);
        assert_eq!(
            reply_for("antigravity", "allow", "git status").unwrap(),
            r#"{"decision":"allow","permissionOverrides":["command(git status)"]}"#
        );
        // No answer must never become a block: agy gets `ask` on a tool check, `{}` elsewhere.
        assert_eq!(silent_reply("antigravity", "PreToolUse").unwrap(), AGY_ASK);
        assert_eq!(silent_reply("antigravity", "PermissionRequest").unwrap(), AGY_ASK);
        assert_eq!(silent_reply("antigravity", "Stop").unwrap(), "{}");
        assert_eq!(silent_reply("claude", "PermissionRequest"), None);
    }

    #[test]
    fn only_a_codex_stop_prints_without_a_decision() {
        assert_eq!(silent_reply("codex", "Stop").as_deref(), Some("{}"));
        assert!(silent_reply("claude", "Stop").is_none());
        assert!(silent_reply("codex", "PreToolUse").is_none());
        // A permission request nobody answered must stay silent for Codex too:
        // empty output is what hands the question back to the terminal.
        assert!(silent_reply("codex", "PermissionRequest").is_none());
    }

    #[test]
    fn long_strings_are_cut_on_a_char_boundary() {
        let mut v = serde_json::json!({ "tool_input": { "content": "é".repeat(4000) } });
        truncate_strings(&mut v);
        let s = v["tool_input"]["content"].as_str().unwrap();
        assert!(s.len() <= MAX_FIELD_LEN + 4);
        assert!(s.ends_with('…'));
    }
}
