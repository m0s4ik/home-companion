// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Home Assistant set up for the body, after the QR link, with only what a normal installation has and the link's token:
//!
//! | row | what | made by the body |
//! |---|---|---|
//! | `link` | the QR link's token works (an administrator's: config flows need one) | no: the QR page |
//! | `wyoming` | the Wyoming integration for the satellite (`host`, `port`) | yes |
//! | `mcp` | the MCP integration on home-companion-brain's tools (`http://<body>:<port>/mcp`) | yes; reloaded by "Check and repair" |
//! | `ai` | an AI integration with conversation agents (Gemini, OpenAI, Anthropic, Ollama…) | no: the user adds it; "skipped" without |
//! | `agent` | the agent "<name>" under it, the MCP tools attached | yes |
//! | `pipeline` | the pipeline "<name>": speech-to-text and voice copied from the preferred one, the agent, local intents off | yes |
//! | `satellite` | the satellite on that pipeline | yes (first time; afterwards only with "Repair") |
//! | `vad` | end of speech "relaxed" | yes (idem) |
//! | `ai_task` | an AI task entity for the memory jobs | no: the user's; "skipped" without |
//!
//! What exists is never changed by itself: what the body made is in `<state_dir>/ha-setup.json`
//! (with the satellite's options as they were) and [`unlink`] removes exactly that. A row that
//! differs because of the user's choice in Home Assistant (another pipeline on the satellite, the
//! agent's tools changed) is yellow, and only its own "Repair" puts it back.
//!
//! Everything goes through [`Api`] (Home Assistant's WebSocket and REST APIs): [`HaApi`] on the
//! device, a fake in the tests.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::auth::{self, Auth};
use super::ws::{Message, WebSocket};
use super::{Endpoint, endpoint};

/// What the body made, in `<state_dir>/ha-setup.json`.
pub const STATE_FILE: &str = "ha-setup.json";

/// Home Assistant's own agent: the pipeline's agent while there is no AI integration.
pub const DEFAULT_AGENT: &str = "conversation.home_assistant";

/// The rows, in screen order.
pub const ROWS: [&str; 9] = [
    "link",
    "wyoming",
    "mcp",
    "ai",
    "agent",
    "pipeline",
    "satellite",
    "vad",
    "ai_task",
];

/// A row's colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// Green.
    Ok,
    /// Yellow: works, but not as set up (the user's choice, or something only the user can add).
    Warn,
    /// Red: missing or broken.
    Bad,
    /// Grey: not needed or not possible now (no AI in Home Assistant).
    Skipped,
    /// Not checked yet.
    Unknown,
}

/// One row of Settings → Home Assistant: the locale key of its detail ("ha_<row>_<what>") and
/// the words to put in it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub id: String,
    pub level: Level,
    pub key: String,
    pub args: Vec<(String, String)>,
    /// "Repair" on the row: it puts back a choice the user changed in Home Assistant.
    pub repair: bool,
}

impl Row {
    fn new(id: &str, level: Level, what: &str) -> Row {
        Row {
            id: id.into(),
            level,
            key: format!("ha_{id}_{what}"),
            args: Vec::new(),
            repair: false,
        }
    }

    fn arg(mut self, k: &str, v: impl Into<String>) -> Row {
        self.args.push((k.into(), v.into()));
        self
    }

    fn repairable(mut self) -> Row {
        self.repair = true;
        self
    }

    /// The detail in the screen's language: `text(key)` with `{arg}`s filled in.
    pub fn detail(&self, text: impl Fn(&str) -> String) -> String {
        let mut s = text(&self.key);
        for (k, v) in &self.args {
            s = s.replace(&format!("{{{k}}}"), v);
        }
        s
    }
}

/// What the body made (and what was there before).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// The user said "set up" once: the checks run by themselves from now on.
    pub configured: bool,
    pub wyoming_entry: String,
    pub wyoming_ours: bool,
    /// The satellite's `assist_satellite` entity (handed to the brain for its announcements).
    pub satellite: String,
    pub mcp_entry: String,
    pub mcp_ours: bool,
    /// The AI integration the agent lives under, and the agent (a subentry of it).
    pub ai_entry: String,
    pub agent_sub: String,
    pub agent_ours: bool,
    pub pipeline: String,
    pub pipeline_ours: bool,
    /// The satellite's selects and their options before the body set them.
    pub satellite_select: String,
    pub satellite_before: String,
    pub vad_select: String,
    pub vad_before: String,
    /// The brain's tools as Home Assistant last read them (a hash, from the brain): another
    /// value at start = the tools changed, the MCP integration is reloaded.
    pub tools_hash: String,
    /// The last check by itself (seconds since the epoch).
    pub checked_at: u64,
    /// Home Assistant's own addresses (its network adapters): they may connect to the satellite
    /// and to the brain's tools without `satellite.allowed`.
    pub ha_addresses: Vec<String>,
}

/// Home Assistant's own addresses, from its network adapters (`network`, admin only): the
/// enabled ones (all of them when none is), never loopback. Empty when it cannot tell.
pub fn ha_addresses(api: &mut dyn Api) -> Vec<String> {
    let Ok(r) = api.ws(json!({"type": "network"})) else {
        return Vec::new();
    };
    let adapters = r["adapters"].as_array().cloned().unwrap_or_default();
    let any_enabled = adapters.iter().any(|a| a["enabled"] == true);
    let mut out = Vec::new();
    for a in adapters
        .iter()
        .filter(|a| !any_enabled || a["enabled"] == true)
    {
        for fam in ["ipv4", "ipv6"] {
            for ip in a[fam].as_array().into_iter().flatten() {
                if let Some(ip) = ip["address"]
                    .as_str()
                    .and_then(|s| s.parse::<std::net::IpAddr>().ok())
                    && !ip.is_loopback()
                    && !out.contains(&ip.to_string())
                {
                    out.push(ip.to_string());
                }
            }
        }
    }
    out
}

/// Learns Home Assistant's addresses (each run): kept in the state file and put in the shared
/// list the satellite's server reads. True when they changed.
fn learn_addresses(api: &mut dyn Api, st: &mut State, learned: &Option<Learned>) -> bool {
    let now = ha_addresses(api);
    if now.is_empty() || now == st.ha_addresses {
        return false;
    }
    log::info!(event = "ha_setup"; "Home Assistant's addresses: {}", now.join(", "));
    st.ha_addresses = now;
    set_learned(learned, &st.ha_addresses);
    true
}

/// The satellite's list of learned addresses (wyoming::ServerConfig::learned).
pub type Learned = Arc<std::sync::RwLock<Vec<wyoming::Net>>>;

pub fn set_learned(learned: &Option<Learned>, addrs: &[String]) {
    if let Some(l) = learned
        && let Ok(mut v) = l.write()
    {
        *v = addrs
            .iter()
            .filter_map(|a| wyoming::Net::parse(a).ok())
            .collect();
    }
}

impl State {
    pub fn load(path: &Path) -> State {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)
    }
}

/// Home Assistant's APIs, as the setup uses them.
pub trait Api {
    /// One WebSocket command (`type` and its fields; the id is added): its `result`.
    fn ws(&mut self, msg: Value) -> Result<Value, String>;
    /// One REST call: (status, JSON body or null).
    fn rest(
        &mut self,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Result<(u16, Value), String>;
}

/// What the setup needs to know about the body.
#[derive(Debug, Clone)]
pub struct Body {
    /// The satellite's (and the character's) name: the agent and the pipeline are called so.
    pub name: String,
    /// The body's address as Home Assistant reaches it.
    pub host: String,
    pub wyoming_port: u16,
    /// home-companion-brain's MCP server.
    pub mcp_port: u16,
    /// The agent's instructions (the character's persona and how to use the tools).
    pub prompt: String,
}

impl Body {
    pub fn mcp_url(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        format!("http://{host}:{}/mcp", self.mcp_port)
    }
}

/// What the brain says about the tools (its `status`): when Home Assistant last read them and
/// their hash. None = the brain does not answer.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BrainInfo {
    pub tools_listed_at: u64,
    pub tools_hash: String,
}

/// Home Assistant as one check saw it.
#[derive(Debug, Default, Clone)]
struct Seen {
    entries: Vec<Value>,
    registry: Vec<Value>,
    states: Vec<Value>,
    pipelines: Vec<Value>,
    preferred: String,
    is_admin: bool,
}

impl Seen {
    fn entry(&self, id: &str) -> Option<&Value> {
        (!id.is_empty())
            .then(|| self.entries.iter().find(|e| s(e, "entry_id") == id))
            .flatten()
    }

    fn state(&self, entity: &str) -> Option<&Value> {
        self.states.iter().find(|e| s(e, "entity_id") == entity)
    }

