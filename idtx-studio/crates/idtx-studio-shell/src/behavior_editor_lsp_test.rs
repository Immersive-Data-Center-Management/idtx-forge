//! Optional real language-server acceptance probe. It consumes the generated
//! workspace settings, including the actual SDK procedural-macro recipe.

use std::io::{BufRead, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

struct Client {
    process: Child,
    stdin: ChildStdin,
    messages: Receiver<Value>,
    config: Value,
    id: u64,
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

impl Client {
    fn start(program: &Path, workspace: &super::EditorWorkspace, config: Value) -> Self {
        let mut process = Command::new(program).envs(&workspace.context.environment)
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().unwrap();
        let stdin = process.stdin.take().unwrap();
        let stdout = process.stdout.take().unwrap();
        let (tx, messages) = channel();
        std::thread::spawn(move || {
            let mut reader = std::io::BufReader::new(stdout);
            loop {
                let mut length = None;
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap_or(0) == 0 { return; }
                    if header.trim().is_empty() { break; }
                    if let Some(value) = header.strip_prefix("Content-Length:") { length = value.trim().parse::<usize>().ok(); }
                }
                let mut body = vec![0; length.expect("LSP Content-Length")];
                if reader.read_exact(&mut body).is_err() { return; }
                if tx.send(serde_json::from_slice(&body).expect("LSP JSON")).is_err() { return; }
            }
        });
        Self { process, stdin, messages, config, id: 0 }
    }

    fn send(&mut self, message: &Value) {
        let body = serde_json::to_vec(message).unwrap();
        write!(self.stdin, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
        self.stdin.write_all(&body).unwrap();
        self.stdin.flush().unwrap();
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    fn next(&mut self, deadline: Instant) -> Value {
        let message = self.messages.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("rust-analyzer response before timeout");
        if message.get("id").is_some() && message.get("method").is_some() {
            let result = if message["method"] == "workspace/configuration" {
                Value::Array(message["params"]["items"].as_array().unwrap().iter().map(|item| {
                    let section = item["section"].as_str().unwrap_or("rust-analyzer");
                    section.strip_prefix("rust-analyzer.").map_or_else(|| self.config.clone(), |path|
                        path.split('.').fold(&self.config, |value, key| &value[key]).clone())
                }).collect())
            } else { Value::Null };
            self.send(&json!({"jsonrpc": "2.0", "id": message["id"], "result": result}));
        }
        message
    }

    fn wait(&mut self, predicate: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(180);
        loop {
            let message = self.next(deadline);
            if predicate(&message) { return message; }
        }
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        let id = self.id;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let message = self.wait(|m| m["id"] == id && m.get("method").is_none());
        assert!(message.get("error").is_none(), "{method}: {message}");
        message["result"].clone()
    }
}

fn uri(path: &Path) -> String { url::Url::from_file_path(path).unwrap().to_string() }

fn position(source: &str, needle: &str, extra: usize) -> Value {
    let before = &source[..source.find(needle).unwrap() + extra];
    json!({"line": before.bytes().filter(|b| *b == b'\n').count(), "character": before.rsplit('\n').next().unwrap().encode_utf16().count()})
}

fn analyzer_config(settings: &Value) -> Value {
    let mut result = json!({});
    for (key, value) in settings.as_object().unwrap() {
        let Some(key) = key.strip_prefix("rust-analyzer.") else { continue; };
        if key.starts_with("server.") { continue; }
        let mut target = &mut result;
        let parts: Vec<_> = key.split('.').collect();
        for part in &parts[..parts.len() - 1] {
            target = target.as_object_mut().unwrap().entry(*part).or_insert_with(|| json!({}));
        }
        target[parts[parts.len() - 1]] = value.clone();
    }
    result
}

#[test]
#[ignore = "requires a rust-analyzer server; set IDTX_TEST_RUST_ANALYZER to the VS Code extension's server binary"]
fn rust_analyzer_completes_unsaved_behavior_and_reports_original_helper_errors() {
    let server = std::env::var_os("IDTX_TEST_RUST_ANALYZER").expect("IDTX_TEST_RUST_ANALYZER");
    let temporary = tempfile::tempdir().unwrap();
    let project = temporary.path().join("author é project");
    let entry = project.join("behaviors/spin/behavior.rs");
    let helper = entry.with_file_name("motion.rs");
    std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
    let source = r#"use idtx_behavior_sdk::prelude::*;
mod motion;
#[derive(Default)]
pub struct Spin;
#[behavior(kind = "runtime")]
impl Spin {
    fn on_update(&mut self, ctx: &Ctx, dt: f32) -> Result<()> {
        let _ = ctx.scene();
        let _ = motion::step(dt);
        Ok(())
    }
}
"#;
    std::fs::write(&entry, source).unwrap();
    std::fs::write(&helper, "pub fn step(dt: f32) -> f32 { dt }").unwrap();
    let sdk = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("idtx-behavior-sdk");
    let workspace = super::prepare_workspace(&project, &entry, &sdk, false).unwrap();
    let text = std::fs::read_to_string(&workspace.path).unwrap();
    let document: Value = serde_json::from_str(text.split_once('\n').unwrap().1).unwrap();
    let config = analyzer_config(&document["settings"]);
    let mut client = Client::start(Path::new(&server), &workspace, config.clone());
    let entry_uri = uri(workspace.context.source.entry());
    let helper_uri = uri(&workspace.context.source.entry().with_file_name("motion.rs"));
    let root_uri = uri(workspace.context.source.root());
    client.request("initialize", json!({
        "processId": std::process::id(), "rootUri": root_uri,
        "workspaceFolders": [{"uri": root_uri, "name": "Behavior Sources"}],
        "capabilities": {
            "workspace": {"configuration": true, "workspaceFolders": true},
            "textDocument": {"completion": {"completionItem": {"snippetSupport": true}}},
            "window": {"workDoneProgress": true},
            "experimental": {"serverStatusNotification": true}
        },
        "initializationOptions": config
    }));
    client.notify("initialized", json!({}));
    client.notify("textDocument/didOpen", json!({"textDocument": {"uri": entry_uri, "languageId": "rust", "version": 1, "text": source}}));
    client.wait(|m| m["method"] == "experimental/serverStatus" && m["params"]["quiescent"] == true);

    let hover = client.request("textDocument/hover", json!({"textDocument": {"uri": entry_uri}, "position": position(source, "&Ctx", 2)}));
    assert!(hover.to_string().contains("Ctx"), "SDK hover: {hover}");
    for (version, expression) in [(2, "ctx."), (3, "ctx.sc")] {
        let unsaved = source.replace("ctx.scene()", expression);
        client.notify("textDocument/didChange", json!({"textDocument": {"uri": entry_uri, "version": version}, "contentChanges": [{"text": unsaved}]}));
        let completion = client.request("textDocument/completion", json!({
            "textDocument": {"uri": entry_uri}, "position": position(&unsaved, expression, expression.len()),
            "context": {"triggerKind": 1}
        }));
        let items = completion.as_array().or_else(|| completion["items"].as_array()).expect("completion items");
        assert!(items.iter().any(|i| i["label"].as_str().is_some_and(|s| s.starts_with("scene"))), "{expression}: {completion}");
    }
    client.notify("textDocument/didChange", json!({"textDocument": {"uri": entry_uri, "version": 4}, "contentChanges": [{"text": source}]}));
    let definition = client.request("textDocument/definition", json!({"textDocument": {"uri": entry_uri}, "position": position(source, "motion::step", 9)}));
    assert!(definition.as_array().unwrap().iter().any(|d| d["targetUri"] == helper_uri || d["uri"] == helper_uri), "helper definition: {definition}");
    let rename = client.request("textDocument/rename", json!({"textDocument": {"uri": entry_uri}, "position": position(source, "motion::step", 9), "newName": "advance"}));
    let rename_text = rename.to_string();
    assert!(rename_text.contains(&entry_uri) && rename_text.contains(&helper_uri), "rename must edit original files: {rename}");
    assert!(!rename_text.contains("/crates/"), "rename must not edit managed copies: {rename}");
    let unformatted = source.replace("let _ = ctx.scene();", "let _=ctx.scene( );");
    client.notify("textDocument/didChange", json!({"textDocument": {"uri": entry_uri, "version": 5}, "contentChanges": [{"text": unformatted}]}));
    let formatting = client.request("textDocument/formatting", json!({"textDocument": {"uri": entry_uri}, "options": {"tabSize": 4, "insertSpaces": true}}));
    assert!(formatting.as_array().is_some_and(|edits| !edits.is_empty()), "formatting: {formatting}");
    client.notify("textDocument/didChange", json!({"textDocument": {"uri": entry_uri, "version": 6}, "contentChanges": [{"text": source}]}));

    let broken = "pub fn step(_: f32) -> f32 { \"wrong type\" }";
    std::fs::write(&helper, broken).unwrap();
    client.notify("textDocument/didOpen", json!({"textDocument": {"uri": helper_uri, "languageId": "rust", "version": 1, "text": broken}}));
    client.notify("textDocument/didSave", json!({"textDocument": {"uri": helper_uri}}));
    client.wait(|m| m["method"] == "textDocument/publishDiagnostics" && m["params"]["uri"] == helper_uri
        && m["params"]["diagnostics"].as_array().is_some_and(|d| d.iter().any(|d| d["code"] == "E0308")));
    let fixed = "pub fn step(dt: f32) -> f32 { dt }";
    std::fs::write(&helper, fixed).unwrap();
    client.notify("textDocument/didChange", json!({"textDocument": {"uri": helper_uri, "version": 2}, "contentChanges": [{"text": fixed}]}));
    client.notify("textDocument/didSave", json!({"textDocument": {"uri": helper_uri}}));
    client.wait(|m| m["method"] == "textDocument/publishDiagnostics" && m["params"]["uri"] == helper_uri
        && m["params"]["diagnostics"].as_array().is_some_and(Vec::is_empty));
}
