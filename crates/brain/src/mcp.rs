// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Alfred's tools for Home Assistant's own conversation agent: a Model Context Protocol server
//! (Streamable HTTP, JSON answers only) that Home Assistant's built-in `mcp` integration
//! connects to. Home Assistant's AI thinks with its own key; Alfred lends memory, reminders,
//! the shopping list and the house with its gates.
//!
//! What Home Assistant's client does (homeassistant/components/mcp, 2026.10): a fresh
//! connection and `initialize` for every tool list (every 30 min) and every tool call, 30 s
//! timeout, Streamable HTTP first (POST, `Accept: application/json, text/event-stream`), SSE
//! only after a 404/405 before the session starts. No authentication: only Home Assistant's
//! address may connect (`allow`).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use crate::agent::Brain;

const MAX_BODY: usize = 1 << 20;

/// Protocol versions we speak (the newest first); the client's is echoed when we know it.
const VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26"];

/// One JSON-RPC message → its answer (`None` for notifications).
pub fn answer(brain: &Brain, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let id = id?; // notifications (initialized, cancelled…) get no answer
    let ok = |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result});
    let err = |code: i64, message: &str| json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}});
    let params = msg.get("params").cloned().unwrap_or(json!({}));
    Some(match method {
        "initialize" => {
            let asked = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("");
            let version = VERSIONS
                .iter()
                .find(|v| **v == asked)
                .copied()
                .unwrap_or(VERSIONS[0]);
            ok(json!({
                "protocolVersion": version,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": brain.name(), "version": env!("CARGO_PKG_VERSION")},
                "instructions": format!("{}'s tools: memory, reminders, shopping list, the house.", brain.name()),
            }))
        }
        "ping" => ok(json!({})),
        "tools/list" => {
            brain.tools_listed();
            let tools: Vec<Value> = brain
                .mcp_tools()
                .iter()
                .map(|t| {
                    let mut v = json!({"name": t.name, "description": t.description,
                        "inputSchema": t.parameters});
                    if matches!(
                        t.name,
                        "home_entities" | "recall" | "list_read" | "list_jobs" | "read_briefing"
                    ) {
                        v["annotations"] = json!({"readOnlyHint": true});
                    }
                    v
                })
                .collect();
            ok(json!({"tools": tools}))
        }
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            let (v, is_error) = brain.mcp_call(name, &args);
            let text = match &v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            ok(json!({"content": [{"type": "text", "text": text}], "isError": is_error}))
        }
        _ => err(-32601, "method not found"),
    })
}

struct Request {
    method: String,
    body: Vec<u8>,
}

fn read_request(r: &mut BufReader<TcpStream>) -> std::io::Result<Option<Request>> {
    let mut line = String::new();
    if r.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let method = line.split_whitespace().next().unwrap_or("").to_string();
    let mut len = 0usize;
    let mut chunked = false;
    loop {
        line.clear();
        if r.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let l = line.trim_end();
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            match k.trim().to_ascii_lowercase().as_str() {
                "content-length" => len = v.trim().parse().unwrap_or(0),
                "transfer-encoding" => chunked = v.to_ascii_lowercase().contains("chunked"),
                _ => {}
            }
        }
    }
    let mut body = Vec::new();
    if chunked {
        loop {
            line.clear();
            r.read_line(&mut line)?;
            let n = usize::from_str_radix(line.trim().split(';').next().unwrap_or("0"), 16)
                .unwrap_or(0);
            if n == 0 || body.len() + n > MAX_BODY {
                line.clear();
                let _ = r.read_line(&mut line);
                break;
            }
            let mut chunk = vec![0; n];
            r.read_exact(&mut chunk)?;
            body.extend_from_slice(&chunk);
            line.clear();
            r.read_line(&mut line)?;
        }
    } else if len > 0 {
        if len > MAX_BODY {
            return Err(std::io::Error::other("body too large"));
        }
        body.resize(len, 0);
        r.read_exact(&mut body)?;
    }
    Ok(Some(Request { method, body }))
}