    fn options(&self, entity: &str) -> Vec<String> {
        self.state(entity)
            .and_then(|st| st.pointer("/attributes/options"))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|o| o.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn current(&self, entity: &str) -> String {
        self.state(entity)
            .map(|st| s(st, "state").to_string())
            .unwrap_or_default()
    }

    fn pipeline(&self, id: &str) -> Option<&Value> {
        (!id.is_empty())
            .then(|| self.pipelines.iter().find(|p| s(p, "id") == id))
            .flatten()
    }

    /// The entity of an entry's registry with this unique id suffix ("-pipeline").
    fn entity_of(&self, entry: &str, suffix: &str) -> Option<String> {
        self.registry
            .iter()
            .find(|e| s(e, "config_entry_id") == entry && s(e, "unique_id").ends_with(suffix))
            .map(|e| s(e, "entity_id").to_string())
    }

    /// The conversation entity of an agent (a subentry).
    fn agent_entity(&self, sub: &str) -> Option<String> {
        if sub.is_empty() {
            return None;
        }
        self.registry
            .iter()
            .find(|e| {
                s(e, "config_subentry_id") == sub && s(e, "entity_id").starts_with("conversation.")
            })
            .map(|e| s(e, "entity_id").to_string())
    }

    /// The AI integrations: entries that can have conversation agents (not the MCP client).
    fn ais(&self) -> Vec<&Value> {
        self.entries
            .iter()
            .filter(|e| {
                e.pointer("/supported_subentry_types/conversation")
                    .is_some()
                    && s(e, "domain") != "mcp"
                    && s(e, "disabled_by").is_empty()
            })
            .collect()
    }

    fn friendly(&self, entity: &str) -> String {
        self.state(entity)
            .and_then(|st| st.pointer("/attributes/friendly_name"))
            .and_then(Value::as_str)
            .unwrap_or(entity)
            .to_string()
    }
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn hm(secs: u64) -> String {
    let t = secs as libc::time_t;
    // SAFETY: localtime_r only writes into the struct we pass.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
    }
}

fn read(api: &mut dyn Api) -> Result<Seen, String> {
    let (status, entries) = api.rest("GET", "/api/config/config_entries/entry", None)?;
    if status == 401 || status == 403 {
        return Err(format!("config entries: HTTP {status}"));
    }
    let user = api.ws(json!({"type": "auth/current_user"}))?;
    let pl = api.ws(json!({"type": "assist_pipeline/pipeline/list"}))?;
    Ok(Seen {
        entries: entries.as_array().cloned().unwrap_or_default(),
        registry: api
            .ws(json!({"type": "config/entity_registry/list"}))?
            .as_array()
            .cloned()
            .unwrap_or_default(),
        states: api
            .ws(json!({"type": "get_states"}))?
            .as_array()
            .cloned()
            .unwrap_or_default(),
        pipelines: pl
            .get("pipelines")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        preferred: s(&pl, "preferred_pipeline").to_string(),
        is_admin: user
            .get("is_admin")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

/// The state of an entry: "loaded", "setup_retry", "setup_error", "not_loaded"…
fn entry_state(e: &Value) -> &str {
    s(e, "state")
}

/// The Wyoming entry of this satellite: the one the body made, else one titled with its name
/// that has a satellite.
fn find_wyoming(seen: &Seen, st: &State, body: &Body) -> Option<String> {
    if seen.entry(&st.wyoming_entry).is_some() {
        return Some(st.wyoming_entry.clone());
    }
    seen.entries
        .iter()
        .filter(|e| s(e, "domain") == "wyoming" && s(e, "title") == body.name)
        .map(|e| s(e, "entry_id").to_string())
        .find(|id| seen.entity_of(id, "-assist_satellite").is_some())
}

fn find_mcp(seen: &Seen, st: &State, body: &Body) -> Option<String> {
    if seen.entry(&st.mcp_entry).is_some() {
        return Some(st.mcp_entry.clone());
    }
    seen.entries
        .iter()
        .find(|e| s(e, "domain") == "mcp" && s(e, "title") == body.name)
        .map(|e| s(e, "entry_id").to_string())
}

/// The AI to use: the one the agent lives under while it exists, else the first one.
fn find_ai<'a>(seen: &'a Seen, st: &State) -> Option<&'a Value> {
    let ais = seen.ais();
    ais.iter()
        .find(|e| s(e, "entry_id") == st.ai_entry)
        .or(ais.first())
        .copied()
}

/// The agent's current settings, read from its reconfigure form (Home Assistant has no other
/// way to read a subentry's data); the flow is closed again.
fn agent_settings(api: &mut dyn Api, ai: &str, sub: &str) -> Result<Value, String> {
    let (_, f) = api.rest(
        "POST",
        "/api/config/config_entries/subentries/flow",
        Some(
            json!({"handler": [ai, "conversation"], "show_advanced_options": false,
                    "subentry_id": sub, "source": "reconfigure"}),
        ),
    )?;
    if let Some(id) = f.get("flow_id").and_then(Value::as_str) {
        let _ = api.rest(
            "DELETE",
            &format!("/api/config/config_entries/subentries/flow/{id}"),
            None,
        );
    }
    let mut out = serde_json::Map::new();
    for field in f
        .get("data_schema")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let v = field
            .pointer("/description/suggested_value")
            .or_else(|| field.get("default"))
            .cloned()
            .unwrap_or(Value::Null);
        out.insert(s(field, "name").to_string(), v);
    }
    Ok(Value::Object(out))
}

/// The LLM API id of an MCP entry, as agents list it.
fn mcp_api(entry: &str) -> String {
    format!("mcp-{entry}")
}

fn has_api(settings: &Value, api: &str) -> bool {
    match settings.get("llm_hass_api") {
        Some(Value::Array(a)) => a.iter().any(|v| v.as_str() == Some(api)),
        Some(Value::String(v)) => v == api,
        _ => false,
    }
}

/// Asks the brain about its tools (None: it does not answer).
pub type BrainFn = fn() -> Option<BrainInfo>;

/// Every row, read-only. `brain` = what the brain says (None: it does not answer).
pub fn check(api: &mut dyn Api, st: &State, body: &Body, brain: BrainFn) -> Vec<Row> {
    let seen = match read(api) {
        Ok(s) => s,
        Err(e) => {
            let mut rows = vec![Row::new("link", Level::Bad, "unreachable").arg("error", e)];
            rows.extend(
                ROWS[1..]
                    .iter()
                    .map(|id| Row::new(id, Level::Unknown, "unknown")),
            );
            return rows;
        }
    };
    let mut cache = AgentCache::default();
    rows_of(api, &seen, st, body, brain, &mut cache)
}

#[derive(Default)]
struct AgentCache {
    settings: Option<Value>,
}

fn rows_of(
    api: &mut dyn Api,
    seen: &Seen,
    st: &State,
    body: &Body,
    brain: BrainFn,
    cache: &mut AgentCache,
) -> Vec<Row> {
    let brain = brain();
    let mut rows = Vec::new();
    rows.push(if seen.is_admin {
        Row::new("link", Level::Ok, "ok")
    } else {
        Row::new("link", Level::Bad, "not_admin")
    });

    // The satellite.
    let wy = find_wyoming(seen, st, body);
    let wy_entry = wy.as_deref().and_then(|id| seen.entry(id));
    rows.push(match wy_entry {
        None => {
            Row::new("wyoming", Level::Bad, "missing").arg("port", body.wyoming_port.to_string())
        }
        Some(e) if entry_state(e) == "loaded" => Row::new("wyoming", Level::Ok, "ok").arg(
            "satellite",
            wy.as_deref()
                .and_then(|w| seen.entity_of(w, "-assist_satellite"))
                .unwrap_or_default(),
        ),
        Some(e) => Row::new("wyoming", Level::Bad, "not_loaded")
            .arg("state", entry_state(e))
            .arg("port", body.wyoming_port.to_string()),
    });

    // The tools.
    let mcp = find_mcp(seen, st, body);
    rows.push(match mcp.as_deref().and_then(|id| seen.entry(id)) {
        None => Row::new("mcp", Level::Bad, "missing"),
        Some(e) if entry_state(e) != "loaded" => Row::new("mcp", Level::Bad, "not_loaded")
            .arg("state", entry_state(e))
            .arg("port", body.mcp_port.to_string()),
        Some(_) => match &brain {
            None => Row::new("mcp", Level::Warn, "no_brain"),
            Some(b) if b.tools_listed_at == 0 => Row::new("mcp", Level::Ok, "ok_not_read"),
            Some(b) => Row::new("mcp", Level::Ok, "ok").arg("time", hm(b.tools_listed_at)),
        },
    });

    // The AI and the agent.
    let ai = find_ai(seen, st);
    rows.push(match ai {
        None => Row::new("ai", Level::Skipped, "none"),
        Some(e) if entry_state(e) != "loaded" => Row::new("ai", Level::Bad, "not_loaded")
            .arg("ai", s(e, "title"))
            .arg("state", entry_state(e)),
        Some(e) => Row::new("ai", Level::Ok, "ok").arg("ai", s(e, "title")),
    });
    let agent_here =
        ai.is_some_and(|e| s(e, "entry_id") == st.ai_entry) && !st.agent_sub.is_empty();
    let agent = if agent_here {
        seen.agent_entity(&st.agent_sub)
    } else {
        None
    };
    rows.push(match (ai, &agent) {
        (None, _) => Row::new("agent", Level::Skipped, "no_ai"),
        (Some(_), None) => Row::new("agent", Level::Bad, "missing"),
        (Some(e), Some(entity)) => {
            if cache.settings.is_none() {
                cache.settings = agent_settings(api, s(e, "entry_id"), &st.agent_sub).ok();
            }
            let settings = cache.settings.clone().unwrap_or(Value::Null);
            let recommended = settings.get("recommended") == Some(&json!(true));
            let model = ["chat_model", "model"]
                .iter()
                .find_map(|k| settings.get(*k).and_then(Value::as_str))
                .filter(|m| !m.is_empty() && !recommended)
                .map(String::from);
            match &mcp {
                Some(m) if !settings.is_null() && !has_api(&settings, &mcp_api(m)) => {
                    Row::new("agent", Level::Warn, "no_tools")
                        .arg("agent", entity.clone())
                        .repairable()
                }
                _ => match model {
                    Some(m) => Row::new("agent", Level::Ok, "ok_model")
                        .arg("agent", entity.clone())
                        .arg("model", m),
                    None if recommended => {
                        Row::new("agent", Level::Ok, "ok").arg("agent", entity.clone())
                    }
                    None => Row::new("agent", Level::Ok, "ok_plain").arg("agent", entity.clone()),
                },
            }
        }
    });
    // The detail names the AI it lives under ("With Google Generative AI").
    if let (Some(e), Some(r)) = (ai, rows.last_mut()) {
        r.args.push(("ai".into(), s(e, "title").to_string()));
    }

    // The pipeline.
    let want_agent = agent.clone().unwrap_or_else(|| DEFAULT_AGENT.to_string());
    let pipeline = seen.pipeline(&st.pipeline);
    rows.push(match pipeline {
        None => Row::new("pipeline", Level::Bad, "missing"),
        Some(p) => {
            let engine = s(p, "conversation_engine");
            let engine_exists = engine == DEFAULT_AGENT || seen.state(engine).is_some();
            let names = |id: &str| {
                if id.is_empty() {
                    "—".to_string()
                } else {
                    seen.friendly(id)
                }
            };
            let base = |lvl, what| {
                Row::new("pipeline", lvl, what)
                    .arg("stt", names(s(p, "stt_engine")))
                    .arg("tts", names(s(p, "tts_engine")))
            };
            if !engine_exists {
                base(Level::Bad, "agent_gone")
            } else if engine != want_agent || p.get("prefer_local_intents") == Some(&json!(true)) {
                base(Level::Warn, "changed").repairable()
            } else if s(p, "stt_engine").is_empty() || s(p, "tts_engine").is_empty() {
                base(Level::Warn, "no_voice")
            } else {
                base(Level::Ok, "ok")
            }
        }
    });

    // The satellite's selects.
    let pipe_name = pipeline
        .map(|p| s(p, "name").to_string())
        .unwrap_or_else(|| body.name.clone());
    for (id, suffix, want) in [
        ("satellite", "-pipeline", pipe_name.as_str()),
        ("vad", "-vad_sensitivity", "relaxed"),
    ] {
        let sel = wy.as_deref().and_then(|w| seen.entity_of(w, suffix));
        rows.push(match sel {
            None => Row::new(id, Level::Unknown, "no_satellite"),
            Some(sel) => {
                let now = seen.current(&sel);
                if now == want {
                    Row::new(id, Level::Ok, "ok")
                } else if id == "satellite" && pipeline.is_none() {
                    Row::new(id, Level::Bad, "no_pipeline")
                } else {
                    let set_before = if id == "satellite" {
                        &st.satellite_select
                    } else {
                        &st.vad_select
                    };
                    // Home Assistant's own options have words of their own on the screen.
                    let what = match (id, now.as_str()) {
                        ("satellite", "preferred") => "changed_preferred",
                        ("vad", "default") => "changed_default",
                        ("vad", "aggressive") => "changed_aggressive",
                        _ => "changed",
                    };
                    let r = Row::new(
                        id,
                        if set_before.is_empty() {
                            Level::Bad
                        } else {
                            Level::Warn
                        },
                        what,
                    )
                    .arg("now", now);
                    if set_before.is_empty() {
                        r
                    } else {
                        r.repairable()
                    }
                }
            }
        });
    }

    // The AI task of the memory jobs.
    let first_task = seen
        .states
        .iter()
        .map(|e| s(e, "entity_id"))
        .find(|id| id.starts_with("ai_task."))
        .map(String::from);
    let preferred = api
        .ws(json!({"type": "ai_task/preferences/get"}))
        .ok()
        .and_then(|p| {
            p.get("gen_data_entity_id")
                .and_then(Value::as_str)
                .map(String::from)
        });
    rows.push(match preferred.or(first_task) {
        Some(t) => Row::new("ai_task", Level::Ok, "ok").arg("entity", seen.friendly(&t)),
        None => Row::new("ai_task", Level::Skipped, "none"),
    });
    rows
}

/// A step's outcome, for the screen while the setup runs.
pub type Progress<'a> = &'a mut dyn FnMut(&[Row]);

/// What [`repair`] may do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Everything missing or broken; the user's choices stay (the first setup, "Check and repair").
    Missing,
    /// That row too, even if it is the user's choice ("Repair" on the row).
    Row(String),
}

fn flow(api: &mut dyn Api, handler: &str, data: Value) -> Result<Value, String> {
    let (_, f) = api.rest(
        "POST",
        "/api/config/config_entries/flow",
        Some(json!({"handler": handler, "show_advanced_options": false})),
    )?;
    match s(&f, "type") {
        "form" => {}
        "abort" => return Ok(f),
        t => {
            return Err(format!(
                "{handler}: {t} {}",
                f.get("errors").unwrap_or(&Value::Null)
            ));
        }
    }
    let id = s(&f, "flow_id").to_string();
    let (_, r) = api.rest(
        "POST",
        &format!("/api/config/config_entries/flow/{id}"),
        Some(data),
    )?;
    if s(&r, "type") == "form" {
        let _ = api.rest(
            "DELETE",
            &format!("/api/config/config_entries/flow/{id}"),
            None,
        );
        return Err(format!(
            "{handler}: {}",
            r.get("errors").map(Value::to_string).unwrap_or_default()
        ));
    }
    Ok(r)
}

fn select(api: &mut dyn Api, entity: &str, option: &str) -> Result<(), String> {
    api.ws(
        json!({"type": "call_service", "domain": "select", "service": "select_option",
                  "service_data": {"entity_id": entity, "option": option}}),
    )
    .map(|_| ())
}

/// Waits (up to `secs`) for something to appear in Home Assistant after a change.
fn wait_for<T>(
    api: &mut dyn Api,
    secs: u32,
    mut probe: impl FnMut(&mut dyn Api) -> Option<T>,
) -> Option<T> {
    for i in 0..=secs * 2 {
        if let Some(v) = probe(api) {
            return Some(v);
        }
        if i < secs * 2 {
            std::thread::sleep(Duration::from_millis(500));
        }
    }
    None
}

fn registry(api: &mut dyn Api) -> Vec<Value> {
    api.ws(json!({"type": "config/entity_registry/list"}))
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
}