fn respond(w: &mut TcpStream, status: &str, body: &str) -> std::io::Result<()> {
    let ctype = if body.is_empty() {
        ""
    } else {
        "Content-Type: application/json\r\n"
    };
    write!(
        w,
        "HTTP/1.1 {status}\r\n{ctype}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    w.flush()
}

fn serve(brain: &Brain, stream: TcpStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut w = stream.try_clone()?;
    let mut r = BufReader::new(stream);
    let Some(req) = read_request(&mut r)? else {
        return Ok(());
    };
    if req.method != "POST" {
        // No server-to-client stream: the client goes on with POSTs only.
        return respond(&mut w, "405 Method Not Allowed", "");
    }
    let msg: Value = match serde_json::from_slice(&req.body) {
        Ok(v) => v,
        Err(_) => {
            let e = json!({"jsonrpc": "2.0", "id": null,
                "error": {"code": -32700, "message": "parse error"}});
            return respond(&mut w, "400 Bad Request", &e.to_string());
        }
    };
    let out = match &msg {
        Value::Array(batch) => {
            let answers: Vec<Value> = batch.iter().filter_map(|m| answer(brain, m)).collect();
            (!answers.is_empty()).then_some(Value::Array(answers))
        }
        m => answer(brain, m),
    };
    match out {
        Some(v) => respond(&mut w, "200 OK", &v.to_string()),
        None => respond(&mut w, "202 Accepted", ""),
    }
}

/// Serves until the process ends; one thread per connection, only from `allow`.
pub fn run(brain: Arc<Brain>, listener: TcpListener, allow: Vec<IpAddr>) {
    for conn in listener.incoming() {
        let Ok(stream) = conn else { continue };
        let peer = stream.peer_addr().map(|a| a.ip()).ok();
        let ok = peer.is_some_and(|p| {
            allow.is_empty() || allow.contains(&p) || brain.learned_allow().contains(&p)
        });
        if !ok {
            log::warn!(event = "brain_refused"; "MCP connection from {peer:?} refused");
            continue;
        }
        let b = brain.clone();
        std::thread::spawn(move || {
            if let Err(e) = serve(&b, stream) {
                log::debug!("MCP connection: {e}");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_and_tools() {
        let dir =
            std::env::temp_dir().join(format!("home-companion-brain-mcp-{}", std::process::id()));
        let mut cfg = crate::config::Config::default();
        cfg.memory.dir = dir.display().to_string();
        let brain = Brain::new(cfg).unwrap();
        let init = answer(
            &brain,
            &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t"}}}),
        )
        .unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
        assert!(init["result"]["capabilities"]["tools"].is_object());
        assert!(
            answer(
                &brain,
                &json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
            )
            .is_none()
        );
        let list = answer(
            &brain,
            &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        )
        .unwrap();
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"remember") && names.contains(&"schedule"));
        assert!(!names.contains(&"web_search") && !names.contains(&"home_assist"));
        let r = answer(
            &brain,
            &json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                "params": {"name": "remember", "arguments": {"text": "Andrea ha il dentista il 12 novembre"}}}),
        )
        .unwrap();
        assert_eq!(r["result"]["isError"], false, "{r}");
        let r = answer(
            &brain,
            &json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
                "params": {"name": "recall", "arguments": {"query": "dentista"}}}),
        )
        .unwrap();
        assert!(
            r["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("dentista")
        );
        // Gemini refuses empty enum values and nested objects without properties.
        fn check(v: &Value, path: &str, top: bool) {
            if let Some(e) = v.get("enum").and_then(Value::as_array) {
                assert!(
                    e.iter().all(|x| x.as_str() != Some("")),
                    "{path}: empty enum value"
                );
            }
            if v.get("type").and_then(Value::as_str) == Some("object") && !top {
                assert!(
                    v.get("properties")
                        .and_then(Value::as_object)
                        .is_some_and(|p| !p.is_empty()),
                    "{path}: object without properties"
                );
            }
            if let Some(p) = v.get("properties").and_then(Value::as_object) {
                for (k, sub) in p {
                    check(sub, &format!("{path}.{k}"), false);
                }
            }
            if let Some(i) = v.get("items") {
                check(i, &format!("{path}[]"), false);
            }
        }
        let mut cfg2 = crate::config::Config::default();
        cfg2.memory.dir = dir.join("b2").display().to_string();
        cfg2.home_assistant.url = "http://ha.example:8123".into();
        cfg2.lists.shopping = "todo.spesa".into();
        let all = Brain::new(cfg2).unwrap();
        for t in all.mcp_tools() {
            check(&t.parameters, t.name, true);
        }
        let r = answer(
            &brain,
            &json!({"jsonrpc": "2.0", "id": 5, "method": "nope"}),
        )
        .unwrap();
        assert_eq!(r["error"]["code"], -32601);
        let _ = std::fs::remove_dir_all(dir);
    }
}