/// Does what is missing (and, with [`Scope::Row`], puts that row back), saving what it made in
/// `st` after every step; `progress` gets the rows after each one. Returns the final rows.
pub fn repair(
    api: &mut dyn Api,
    st: &mut State,
    state_file: &Path,
    body: &Body,
    brain: BrainFn,
    scope: Scope,
    progress: Progress,
) -> Result<Vec<Row>, String> {
    let save = |st: &State| {
        if let Err(e) = st.save(state_file) {
            log::warn!(event = "ha_setup"; "{}: {e}", state_file.display());
        }
    };
    let forced = |id: &str| scope == Scope::Row(id.to_string());
    let mut seen = read(api)?;
    if !seen.is_admin {
        return Err("not_admin".into());
    }
    let mut cache = AgentCache::default();
    progress(&rows_of(api, &seen, st, body, brain, &mut cache));

    // 1. Wyoming.
    match find_wyoming(&seen, st, body) {
        Some(id) => {
            if st.wyoming_entry != id {
                st.wyoming_entry = id;
                save(st);
            }
        }
        None => {
            let r = flow(
                api,
                "wyoming",
                json!({"host": body.host, "port": body.wyoming_port}),
            )?;
            if s(&r, "type") == "create_entry" {
                st.wyoming_entry = r
                    .pointer("/result/entry_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .into();
                st.wyoming_ours = true;
                log::info!(event = "ha_setup"; "Wyoming integration created");
            } else {
                // "already_configured": made by hand, under another title.
                log::info!(event = "ha_setup"; "Wyoming: {}", s(&r, "reason"));
            }
            save(st);
        }
    }

    // 2. MCP.
    match find_mcp(&seen, st, body) {
        Some(id) => {
            if st.mcp_entry != id {
                st.mcp_entry = id;
                save(st);
            }
            // Home Assistant reads the tools again now (by itself only every 30 minutes).
            let _ = api.rest(
                "POST",
                &format!("/api/config/config_entries/entry/{}/reload", st.mcp_entry),
                None,
            );
        }
        None => {
            let r = flow(api, "mcp", json!({"url": body.mcp_url()}))?;
            match s(&r, "type") {
                "create_entry" => {
                    st.mcp_entry = r
                        .pointer("/result/entry_id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .into();
                    st.mcp_ours = true;
                    log::info!(event = "ha_setup"; "MCP integration created: {}", body.mcp_url());
                }
                _ => return Err(format!("mcp: {}", s(&r, "reason"))),
            }
            save(st);
        }
    }
    if let Some(b) = brain() {
        st.tools_hash = b.tools_hash;
        save(st);
    }
    seen = read(api)?;
    progress(&rows_of(api, &seen, st, body, brain, &mut cache));

    // 3. The agent, under the AI (if there is one).
    let mut agent: Option<String> = None;
    if let Some(ai) = find_ai(&seen, st).cloned() {
        let ai_id = s(&ai, "entry_id").to_string();
        if ai_id != st.ai_entry {
            // Another AI (or the first): a new agent under it; the old one went with its AI.
            st.ai_entry = ai_id.clone();
            st.agent_sub.clear();
        }
        if st.agent_sub.is_empty() {
            // One made before (by the prototype, or the state file was lost)?
            let subs =
                api.ws(json!({"type": "config_entries/subentries/list", "entry_id": ai_id}))?;
            if let Some(sub) = subs
                .as_array()
                .into_iter()
                .flatten()
                .find(|x| s(x, "subentry_type") == "conversation" && s(x, "title") == body.name)
            {
                st.agent_sub = s(sub, "subentry_id").to_string();
            } else {
                st.agent_sub = create_agent(api, &ai_id, &st.mcp_entry, body)?;
                st.agent_ours = true;
                log::info!(event = "ha_setup"; "agent created under {}", s(&ai, "title"));
            }
            save(st);
        } else if forced("agent") {
            add_tools(api, &ai_id, &st.agent_sub, &st.mcp_entry)?;
        }
        let sub = st.agent_sub.clone();
        // The AI integration reloads after a new agent: its entity comes a few seconds later.
        agent = wait_for(api, 20, |api| {
            registry(api)
                .iter()
                .find(|e| {
                    s(e, "config_subentry_id") == sub
                        && s(e, "entity_id").starts_with("conversation.")
                })
                .map(|e| s(e, "entity_id").to_string())
        });
        if agent.is_none() {
            return Err("agent: no entity".into());
        }
    }
    seen = read(api)?;
    cache = AgentCache::default();
    progress(&rows_of(api, &seen, st, body, brain, &mut cache));

    // 4. The pipeline.
    let want_agent = agent.unwrap_or_else(|| DEFAULT_AGENT.to_string());
    match seen.pipeline(&st.pipeline).cloned() {
        Some(p) => {
            let engine = s(&p, "conversation_engine");
            let engine_gone = engine != DEFAULT_AGENT && seen.state(engine).is_none();
            // The agent changed under the body (a new AI, the first AI): ours to follow. A
            // choice the user made: only with "Repair".
            let follows = engine_gone || (engine == DEFAULT_AGENT && want_agent != DEFAULT_AGENT);
            if (follows || forced("pipeline"))
                && (engine != want_agent || p.get("prefer_local_intents") == Some(&json!(true)))
            {
                let mut upd = p.clone();
                let o = upd.as_object_mut().ok_or("pipeline")?;
                o.remove("id");
                o.insert("conversation_engine".into(), json!(want_agent));
                o.insert("prefer_local_intents".into(), json!(false));
                o.insert("type".into(), json!("assist_pipeline/pipeline/update"));
                o.insert("pipeline_id".into(), json!(st.pipeline));
                api.ws(upd)?;
                log::info!(event = "ha_setup"; "pipeline: agent {want_agent}");
            }
        }
        None => {
            let pref = seen
                .pipeline(&seen.preferred)
                .or(seen.pipelines.first())
                .cloned()
                .unwrap_or(json!({}));
            let mut new = serde_json::Map::new();
            for k in [
                "language",
                "stt_engine",
                "stt_language",
                "tts_engine",
                "tts_language",
                "tts_voice",
            ] {
                new.insert(k.into(), pref.get(k).cloned().unwrap_or(Value::Null));
            }
            let lang = pref
                .get("conversation_language")
                .cloned()
                .unwrap_or(Value::Null);
            new.insert("conversation_language".into(), lang);
            // The body hears its own wake word.
            new.insert("wake_word_entity".into(), Value::Null);
            new.insert("wake_word_id".into(), Value::Null);
            new.insert("name".into(), json!(body.name));
            new.insert("conversation_engine".into(), json!(want_agent));
            // Local intents off: Home Assistant's own commands would bypass the gates.
            new.insert("prefer_local_intents".into(), json!(false));
            new.insert("type".into(), json!("assist_pipeline/pipeline/create"));
            let r = api.ws(Value::Object(new))?;
            st.pipeline = s(&r, "id").to_string();
            st.pipeline_ours = true;
            save(st);
            log::info!(event = "ha_setup"; "pipeline created");
        }
    }
    seen = read(api)?;
    progress(&rows_of(api, &seen, st, body, brain, &mut cache));

    // 5. The satellite's selects: set the first time; afterwards the user's choice.
    let wy = find_wyoming(&seen, st, body);
    if let Some(w) = wy {
        if let Some(sat) = seen.entity_of(&w, "-assist_satellite")
            && st.satellite != sat
        {
            st.satellite = sat;
            save(st);
        }
        let pipe_name = seen
            .pipeline(&st.pipeline)
            .map(|p| s(p, "name").to_string())
            .unwrap_or_else(|| body.name.clone());
        // The selects appear with the satellite's first connection.
        let w2 = w.clone();
        let pipe_sel = wait_for(api, 15, |api| {
            registry(api)
                .iter()
                .find(|e| s(e, "config_entry_id") == w2 && s(e, "unique_id").ends_with("-pipeline"))
                .map(|e| s(e, "entity_id").to_string())
        });
        seen = read(api)?;
        if let Some(sel) = pipe_sel {
            let first = st.satellite_select.is_empty();
            if (first || forced("satellite")) && seen.current(&sel) != pipe_name {
                // The new pipeline is among the options a moment after it was made.
                let (sel2, name2) = (sel.clone(), pipe_name.clone());
                wait_for(api, 10, |api| {
                    let states = api.ws(json!({"type": "get_states"})).ok()?;
                    states
                        .as_array()?
                        .iter()
                        .find(|e| s(e, "entity_id") == sel2)?
                        .pointer("/attributes/options")?
                        .as_array()?
                        .iter()
                        .any(|o| o.as_str() == Some(&name2))
                        .then_some(())
                });
                if first {
                    st.satellite_select = sel.clone();
                    st.satellite_before = seen.current(&sel);
                    save(st);
                }
                select(api, &sel, &pipe_name)?;
            } else if first {
                st.satellite_select = sel.clone();
                st.satellite_before = seen.current(&sel);
                save(st);
            }
        }
        if let Some(sel) = seen.entity_of(&w, "-vad_sensitivity") {
            let first = st.vad_select.is_empty();
            if first {
                st.vad_select = sel.clone();
                st.vad_before = seen.current(&sel);
                save(st);
            }
            if (first || forced("vad"))
                && seen.current(&sel) != "relaxed"
                && seen.options(&sel).iter().any(|o| o == "relaxed")
            {
                // 1.25 s of silence instead of 0.7: the first pause does not end the sentence.
                select(api, &sel, "relaxed")?;
            }
        }
    }
    st.configured = true;
    save(st);
    seen = read(api)?;
    cache = AgentCache::default();
    let rows = rows_of(api, &seen, st, body, brain, &mut cache);
    progress(&rows);
    Ok(rows)
}

/// A new agent "<name>" under the AI integration, the MCP tools attached.
fn create_agent(api: &mut dyn Api, ai: &str, mcp: &str, body: &Body) -> Result<String, String> {
    let (_, f) = api.rest(
        "POST",
        "/api/config/config_entries/subentries/flow",
        Some(json!({"handler": [ai, "conversation"], "show_advanced_options": false})),
    )?;
    let flow_id = s(&f, "flow_id").to_string();
    let fields: Vec<Value> = f
        .get("data_schema")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut data = serde_json::Map::new();
    let names: Vec<&str> = fields.iter().map(|x| s(x, "name")).collect();
    if names.contains(&"name") {
        data.insert("name".into(), json!(body.name));
    }
    if names.contains(&"prompt") {
        data.insert("prompt".into(), json!(body.prompt));
    }
    if names.contains(&"llm_hass_api") {
        data.insert("llm_hass_api".into(), json!([mcp_api(mcp)]));
    }
    if names.contains(&"recommended") {
        data.insert("recommended".into(), json!(true));
    }
    // A required choice without a default (Ollama's model): the first one offered (Ollama lists
    // the downloaded models first).
    for x in &fields {
        let n = s(x, "name");
        if x.get("required") == Some(&json!(true))
            && x.get("default").is_none()
            && !data.contains_key(n)
            && let Some(first) = x.pointer("/selector/select/options/0")
        {
            let v = first.get("value").cloned().unwrap_or_else(|| first.clone());
            data.insert(n.into(), v);
        }
    }
    let unknown: Vec<&str> = fields
        .iter()
        .filter(|x| x.get("required") == Some(&json!(true)) && x.get("default").is_none())
        .map(|x| s(x, "name"))
        .filter(|n| !data.contains_key(*n))
        .collect();
    if !unknown.is_empty() {
        let _ = api.rest(
            "DELETE",
            &format!("/api/config/config_entries/subentries/flow/{flow_id}"),
            None,
        );
        return Err(format!("agent: unknown required fields {unknown:?}"));
    }
    let before: Vec<String> = subentries(api, ai)?;
    let (_, mut r) = api.rest(
        "POST",
        &format!("/api/config/config_entries/subentries/flow/{flow_id}"),
        Some(Value::Object(data.clone())),
    )?;
    // Some integrations ask a second form (the model settings when not recommended): defaults.
    for _ in 0..3 {
        if s(&r, "type") != "form" {
            break;
        }
        let id = s(&r, "flow_id").to_string();
        let mut d = serde_json::Map::new();
        for x in r
            .get("data_schema")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let n = s(x, "name");
            if let Some(v) = data.get(n).or(x.get("default")) {
                d.insert(n.into(), v.clone());
            }
        }
        r = api
            .rest(
                "POST",
                &format!("/api/config/config_entries/subentries/flow/{id}"),
                Some(Value::Object(d)),
            )?
            .1;
    }
    if s(&r, "type") != "create_entry" {
        return Err(format!(
            "agent: {} {}",
            s(&r, "type"),
            r.get("errors").unwrap_or(&Value::Null)
        ));
    }
    let after = subentries(api, ai)?;
    after
        .into_iter()
        .find(|x| !before.contains(x))
        .ok_or_else(|| "agent: not found after creation".into())
}

fn subentries(api: &mut dyn Api, ai: &str) -> Result<Vec<String>, String> {
    Ok(api
        .ws(json!({"type": "config_entries/subentries/list", "entry_id": ai}))?
        .as_array()
        .into_iter()
        .flatten()
        .filter(|x| s(x, "subentry_type") == "conversation")
        .map(|x| s(x, "subentry_id").to_string())
        .collect())
}

/// "Repair" on the agent: the MCP tools attached again (the rest of its settings kept).
fn add_tools(api: &mut dyn Api, ai: &str, sub: &str, mcp: &str) -> Result<(), String> {
    let (_, f) = api.rest(
        "POST",
        "/api/config/config_entries/subentries/flow",
        Some(
            json!({"handler": [ai, "conversation"], "show_advanced_options": false,
                    "subentry_id": sub, "source": "reconfigure"}),
        ),
    )?;
    let flow_id = s(&f, "flow_id").to_string();
    let mut data = serde_json::Map::new();
    for x in f
        .get("data_schema")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(v) = x
            .pointer("/description/suggested_value")
            .or(x.get("default"))
        {
            data.insert(s(x, "name").into(), v.clone());
        }
    }
    let mut apis: Vec<Value> = match data.get("llm_hass_api") {
        Some(Value::Array(a)) => a.clone(),
        Some(Value::String(v)) => vec![json!(v)],
        _ => vec![],
    };
    let ours = json!(mcp_api(mcp));
    if !apis.contains(&ours) {
        apis.push(ours);
    }
    data.insert("llm_hass_api".into(), Value::Array(apis));
    let (_, r) = api.rest(
        "POST",
        &format!("/api/config/config_entries/subentries/flow/{flow_id}"),
        Some(Value::Object(data)),
    )?;
    match s(&r, "type") {
        "abort" | "create_entry" => Ok(()),
        t => {
            let _ = api.rest(
                "DELETE",
                &format!("/api/config/config_entries/subentries/flow/{flow_id}"),
                None,
            );
            Err(format!(
                "agent tools: {t} {}",
                r.get("errors").unwrap_or(&Value::Null)
            ))
        }
    }
}

/// "Unlink": the satellite's options back as they were, then what the body made removed (the
/// pipeline, the agent, the MCP and the Wyoming integrations), newest first. Errors are logged
/// and the rest goes on.
pub fn unlink(api: &mut dyn Api, st: &State) -> Vec<String> {
    let mut errors = Vec::new();
    let mut note = |what: &str, r: Result<(), String>| {
        if let Err(e) = r {
            log::warn!(event = "ha_setup"; "unlink: {what}: {e}");
            errors.push(format!("{what}: {e}"));
        }
    };
    if !st.vad_select.is_empty() && !st.vad_before.is_empty() {
        note("vad", select(api, &st.vad_select, &st.vad_before));
    }
    if !st.satellite_select.is_empty() && !st.satellite_before.is_empty() {
        note(
            "satellite",
            select(api, &st.satellite_select, &st.satellite_before),
        );
    }
    if st.pipeline_ours && !st.pipeline.is_empty() {
        let r =
            api.ws(json!({"type": "assist_pipeline/pipeline/delete", "pipeline_id": st.pipeline}));
        note("pipeline", r.map(|_| ()));
    }
    if st.agent_ours && !st.agent_sub.is_empty() {
        let r = api.ws(
            json!({"type": "config_entries/subentries/delete", "entry_id": st.ai_entry,
                              "subentry_id": st.agent_sub}),
        );
        note("agent", r.map(|_| ()));
    }
    for (ours, id, what) in [
        (st.mcp_ours, &st.mcp_entry, "mcp"),
        (st.wyoming_ours, &st.wyoming_entry, "wyoming"),
    ] {
        if ours && !id.is_empty() {
            let r = api.rest(
                "DELETE",
                &format!("/api/config/config_entries/entry/{id}"),
                None,
            );
            note(what, r.map(|_| ()));
        }
    }
    errors
}

// ---------------------------------------------------------------------------------------------
// Home Assistant for real: one WebSocket connection per run, REST calls beside it.

pub struct HaApi {
    url: String,
    token: String,
    ws: WebSocket<std::net::TcpStream>,
    next_id: u64,
    timeout: Duration,
}

impl HaApi {
    pub fn connect(url: &str, token: &str) -> Result<HaApi, String> {
        let Endpoint { host, port, path } = endpoint(url)?;
        let timeout = Duration::from_secs(30);
        let mut ws = WebSocket::connect(&host, port, &path, Duration::from_secs(10))
            .map_err(|e| format!("{host}:{port}: {e}"))?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let msg = match ws.read().map_err(|e| e.to_string())? {
                Some(Message::Text(t)) => t,
                Some(Message::Close) => return Err("closed during authentication".into()),
                None if Instant::now() < deadline => continue,
                None => return Err("no answer to the authentication".into()),
            };
            let v: Value = serde_json::from_str(&msg).unwrap_or(Value::Null);
            match s(&v, "type") {
                "auth_required" => ws
                    .send_text(&json!({"type": "auth", "access_token": token}).to_string())
                    .map_err(|e| e.to_string())?,
                "auth_ok" => break,
                "auth_invalid" => return Err(format!("token refused: {}", s(&v, "message"))),
                _ => {}
            }
        }
        Ok(HaApi {
            url: url.to_string(),
            token: token.to_string(),
            ws,
            next_id: 1,
            timeout,
        })
    }
}

impl Drop for HaApi {
    fn drop(&mut self) {
        self.ws.close();
    }
}

impl Api for HaApi {
    fn ws(&mut self, mut msg: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        msg["id"] = json!(id);
        let what = s(&msg, "type").to_string();
        self.ws
            .send_text(&msg.to_string())
            .map_err(|e| e.to_string())?;
        let deadline = Instant::now() + self.timeout;
        loop {
            let t = match self.ws.read().map_err(|e| e.to_string())? {
                Some(Message::Text(t)) => t,
                Some(Message::Close) => return Err("Home Assistant closed the connection".into()),
                None if Instant::now() < deadline => continue,
                None => return Err(format!("{what}: no answer")),
            };
            let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
            if v.get("id").and_then(Value::as_u64) != Some(id) || s(&v, "type") != "result" {
                continue;
            }
            if v.get("success") == Some(&json!(true)) {
                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
            }
            let e = v.get("error").cloned().unwrap_or(Value::Null);
            return Err(format!("{what}: {} {}", s(&e, "code"), s(&e, "message")));
        }
    }

    fn rest(
        &mut self,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Result<(u16, Value), String> {
        let bytes = body.map(|b| b.to_string().into_bytes());
        let r = auth::http(
            &self.url,
            method,
            path,
            Some(&self.token),
            bytes.as_deref().map(|b| ("application/json", b)),
            4 << 20,
            self.timeout,
        )
        .map_err(|e| format!("{method} {path}: {e}"))?;
        let v = serde_json::from_str(&r.body).unwrap_or(Value::Null);
        if r.status >= 400 {
            let why = v
                .get("message")
                .and_then(Value::as_str)
                .map(String::from)
                .unwrap_or(r.body.chars().take(200).collect());
            return Err(format!("{method} {path}: HTTP {} {why}", r.status));
        }
        Ok((r.status, v))
    }
}

// ---------------------------------------------------------------------------------------------
// The thread: the UI asks, the rows come back.

/// What the screen asks.
#[derive(Debug, Clone, PartialEq)]
pub enum SetupCmd {
    /// Read-only (the page opened).
    Check,
    /// The first setup ("Set up") or "Check and repair".
    Repair,
    /// "Repair" on a row.
    RepairRow(String),
    /// Remove what the body made, then the link.
    Unlink,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SetupEvent {
    /// Working (true) or done (false).
    Busy(bool),
    Rows(Vec<Row>),
    /// A run ended badly: the locale key of the reason, and its error.
    Failed(String),
    /// After a check by itself: something needs the user (a home card).
    Attention(bool),
    /// Done: what the body made is gone, the link too.
    Unlinked,
}

pub struct SetupConfig {
    pub url: String,
    pub auth: Arc<Auth>,
    pub state_file: PathBuf,
    /// The body (its address is looked up when a run starts: it may change with DHCP).
    pub name: String,
    pub wyoming_port: u16,
    pub mcp_port: u16,
    pub prompt: String,
    /// The brain's status (None: it does not answer).
    pub brain: BrainFn,
    /// Home Assistant's addresses as the satellite's server lets them in.
    pub learned: Option<Learned>,
}

pub struct Setup {
    pub tx: Sender<SetupCmd>,
    pub rx: Receiver<SetupEvent>,
    _thread: JoinHandle<()>,
}

/// Starts the thread: it answers the screen, and once a day (and at start, after a minute)
/// checks by itself; at start it also reloads the MCP integration when the tools changed.
pub fn spawn(cfg: SetupConfig, stop: fn() -> bool) -> io::Result<Setup> {
    let (tx, cmd_rx) = channel();
    let (ev_tx, rx) = channel();
    let h = std::thread::Builder::new()
        .name("ha-setup".into())
        .spawn(move || run(cfg, cmd_rx, ev_tx, stop))?;
    Ok(Setup { tx, rx, _thread: h })
}

fn body_of(cfg: &SetupConfig) -> Result<Body, String> {
    let ep = endpoint(&cfg.url)?;
    let host = super::link::local_address(&ep.host, ep.port)
        .map_err(|e| format!("{}: {e}", ep.host))?
        .to_string();
    Ok(Body {
        name: cfg.name.clone(),
        host,
        wyoming_port: cfg.wyoming_port,
        mcp_port: cfg.mcp_port,
        prompt: cfg.prompt.clone(),
    })
}

fn connect(cfg: &SetupConfig) -> Result<HaApi, String> {
    let token = cfg.auth.access_token().map_err(|e| e.to_string())?;
    HaApi::connect(&cfg.url, &token)
}

fn attention(rows: &[Row]) -> bool {
    rows.iter().any(|r| matches!(r.level, Level::Bad) && r.id != "ai")
        // An AI appeared and the agent is not made yet: "Complete".
        || rows.iter().any(|r| r.id == "agent" && r.key == "ha_agent_missing")
}

fn run(cfg: SetupConfig, cmds: Receiver<SetupCmd>, ev: Sender<SetupEvent>, stop: fn() -> bool) {
    const DAY: u64 = 24 * 3600;
    let started = Instant::now();
    let mut started_check_done = false;
    loop {
        if stop() {
            return;
        }
        let cmd = match cmds.recv_timeout(Duration::from_millis(500)) {
            Ok(c) => Some(c),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
            Err(_) => return,
        };
        let mut st = State::load(&cfg.state_file);
        let Some(cmd) = cmd else {
            // By itself: a minute after the start, then once a day; only once configured.
            let due = if !started_check_done {
                started.elapsed() >= Duration::from_secs(60)
            } else {
                auth::now_secs() >= st.checked_at + DAY
            };
            if due && st.configured && cfg.auth.is_linked() {
                let first = !started_check_done;
                started_check_done = true;
                background(&cfg, &mut st, first, &ev);
            } else if due {
                started_check_done = true;
            }
            continue;
        };
        if ev.send(SetupEvent::Busy(true)).is_err() {
            return;
        }
        let brain = cfg.brain;
        let t0 = Instant::now();
        let result = (|| -> Result<(), String> {
            let body = body_of(&cfg)?;
            let mut api = connect(&cfg)?;
            if cmd != SetupCmd::Unlink && learn_addresses(&mut api, &mut st, &cfg.learned) {
                let _ = st.save(&cfg.state_file);
            }
            match &cmd {
                SetupCmd::Check => {
                    let rows = check(&mut api, &st, &body, brain);
                    let _ = ev.send(SetupEvent::Attention(st.configured && attention(&rows)));
                    let _ = ev.send(SetupEvent::Rows(rows));
                }
                SetupCmd::Repair | SetupCmd::RepairRow(_) => {
                    let scope = match &cmd {
                        SetupCmd::RepairRow(id) => Scope::Row(id.clone()),
                        _ => Scope::Missing,
                    };
                    let ev2 = ev.clone();
                    let mut progress = move |rows: &[Row]| {
                        let _ = ev2.send(SetupEvent::Rows(rows.to_vec()));
                    };
                    let rows = repair(
                        &mut api,
                        &mut st,
                        &cfg.state_file,
                        &body,
                        brain,
                        scope,
                        &mut progress,
                    )?;
                    let not_ok: Vec<&str> = rows
                        .iter()
                        .filter(|r| !matches!(r.level, Level::Ok | Level::Skipped))
                        .map(|r| r.id.as_str())
                        .collect();
                    log::info!(
                        event = "ha_setup", ms = t0.elapsed().as_millis() as u64;
                        "{cmd:?} done in {} ms: {}",
                        t0.elapsed().as_millis(),
                        if not_ok.is_empty() { "all fine".to_string() } else { format!("still to fix: {}", not_ok.join(", ")) }
                    );
                    let _ = ev.send(SetupEvent::Attention(attention(&rows)));
                }
                SetupCmd::Unlink => {
                    let errors = unlink(&mut api, &st);
                    if errors.is_empty() {
                        let _ = std::fs::remove_file(&cfg.state_file);
                    }
                    drop(api);
                    cfg.auth.unlink();
                    let _ = ev.send(SetupEvent::Unlinked);
                }
            }
            Ok(())
        })();
        if let Err(e) = result {
            log::warn!(event = "ha_setup"; "{cmd:?}: {e}");
            let key = if e == "not_admin" {
                "ha_failed_admin"
            } else {
                "ha_failed"
            };
            let _ = ev.send(SetupEvent::Failed(format!("{key}\u{1f}{e}")));
            if cmd == SetupCmd::Unlink {
                // Home Assistant away: the link goes anyway (what was made stays, listed in
                // the log), so the user is never stuck.
                cfg.auth.unlink();
                let _ = ev.send(SetupEvent::Unlinked);
            }
        }
        if ev.send(SetupEvent::Busy(false)).is_err() {
            return;
        }
    }
}

/// The check by itself: at start the tools are read again if they changed; then the rows.
fn background(cfg: &SetupConfig, st: &mut State, at_start: bool, ev: &Sender<SetupEvent>) {
    let brain = (cfg.brain)();
    let r = (|| -> Result<(), String> {
        let body = body_of(cfg)?;
        let mut api = connect(cfg)?;
        learn_addresses(&mut api, st, &cfg.learned);
        if at_start
            && let Some(b) = &brain
            && !b.tools_hash.is_empty()
            && b.tools_hash != st.tools_hash
            && !st.mcp_entry.is_empty()
        {
            api.rest(
                "POST",
                &format!("/api/config/config_entries/entry/{}/reload", st.mcp_entry),
                None,
            )?;
            st.tools_hash = b.tools_hash.clone();
            log::info!(event = "ha_setup"; "the tools changed: Home Assistant reads them again");
        }
        let rows = check(&mut api, st, &body, cfg.brain);
        let _ = ev.send(SetupEvent::Attention(attention(&rows)));
        let _ = ev.send(SetupEvent::Rows(rows));
        Ok(())
    })();
    if let Err(e) = r {
        log::info!(event = "ha_setup"; "check: {e}");
    }
    st.checked_at = auth::now_secs();
    if let Err(e) = st.save(&cfg.state_file) {
        log::warn!(event = "ha_setup"; "{}: {e}", cfg.state_file.display());
    }
}

/// The agent's instructions: the character's persona (`persona.<lang>.md`, comments dropped)
/// and how to use the tools, in the character's language (English if there is none).
pub fn agent_prompt(character_dir: &Path, language: &str) -> String {
    let tools = match language {
        "it" => include_str!("agent-prompt.it.md"),
        _ => include_str!("agent-prompt.en.md"),
    };
    let persona = [language, "en"]
        .iter()
        .find_map(|l| std::fs::read_to_string(character_dir.join(format!("persona.{l}.md"))).ok())
        .unwrap_or_default();
    let text = |t: &str| {
        t.lines()
            .filter(|l| !l.trim_start().starts_with("<!--"))
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string()
    };
    format!("{}\n\n{}", text(&persona), text(tools))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A small Home Assistant in memory: config entries, subentries, flows, the entity registry,
    /// states and pipelines, enough for the setup's calls.
    #[derive(Default)]
    struct FakeHa {
        admin: bool,
        entries: Vec<Value>,
        subs: HashMap<String, Vec<Value>>,
        registry: Vec<Value>,
        states: Vec<Value>,
        pipelines: Vec<Value>,
        preferred: String,
        flows: HashMap<String, (String, Option<String>)>,
        next: u32,
        reloads: Vec<String>,
        writes: u32,
    }

    impl FakeHa {
        fn new() -> FakeHa {
            let mut ha = FakeHa {
                admin: true,
                ..Default::default()
            };
            ha.pipelines.push(
                json!({"id": "pref", "name": "Home Assistant", "language": "it",
                "conversation_engine": DEFAULT_AGENT, "conversation_language": "it",
                "stt_engine": "stt.cloud", "stt_language": "it-IT", "tts_engine": "tts.cloud",
                "tts_language": "it-IT", "tts_voice": "v", "wake_word_entity": null,
                "wake_word_id": null, "prefer_local_intents": true}),
            );
            ha.preferred = "pref".into();
            for e in ["stt.cloud", "tts.cloud"] {
                ha.states
                    .push(json!({"entity_id": e, "state": "unknown", "attributes": {}}));
            }
            ha
        }

        fn id(&mut self) -> String {
            self.next += 1;
            format!("ID{:03}", self.next)
        }

        fn add_entity(
            &mut self,
            entry: &str,
            sub: Option<&str>,
            entity: &str,
            uid: &str,
            state: Value,
        ) {
            self.registry
                .push(json!({"entity_id": entity, "config_entry_id": entry,
                "config_subentry_id": sub, "unique_id": uid}));
            let mut st = json!({"entity_id": entity, "state": "unknown", "attributes": {}});
            if let Value::Object(o) = state {
                for (k, v) in o {
                    st[k] = v;
                }
            }
            self.states.push(st);
        }

        fn add_ai(&mut self) -> String {
            let id = self.id();
            self.entries.push(
                json!({"entry_id": id, "domain": "google_generative_ai_conversation",
                "title": "Google Gemini", "state": "loaded",
                "supported_subentry_types": {"conversation": {}, "ai_task_data": {}}}),
            );
            id
        }

        fn pipeline_names(&self) -> Vec<Value> {
            let mut o = vec![json!("preferred")];
            o.extend(self.pipelines.iter().map(|p| p["name"].clone()));
            o
        }

        fn refresh_options(&mut self) {
            let names = self.pipeline_names();
            for st in self.states.iter_mut() {
                if st["entity_id"].as_str().unwrap().ends_with("_assistant") {
                    st["attributes"]["options"] = Value::Array(names.clone());
                }
            }
        }

        fn remove_entry(&mut self, id: &str) {
            self.entries.retain(|e| e["entry_id"] != id);
            let gone: Vec<String> = self
                .registry
                .iter()
                .filter(|e| e["config_entry_id"] == id)
                .map(|e| e["entity_id"].as_str().unwrap().to_string())
                .collect();
            self.registry.retain(|e| e["config_entry_id"] != id);
            self.states
                .retain(|s| !gone.iter().any(|g| s["entity_id"] == g.as_str()));
        }

        fn current(&self, entity: &str) -> String {
            self.states
                .iter()
                .find(|s| s["entity_id"] == entity)
                .map(|s| s["state"].as_str().unwrap().to_string())
                .unwrap_or_default()
        }

        fn agent_form(&self, ai: &str, sub: Option<&str>) -> Value {
            let data = sub
                .and_then(|id| self.subs.get(ai)?.iter().find(|x| x["subentry_id"] == id))
                .map(|x| x["data"].clone())
                .unwrap_or(json!({}));
            let field = |n: &str, req: bool| {
                let mut f = json!({"name": n, "required": req});
                if let Some(v) = data.get(n) {
                    f["description"] = json!({"suggested_value": v});
                }
                f
            };
            json!([
                field("name", true),
                field("prompt", false),
                field("llm_hass_api", false),
                field("recommended", false)
            ])
        }
    }

    impl Api for FakeHa {
        fn ws(&mut self, msg: Value) -> Result<Value, String> {
            let t = msg["type"].as_str().unwrap().to_string();
            Ok(match t.as_str() {
                "auth/current_user" => json!({"is_admin": self.admin}),
                "network" => json!({"adapters": [
                    {"name": "lo", "enabled": false,
                     "ipv4": [{"address": "127.0.0.1"}], "ipv6": [{"address": "::1"}]},
                    {"name": "eth0", "enabled": true,
                     "ipv4": [{"address": "192.0.2.11"}], "ipv6": [{"address": "fe80::1"}]},
                    {"name": "wlan0", "enabled": false, "ipv4": [{"address": "198.51.100.7"}]}
                ]}),
                "assist_pipeline/pipeline/list" => {
                    json!({"pipelines": self.pipelines, "preferred_pipeline": self.preferred})
                }
                "assist_pipeline/pipeline/create" => {
                    self.writes += 1;
                    let mut p = msg.clone();
                    p.as_object_mut().unwrap().remove("type");
                    p.as_object_mut().unwrap().remove("id");
                    p["id"] = json!(self.id());
                    self.pipelines.push(p.clone());
                    self.refresh_options();
                    p
                }
                "assist_pipeline/pipeline/update" => {
                    self.writes += 1;
                    let id = msg["pipeline_id"].clone();
                    let mut p = msg.clone();
                    for k in ["type", "pipeline_id", "id"] {
                        p.as_object_mut().unwrap().remove(k);
                    }
                    p["id"] = id.clone();
                    let slot = self
                        .pipelines
                        .iter_mut()
                        .find(|x| x["id"] == id)
                        .ok_or("no pipeline")?;
                    *slot = p;
                    json!(null)
                }
                "assist_pipeline/pipeline/delete" => {
                    self.pipelines.retain(|p| p["id"] != msg["pipeline_id"]);
                    self.refresh_options();
                    json!(null)
                }
                "config/entity_registry/list" => Value::Array(self.registry.clone()),
                "get_states" => Value::Array(self.states.clone()),
                "ai_task/preferences/get" => json!({"gen_data_entity_id": null}),
                "call_service" => {
                    self.writes += 1;
                    let e = msg["service_data"]["entity_id"].clone();
                    let o = msg["service_data"]["option"].clone();
                    let st = self
                        .states
                        .iter_mut()
                        .find(|s| s["entity_id"] == e)
                        .ok_or("no entity")?;
                    st["state"] = o;
                    json!(null)
                }
                "config_entries/subentries/list" => {
                    let ai = msg["entry_id"].as_str().unwrap();
                    Value::Array(self.subs.get(ai).cloned().unwrap_or_default())
                }
                "config_entries/subentries/delete" => {
                    let (ai, sub) = (
                        msg["entry_id"].as_str().unwrap(),
                        msg["subentry_id"].clone(),
                    );
                    self.subs
                        .entry(ai.into())
                        .or_default()
                        .retain(|x| x["subentry_id"] != sub);
                    self.registry.retain(|e| e["config_subentry_id"] != sub);
                    json!(null)
                }
                _ => return Err(format!("fake: {t}")),
            })
        }

        fn rest(
            &mut self,
            method: &str,
            path: &str,
            body: Option<Value>,
        ) -> Result<(u16, Value), String> {
            let body = body.unwrap_or(Value::Null);
            let p: Vec<&str> = path
                .trim_start_matches("/api/config/config_entries/")
                .split('/')
                .collect();
            Ok((
                200,
                match (method, p.as_slice()) {
                    ("GET", ["entry"]) => Value::Array(self.entries.clone()),
                    ("POST", ["flow"]) => {
                        let id = self.id();
                        let h = body["handler"].as_str().unwrap().to_string();
                        self.flows.insert(id.clone(), (h, None));
                        json!({"type": "form", "flow_id": id})
                    }
                    ("POST", ["flow", id]) => {
                        let (h, _) = self.flows.remove(*id).ok_or("no flow")?;
                        self.writes += 1;
                        let entry = self.id();
                        match h.as_str() {
                            "wyoming" => {
                                self.entries.push(json!({"entry_id": entry, "domain": "wyoming", "title": "Alfred", "state": "loaded"}));
                                self.add_entity(
                                    &entry,
                                    None,
                                    "assist_satellite.alfred",
                                    &format!("{entry}-assist_satellite"),
                                    json!({}),
                                );
                                let names = self.pipeline_names();
                                self.add_entity(
                                    &entry,
                                    None,
                                    "select.alfred_assistant",
                                    &format!("{entry}-pipeline"),
                                    json!({"state": "preferred", "attributes": {"options": names}}),
                                );
                                self.add_entity(&entry, None, "select.alfred_vad", &format!("{entry}-vad_sensitivity"),
                                json!({"state": "default", "attributes": {"options": ["default", "aggressive", "relaxed"]}}));
                            }
                            "mcp" => {
                                assert_eq!(body["url"], "http://192.0.2.7:10701/mcp");
                                self.entries.push(json!({"entry_id": entry, "domain": "mcp", "title": "Alfred", "state": "loaded",
                                "supported_subentry_types": {}}));
                            }
                            _ => return Err("fake: handler".into()),
                        }
                        json!({"type": "create_entry", "result": {"entry_id": entry}})
                    }
                    ("DELETE", ["flow", id] | ["subentries", "flow", id]) => {
                        self.flows.remove(*id);
                        json!(null)
                    }
                    ("POST", ["entry", id, "reload"]) => {
                        self.reloads.push(id.to_string());
                        json!(null)
                    }
                    ("DELETE", ["entry", id]) => {
                        self.remove_entry(id);
                        json!(null)
                    }
                    ("POST", ["subentries", "flow"]) => {
                        let ai = body["handler"][0].as_str().unwrap().to_string();
                        let sub = body["subentry_id"].as_str().map(String::from);
                        let id = self.id();
                        let form = self.agent_form(&ai, sub.as_deref());
                        self.flows.insert(id.clone(), (ai, sub));
                        json!({"type": "form", "flow_id": id, "data_schema": form})
                    }
                    ("POST", ["subentries", "flow", id]) => {
                        let (ai, sub) = self.flows.remove(*id).ok_or("no flow")?;
                        self.writes += 1;
                        match sub {
                            Some(sub) => {
                                let x = self
                                    .subs
                                    .get_mut(&ai)
                                    .unwrap()
                                    .iter_mut()
                                    .find(|x| x["subentry_id"] == sub)
                                    .unwrap();
                                x["data"] = body;
                                json!({"type": "abort", "reason": "reconfigure_successful"})
                            }
                            None => {
                                let sub = self.id();
                                let title = body["name"].clone();
                                self.subs
                                    .entry(ai.clone())
                                    .or_default()
                                    .push(json!({"subentry_id": sub,
                                "subentry_type": "conversation", "title": title, "data": body}));
                                self.add_entity(
                                    &ai,
                                    Some(&sub),
                                    "conversation.alfred",
                                    &sub,
                                    json!({}),
                                );
                                json!({"type": "create_entry"})
                            }
                        }
                    }
                    _ => return Err(format!("fake: {method} {path}")),
                },
            ))
        }
    }

    fn body() -> Body {
        Body {
            name: "Alfred".into(),
            host: "192.0.2.7".into(),
            wyoming_port: 10700,
            mcp_port: 10701,
            prompt: "Sei Alfred.".into(),
        }
    }

    fn brain() -> Option<BrainInfo> {
        Some(BrainInfo {
            tools_listed_at: 1_800_000_000,
            tools_hash: "abc".into(),
        })
    }

    fn level(rows: &[Row], id: &str) -> (Level, String) {
        let r = rows.iter().find(|r| r.id == id).unwrap();
        (r.level, r.key.clone())
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "home-companion-setup-{name}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d.join(STATE_FILE)
    }

    fn run(ha: &mut FakeHa, st: &mut State, file: &Path, scope: Scope) -> Vec<Row> {
        repair(ha, st, file, &body(), brain, scope, &mut |_| {}).unwrap()
    }

    #[test]
    fn home_assistants_addresses_from_its_enabled_adapters() {
        let mut ha = FakeHa::default();
        assert_eq!(ha_addresses(&mut ha), ["192.0.2.11", "fe80::1"]);
        let learned: Learned = Default::default();
        let mut st = State::default();
        assert!(learn_addresses(&mut ha, &mut st, &Some(learned.clone())));
        assert_eq!(st.ha_addresses, ["192.0.2.11", "fe80::1"]);
        assert_eq!(learned.read().unwrap().len(), 2);
        assert!(!learn_addresses(&mut ha, &mut st, &Some(learned)));
    }

    #[test]
    fn a_fresh_home_assistant_without_ai_then_with_one() {
        let mut ha = FakeHa::new();
        let file = tmp("fresh");
        let mut st = State::default();
        let rows = check(&mut ha, &st, &body(), brain);
        assert_eq!(level(&rows, "wyoming").0, Level::Bad);
        assert_eq!(level(&rows, "ai").0, Level::Skipped);
        assert_eq!(ha.writes, 0, "a check writes nothing");

        let rows = run(&mut ha, &mut st, &file, Scope::Missing);
        for id in ["link", "wyoming", "mcp", "pipeline", "satellite", "vad"] {
            assert_eq!(level(&rows, id).0, Level::Ok, "{id}: {rows:?}");
        }
        assert_eq!(level(&rows, "agent").0, Level::Skipped);
        let p = ha
            .pipelines
            .iter()
            .find(|p| p["name"] == "Alfred")
            .unwrap()
            .clone();
        assert_eq!(p["conversation_engine"], DEFAULT_AGENT);
        assert_eq!(p["prefer_local_intents"], false);
        assert_eq!(
            (p["stt_engine"].as_str(), p["tts_voice"].as_str()),
            (Some("stt.cloud"), Some("v"))
        );
        assert_eq!(ha.current("select.alfred_assistant"), "Alfred");
        assert_eq!(ha.current("select.alfred_vad"), "relaxed");
        assert_eq!(State::load(&file), st, "saved as it went");
        assert_eq!(
            (st.satellite_before.as_str(), st.vad_before.as_str()),
            ("preferred", "default")
        );
        assert_eq!(st.satellite, "assist_satellite.alfred");
        assert_eq!(st.tools_hash, "abc");

        // The same again: nothing new.
        let writes = ha.writes;
        run(&mut ha, &mut st, &file, Scope::Missing);
        assert_eq!(ha.writes, writes);
        assert_eq!(ha.pipelines.len(), 2);

        // An AI arrives: the check says so, the repair makes the agent with the tools and the
        // pipeline follows it.
        let ai = ha.add_ai();
        let rows = check(&mut ha, &st, &body(), brain);
        assert_eq!(
            level(&rows, "agent"),
            (Level::Bad, "ha_agent_missing".into())
        );
        assert!(attention(&rows));
        let rows = run(&mut ha, &mut st, &file, Scope::Missing);
        assert_eq!(level(&rows, "agent").0, Level::Ok, "{rows:?}");
        let sub = &ha.subs[&ai][0];
        assert_eq!(
            sub["data"]["llm_hass_api"],
            json!([format!("mcp-{}", st.mcp_entry)])
        );
        assert_eq!(sub["data"]["prompt"], "Sei Alfred.");
        let p = ha.pipelines.iter().find(|p| p["name"] == "Alfred").unwrap();
        assert_eq!(p["conversation_engine"], "conversation.alfred");
        assert!(!attention(&rows));
    }

    #[test]
    fn the_users_choices_stay_until_their_row_is_repaired() {
        let mut ha = FakeHa::new();
        ha.add_ai();
        let file = tmp("choices");
        let mut st = State::default();
        run(&mut ha, &mut st, &file, Scope::Missing);
        // The user picks another pipeline and removes the tools from the agent.
        ha.ws(json!({"type": "call_service", "service_data": {"entity_id": "select.alfred_assistant", "option": "preferred"}})).unwrap();
        let ai = st.ai_entry.clone();
        ha.subs.get_mut(&ai).unwrap()[0]["data"]["llm_hass_api"] = json!(["assist"]);
        let rows = check(&mut ha, &st, &body(), brain);
        assert_eq!(
            level(&rows, "satellite"),
            (Level::Warn, "ha_satellite_changed_preferred".into())
        );
        assert_eq!(
            level(&rows, "agent"),
            (Level::Warn, "ha_agent_no_tools".into())
        );
        assert!(rows.iter().find(|r| r.id == "satellite").unwrap().repair);

        run(&mut ha, &mut st, &file, Scope::Missing);
        assert_eq!(ha.current("select.alfred_assistant"), "preferred", "kept");

        let rows = run(&mut ha, &mut st, &file, Scope::Row("satellite".into()));
        assert_eq!(level(&rows, "satellite").0, Level::Ok);
        let rows = run(&mut ha, &mut st, &file, Scope::Row("agent".into()));
        assert_eq!(level(&rows, "agent").0, Level::Ok);
        let apis = &ha.subs[&ai][0]["data"]["llm_hass_api"];
        assert_eq!(
            apis,
            &json!(["assist", format!("mcp-{}", st.mcp_entry)]),
            "the user's API kept"
        );
        assert_eq!(
            st.satellite_before, "preferred",
            "the first value before, not the user's later one"
        );
    }

    #[test]
    fn unlink_removes_only_what_the_body_made() {
        let mut ha = FakeHa::new();
        let ai = ha.add_ai();
        let file = tmp("unlink");
        let mut st = State::default();
        run(&mut ha, &mut st, &file, Scope::Missing);
        assert!(unlink(&mut ha, &st).is_empty());
        assert_eq!(ha.entries.len(), 1, "only the user's AI is left");
        assert_eq!(ha.entries[0]["entry_id"], ai.as_str());
        assert_eq!(ha.pipelines.len(), 1);
        assert!(ha.subs[&ai].is_empty());
    }

    #[test]
    fn something_made_by_hand_is_used_and_left_alone() {
        let mut ha = FakeHa::new();
        // Wyoming added by hand (the satellite was discovered).
        let f = ha
            .rest(
                "POST",
                "/api/config/config_entries/flow",
                Some(json!({"handler": "wyoming"})),
            )
            .unwrap()
            .1;
        let id = f["flow_id"].as_str().unwrap().to_string();
        ha.rest(
            "POST",
            &format!("/api/config/config_entries/flow/{id}"),
            Some(json!({})),
        )
        .unwrap();
        let file = tmp("byhand");
        let mut st = State::default();
        run(&mut ha, &mut st, &file, Scope::Missing);
        assert!(!st.wyoming_ours && st.mcp_ours);
        assert_eq!(
            ha.entries
                .iter()
                .filter(|e| e["domain"] == "wyoming")
                .count(),
            1
        );
        unlink(&mut ha, &st);
        assert_eq!(
            ha.entries
                .iter()
                .filter(|e| e["domain"] == "wyoming")
                .count(),
            1,
            "not ours"
        );
        assert_eq!(
            ha.current("select.alfred_assistant"),
            "preferred",
            "but put back as it was"
        );
    }

    #[test]
    fn not_an_administrator() {
        let mut ha = FakeHa::new();
        ha.admin = false;
        let file = tmp("admin");
        let mut st = State::default();
        let rows = check(&mut ha, &st, &body(), brain);
        assert_eq!(
            level(&rows, "link"),
            (Level::Bad, "ha_link_not_admin".into())
        );
        assert_eq!(
            repair(
                &mut ha,
                &mut st,
                &file,
                &body(),
                brain,
                Scope::Missing,
                &mut |_| {}
            ),
            Err("not_admin".into())
        );
        assert_eq!(ha.writes, 0);
    }

    #[test]
    fn the_agents_instructions() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../characters/alfred");
        let it = agent_prompt(&dir, "it");
        assert!(it.starts_with("Sei Alfred"), "{it}");
        assert!(it.contains("home_control") && !it.contains("<!--"));
        assert!(agent_prompt(&dir, "xx").contains("Tools:"));
    }

    #[test]
    fn rows_say_it_in_the_language() {
        let r = Row::new("mcp", Level::Ok, "ok").arg("time", "15:02");
        assert_eq!(
            r.detail(|k| format!("[{k}] read at {{time}}")),
            "[ha_mcp_ok] read at 15:02"
        );
        assert_eq!(body().mcp_url(), "http://192.0.2.7:10701/mcp");
        let b = Body {
            host: "2001:db8::7".into(),
            ..body()
        };
        assert_eq!(b.mcp_url(), "http://[2001:db8::7]:10701/mcp");
    }

    #[test]
    fn every_row_has_its_words() {
        let dir = locale::repo_locales();
        for lang in ["it", "en"] {
            let l = locale::load(&dir, lang).unwrap();
            let src = include_str!("setup.rs");
            let src = &src[..src.find("#[cfg(test)]").unwrap()];
            for id in ROWS {
                assert_ne!(
                    l.text(&format!("ha_row_{id}")),
                    format!("ha_row_{id}"),
                    "{lang} ha_row_{id}"
                );
            }
            // Every Row::new("id", Level::…, "what") in this file, the pipeline's base(…, "what")
            // and the satellite's own options.
            let mut keys: Vec<String> = Vec::new();
            for cap in src.split("Row::new(\"").skip(1) {
                let call = cap.split(')').next().unwrap();
                let parts: Vec<&str> = call.split(", ").collect();
                if let [id, lvl, what] = parts.as_slice()
                    && ROWS.contains(&id.trim_end_matches('"'))
                    && lvl.starts_with("Level::")
                    && what.starts_with('"')
                {
                    keys.push(format!(
                        "ha_{}_{}",
                        id.trim_end_matches('"'),
                        what.trim_matches('"')
                    ));
                }
            }
            for cap in src.split("base(Level::").skip(1) {
                let what = cap.split('"').nth(1).unwrap();
                keys.push(format!("ha_pipeline_{what}"));
            }
            for cap in src.split("=> \"changed_").skip(1) {
                keys.push(format!(
                    "ha_satellite_changed_{}",
                    cap.split('"').next().unwrap()
                ));
            }
            keys.retain(|k| {
                !k.starts_with("ha_satellite_changed_d") && !k.starts_with("ha_satellite_changed_a")
            });
            keys.extend(
                [
                    "ha_vad_changed_default",
                    "ha_vad_changed_aggressive",
                    "ha_vad_changed",
                ]
                .map(String::from),
            );
            assert!(keys.len() > 30, "{keys:?}");
            for key in keys {
                assert_ne!(l.text(&key), key, "{lang}: {key}");
            }
        }
    }
}
