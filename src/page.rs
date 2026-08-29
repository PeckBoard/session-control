//! The Orchestrators management page: the public HTML shell (served on
//! `GET /plugin-api/v1/orchestrators` via `http.request.before`) and the
//! authenticated JSON routes under `/api/plugin-ui/session-control/*`
//! (served via `http.request.authed`). The page runs in a sandboxed iframe
//! and reaches the JSON routes through the parent `plugin-ui-fetch`
//! postMessage bridge; it polls every 5s, so state stays live without a
//! websocket.

use serde_json::{Value, json};

use crate::engine;
use crate::host::{HostFn, call_host};
use crate::state::{self, Orchestrator};

pub const PAGE_PATH: &str = "/plugin-api/v1/orchestrators";
const API_PREFIX: &str = "/api/plugin-ui/session-control";

// ── Public page (http.request.before) ─────────────────────────────────

pub fn serve_public(payload: Value) -> Result<Value, String> {
    let path = payload.get("path").and_then(|v| v.as_str()).unwrap_or("");
    if path == PAGE_PATH {
        Ok(json!({
            "status": 200,
            "headers": { "content-type": "text/html; charset=utf-8" },
            "body": PAGE_HTML,
        }))
    } else {
        Ok(json!({
            "status": 404,
            "headers": { "content-type": "text/html; charset=utf-8" },
            "body": "<!doctype html><h1>404</h1>",
        }))
    }
}

// ── Authed JSON routes (http.request.authed) ──────────────────────────

fn json_response(status: u16, body: Value) -> Value {
    json!({
        "status": status,
        "headers": { "content-type": "application/json" },
        "body": body.to_string(),
    })
}

fn ok_json(body: Value) -> Value {
    json_response(200, body)
}

fn err_json(status: u16, msg: &str) -> Value {
    json_response(status, json!({ "error": msg }))
}

pub fn serve_authed(payload: Value) -> Result<Value, String> {
    let method = payload
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("GET")
        .to_uppercase();
    let path = payload.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let body: Value = payload
        .get("body")
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(Value::Null);

    let rest = path.strip_prefix(API_PREFIX).unwrap_or("");
    let segs: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();

    let out = match (method.as_str(), segs.as_slice()) {
        ("GET", ["orchestrators"]) => list_route(),
        ("POST", ["orchestrators"]) => create_route(&body),
        ("POST", ["orchestrators", id]) => update_route(id, &body),
        ("POST", ["orchestrators", id, "delete"]) => delete_route(id),
        ("POST", ["orchestrators", id, "run"]) => engine::run_now(id).map(ok_json),
        ("POST", ["orchestrators", id, "dry-run"]) => engine::dry_run(id).map(ok_json),
        ("POST", ["orchestrators", id, "pause"]) => pause_route(id, &body),
        ("POST", ["pause-all"]) => {
            let paused = body.get("paused").and_then(|v| v.as_bool()).unwrap_or(true);
            state::set_global_paused(paused);
            Ok(ok_json(json!({ "ok": true, "paused": paused })))
        }
        ("GET", ["pickers"]) => pickers_route(),
        _ => Ok(err_json(404, "no such route")),
    };
    // Route-level failures become a 400 body, not a broken dispatch.
    Ok(out.unwrap_or_else(|e| err_json(400, &e)))
}

/// List with computed per-orchestrator liveness fields the page shows.
fn list_route() -> Result<Value, String> {
    let now = state::clock().unwrap_or_default();
    let orchs: Vec<Value> = state::list_orchestrators()?
        .into_iter()
        .map(|o| {
            let brain_busy = o
                .session_id
                .as_deref()
                .map(|s| !now.is_empty() && state::is_busy(s, &now))
                .unwrap_or(false);
            let mut v = serde_json::to_value(&o).unwrap_or(Value::Null);
            if let Some(map) = v.as_object_mut() {
                map.insert("brain_busy".into(), json!(brain_busy));
            }
            v
        })
        .collect();
    Ok(ok_json(json!({
        "orchestrators": orchs,
        "global_paused": state::global_paused(),
        "clock": now,
    })))
}

/// Fields the page may set on create/update.
fn apply_fields(o: &mut Orchestrator, body: &Value) -> Result<(), String> {
    if let Some(v) = body.get("name").and_then(|v| v.as_str()) {
        if v.trim().is_empty() {
            return Err("name must not be blank".into());
        }
        o.name = v.trim().to_string();
    }
    if let Some(v) = body.get("goal").and_then(|v| v.as_str()) {
        o.goal = v.to_string();
    }
    if let Some(v) = body.get("prompt").and_then(|v| v.as_str()) {
        o.prompt = v.to_string();
    }
    if let Some(v) = body.get("folder_id").and_then(|v| v.as_str()) {
        if v.trim().is_empty() {
            return Err("folder_id must not be blank".into());
        }
        o.folder_id = v.trim().to_string();
    }
    if let Some(v) = body.get("model") {
        o.model = v.as_str().filter(|s| !s.is_empty()).map(str::to_string);
    }
    if let Some(v) = body.get("enabled").and_then(|v| v.as_bool()) {
        o.enabled = v;
        if v {
            // Re-enabling clears the backoff state.
            o.consecutive_failures = 0;
            o.error = None;
        }
    }
    if let Some(v) = body.get("every_minutes") {
        o.schedule.every_minutes = v.as_u64().filter(|m| *m > 0);
        o.stats.next_due_at = None;
    }
    if let Some(v) = body.get("watchdog_minutes").and_then(|v| v.as_u64()) {
        o.watchdog_minutes = v;
    }
    if let Some(v) = body.get("cooldown_secs").and_then(|v| v.as_u64()) {
        o.cooldown_secs = v;
    }
    if let Some(v) = body.get("max_fires_per_hour").and_then(|v| v.as_u64()) {
        o.caps.max_fires_per_hour = v.max(1) as u32;
    }
    if let Some(v) = body.get("max_sessions_created").and_then(|v| v.as_u64()) {
        o.caps.max_sessions_created = v as u32;
    }
    if let Some(v) = body.get("watch_sessions").and_then(|v| v.as_array()) {
        o.watch.sessions = v
            .iter()
            .filter_map(|s| s.as_str())
            .map(str::to_string)
            .collect();
    }
    if let Some(v) = body.get("watch_folders").and_then(|v| v.as_array()) {
        o.watch.folders = v
            .iter()
            .filter_map(|s| s.as_str())
            .map(str::to_string)
            .collect();
    }
    if let Some(v) = body.get("auto_watch_created").and_then(|v| v.as_bool()) {
        o.watch.auto_watch_created = v;
    }
    if let Some(s) = body.get("standards").and_then(|v| v.as_object()) {
        if let Some(b) = s.get("dev").and_then(|v| v.as_bool()) {
            o.standards.dev = b;
        }
        if let Some(b) = s.get("testing").and_then(|v| v.as_bool()) {
            o.standards.testing = b;
        }
        if let Some(b) = s.get("ux").and_then(|v| v.as_bool()) {
            o.standards.ux = b;
        }
        if let Some(c) = s.get("custom").and_then(|v| v.as_str()) {
            o.standards.custom = c.to_string();
        }
    }
    Ok(())
}

fn create_route(body: &Value) -> Result<Value, String> {
    let now = state::clock().unwrap_or_else(|| "1970-01-01T00:00:00Z".into());
    state::try_with_engine_lock(|| {
        // Under the lease the existence check makes the id race-free even
        // when another instance created one in the same clock second.
        let mut id = new_id();
        while state::load_orchestrator(&id)?.is_some() {
            id = new_id();
        }
        let mut o: Orchestrator = serde_json::from_value(json!({
            "id": id,
            "name": "",
            "folder_id": "",
            "created_at": now,
        }))
        .map_err(|e| e.to_string())?;
        apply_fields(&mut o, body)?;
        if o.name.is_empty() {
            return Err("name is required".into());
        }
        if o.folder_id.is_empty() {
            return Err("folder_id is required".into());
        }
        o.push_log(&now, "report", "orchestrator created");
        state::save_orchestrator(&o)?;
        Ok(ok_json(json!({ "ok": true, "id": o.id })))
    })?
    .ok_or_else(|| state::BUSY_MSG.to_string())
}

fn update_route(id: &str, body: &Value) -> Result<Value, String> {
    state::try_with_engine_lock(|| {
        let mut o = state::load_orchestrator(id)?.ok_or(format!("orchestrator not found: {id}"))?;
        apply_fields(&mut o, body)?;
        state::save_orchestrator(&o)?;
        // Keep an existing brain's standing prompt in sync with edited
        // goal/standards/name — takes effect on its next turn. Best-effort.
        if let Some(sid) = &o.session_id {
            let _ = call_host(
                HostFn::OrchestrateSetPrompt,
                &json!({ "session_id": sid, "system_prompt": engine::standing_prompt(&o) }),
            );
        }
        Ok(ok_json(json!({ "ok": true })))
    })?
    .ok_or_else(|| state::BUSY_MSG.to_string())
}

fn delete_route(id: &str) -> Result<Value, String> {
    state::try_with_engine_lock(|| {
        state::store_delete(state::ORCH_COLLECTION, id)?;
        Ok(ok_json(json!({ "ok": true })))
    })?
    .ok_or_else(|| state::BUSY_MSG.to_string())
}

fn pause_route(id: &str, body: &Value) -> Result<Value, String> {
    state::try_with_engine_lock(|| {
        let mut o = state::load_orchestrator(id)?.ok_or(format!("orchestrator not found: {id}"))?;
        o.paused = body
            .get("paused")
            .and_then(|v| v.as_bool())
            .unwrap_or(!o.paused);
        state::save_orchestrator(&o)?;
        Ok(ok_json(json!({ "ok": true, "paused": o.paused })))
    })?
    .ok_or_else(|| state::BUSY_MSG.to_string())
}

/// Dropdown data: sessions (folder-blind), folders, models. Models come from
/// the thinking-model catalog (`models_read`); failures degrade to empty
/// lists so one missing grant never blanks the page.
fn pickers_route() -> Result<Value, String> {
    let sessions = call_host(HostFn::ListSessions, &json!({ "query": "" }))
        .ok()
        .and_then(|v| v.get("sessions").cloned())
        .unwrap_or(json!([]));
    let folders = call_host(HostFn::ListFolders, &json!({}))
        .ok()
        .and_then(|v| v.get("folders").cloned())
        .unwrap_or(json!([]));
    let models = call_host(HostFn::ListModels, &json!({}))
        .ok()
        .and_then(|v| v.get("models").cloned())
        .unwrap_or(json!([]));
    Ok(ok_json(json!({
        "sessions": sessions, "folders": folders, "models": models,
    })))
}

/// Monotonic-enough id: an atomic counter plus the stored clock second.
fn new_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let ts = state::clock().unwrap_or_default();
    let compact: String = ts.chars().filter(|c| c.is_ascii_digit()).collect();
    format!("orch-{compact}-{n}")
}

// ── The page itself ───────────────────────────────────────────────────

const PAGE_HTML: &str = r##"<!doctype html>
<html>
<head>
<meta charset="utf-8">
<title>Orchestrators</title>
<style>
  :root {
    color-scheme: light dark;
    --bg: #f5f6f8; --card: #ffffff; --text: #1c1e21; --muted: #667085;
    --line: #e4e7ec; --accent: #4f6bed; --ok: #12805c; --warn: #b54708; --bad: #b42318;
  }
  @media (prefers-color-scheme: dark) {
    :root { --bg: #101418; --card: #1a2027; --text: #e6e9ee; --muted: #98a2b3;
            --line: #2c3540; --accent: #7c93f5; --ok: #3ccb9a; --warn: #f7b26a; --bad: #f97066; }
  }
  * { box-sizing: border-box; }
  body { margin: 0; padding: 16px; background: var(--bg); color: var(--text);
         font: 14px/1.45 system-ui, sans-serif; }
  h1 { font-size: 18px; margin: 0; }
  .topbar { display: flex; align-items: center; gap: 10px; margin-bottom: 14px; }
  .topbar .spacer { flex: 1; }
  button { font: inherit; padding: 6px 12px; border-radius: 8px; border: 1px solid var(--line);
           background: var(--card); color: var(--text); cursor: pointer; }
  button.primary { background: var(--accent); border-color: var(--accent); color: #fff; }
  button.danger { color: var(--bad); }
  button:disabled { opacity: .5; cursor: default; }
  .card { background: var(--card); border: 1px solid var(--line); border-radius: 12px;
          padding: 14px; margin-bottom: 12px; }
  .row { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .chip { display: inline-block; padding: 1px 9px; border-radius: 999px; font-size: 12px;
          border: 1px solid var(--line); color: var(--muted); }
  .chip.ok { color: var(--ok); border-color: var(--ok); }
  .chip.warn { color: var(--warn); border-color: var(--warn); }
  .chip.bad { color: var(--bad); border-color: var(--bad); }
  .muted { color: var(--muted); font-size: 12px; }
  .stats { display: flex; gap: 16px; margin: 8px 0; flex-wrap: wrap; }
  .stat b { font-size: 16px; }
  .bar { height: 6px; background: var(--line); border-radius: 4px; overflow: hidden; margin: 6px 0; }
  .bar > div { height: 100%; background: var(--accent); }
  details { margin-top: 8px; }
  summary { cursor: pointer; color: var(--muted); }
  .feed { max-height: 240px; overflow: auto; margin: 6px 0 0; padding: 0; list-style: none; }
  .feed li { padding: 3px 0; border-bottom: 1px dashed var(--line); font-size: 12px; }
  .feed .k { display: inline-block; min-width: 110px; color: var(--muted); }
  label { display: block; margin: 8px 0 2px; font-size: 12px; color: var(--muted); }
  input[type=text], input[type=number], textarea, select {
    width: 100%; padding: 6px 8px; border: 1px solid var(--line); border-radius: 8px;
    background: var(--bg); color: var(--text); font: inherit; }
  textarea { min-height: 64px; resize: vertical; }
  .grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(160px, 1fr)); gap: 8px; }
  .checks { display: flex; gap: 14px; margin-top: 6px; flex-wrap: wrap; }
  .checks label { display: flex; align-items: center; gap: 5px; margin: 0; font-size: 13px; color: var(--text); }
  .error-banner { color: var(--bad); margin: 8px 0; }
  .empty { text-align: center; color: var(--muted); padding: 40px 0; }
  pre.prompt { white-space: pre-wrap; background: var(--bg); border: 1px solid var(--line);
               border-radius: 8px; padding: 8px; font-size: 12px; max-height: 260px; overflow: auto; }
  .watchbox { max-height: 140px; overflow: auto; border: 1px solid var(--line); border-radius: 8px;
              padding: 6px; }
  .watchbox label { display: flex; gap: 6px; align-items: center; margin: 2px 0; color: var(--text);
                    font-size: 13px; }
</style>
</head>
<body>
<div class="topbar">
  <h1>Orchestrators</h1>
  <span id="global-state" class="chip"></span>
  <span class="spacer"></span>
  <button id="pause-all"></button>
  <button class="primary" id="new-btn" data-testid="orch-new">New orchestrator</button>
</div>
<div id="banner" class="error-banner" style="display:none"></div>
<div id="editor" class="card" style="display:none"></div>
<div id="list"></div>

<script>
"use strict";
let seq = 1;
const pending = {};
window.addEventListener("message", (e) => {
  const m = e.data;
  if (!m || m.type !== "plugin-ui-fetch-result") return;
  const cb = pending[m.requestId];
  if (!cb) return;
  delete pending[m.requestId];
  cb(m);
});
function api(method, path, body) {
  return new Promise((resolve, reject) => {
    const id = seq++;
    pending[id] = (m) => {
      let data = null;
      try { data = JSON.parse(m.body); } catch (_) {}
      if (m.status >= 200 && m.status < 300 && !(data && data.error)) resolve(data);
      else reject(new Error((data && data.error) || ("HTTP " + m.status)));
    };
    parent.postMessage({
      type: "plugin-ui-fetch", requestId: id, method, path,
      body: body === undefined ? undefined : JSON.stringify(body),
    }, "*");
  });
}
const BASE = "/api/plugin-ui/session-control";
const esc = (s) => String(s == null ? "" : s).replace(/[&<>"']/g,
  (c) => ({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"}[c]));

// Prompt presets: picking one fills Goal + Trigger prompt; both stay editable.
const PRESETS = [
  { id: "project-def", label: "Project definition → cards, then implement",
    goal: "Implement every feature in the project definition.\n" +
      "1. Read PROJECT_DEFINITION.md at the root of this folder's repository (read_file). If it does not exist, call update_goal_status with state=blocked and a note saying so.\n" +
      "2. Find the project for this folder (list_projects) or create one (create_project).\n" +
      "3. Create one card per feature/requirement in the definition (create_card) — check list_cards first and never duplicate an existing card. Give each card a clear title and a description with acceptance criteria taken from the definition.\n" +
      "4. Drive every card to done: create a session per work area with a fitting hat, review results when sessions go idle, and move_card_to_done only when the work meets the standards.\n" +
      "Done when every feature in PROJECT_DEFINITION.md has a card and every card is done.",
    prompt: "Trigger: {{trigger}}. Compare PROJECT_DEFINITION.md against the project's cards: features missing a card, cards in flight, cards finished. Act on the next thing that needs you, then report via orchestrator_report + update_goal_status." },
  { id: "backlog", label: "Drive the card backlog to done",
    goal: "Drive this folder's existing card backlog to completion.\n" +
      "Enumerate open cards with list_cards, order them by priority and dependencies (list_card_dependencies), delegate each to a session you create with a fitting hat, verify results against the standards, and move finished cards with move_card_to_done.\n" +
      "Done when no open cards remain.",
    prompt: "" },
  { id: "green", label: "Keep the build green (standing watch)",
    goal: "Keep this repository healthy.\n" +
      "On every engagement, have a session run the project's verification (build, lint, tests — use its verify script when it has one), delegate fixes for anything that fails, and re-verify.\n" +
      "This is a standing watch: report status with update_goal_status but never set state=done.",
    prompt: "" },
];

let DATA = { orchestrators: [], global_paused: false, clock: "" };
let PICKERS = { sessions: [], folders: [], models: [] };
let editing = null; // null = closed, "" = new, "<id>" = edit

function banner(msg) {
  const el = document.getElementById("banner");
  el.style.display = msg ? "" : "none";
  el.textContent = msg || "";
}

function fmtEta(gs) {
  if (!gs || !gs.eta) return "no estimate yet";
  const m = gs.eta.minutes_remaining;
  const h = Math.floor(m / 60), r = m % 60;
  const span = h ? h + "h " + r + "m" : r + "m";
  return "~" + span + " → " + (gs.eta.projected_at || "").replace("T", " ").slice(0, 16);
}
function driftArrow(o) {
  const h = o.eta_history || [];
  if (h.length < 2) return "";
  const prev = h[h.length - 2], cur = h[h.length - 1];
  // Slipping when the new estimate did not shrink by the elapsed time.
  if (cur.minutes_remaining > prev.minutes_remaining) return " ↗ slipping";
  if (cur.minutes_remaining < prev.minutes_remaining) return " ↘ ahead";
  return " → holding";
}
function stateChip(o) {
  if (!o.enabled) return ["disabled" + (o.error ? " (backoff)" : ""), "bad"];
  if (o.paused) return ["paused", "warn"];
  if (o.goal_status && o.goal_status.state === "done") return ["done", "ok"];
  if (o.brain_busy) return ["brain running", "ok"];
  return ["idle", ""];
}

function render() {
  document.getElementById("global-state").textContent =
    DATA.global_paused ? "ALL PAUSED" : (DATA.orchestrators.length + " orchestrator(s)");
  document.getElementById("global-state").className =
    "chip" + (DATA.global_paused ? " bad" : "");
  document.getElementById("pause-all").textContent =
    DATA.global_paused ? "Resume all" : "Pause all";

  const list = document.getElementById("list");
  if (!DATA.orchestrators.length && editing === null) {
    list.innerHTML = '<div class="empty">No orchestrators yet. Create one to drive a goal autonomously.</div>';
    return;
  }
  list.innerHTML = DATA.orchestrators.map((o) => {
    const [chipTxt, chipCls] = stateChip(o);
    const gs = o.goal_status || {};
    const pct = gs.percent == null ? null : gs.percent;
    const hats = Object.entries(o.hats || {});
    const feed = (o.log || []).slice().reverse();
    const managed = (o.watch && o.watch.sessions || []).concat(o.created_sessions || []);
    return '<div class="card" data-testid="orch-card" data-orch="' + esc(o.id) + '">' +
      '<div class="row">' +
        '<b>' + esc(o.name) + '</b>' +
        '<span class="chip ' + chipCls + '" data-testid="orch-state">' + esc(chipTxt) + '</span>' +
        (o.error ? '<span class="chip bad" title="' + esc(o.error) + '">error</span>' : "") +
        '<span class="spacer" style="flex:1"></span>' +
        '<button data-act="run" data-id="' + esc(o.id) + '" data-testid="orch-run">Run now</button>' +
        '<button data-act="dry" data-id="' + esc(o.id) + '">Dry run</button>' +
        '<button data-act="pause" data-id="' + esc(o.id) + '">' + (o.paused ? "Resume" : "Pause") + '</button>' +
        '<button data-act="edit" data-id="' + esc(o.id) + '">Edit</button>' +
        '<button data-act="del" data-id="' + esc(o.id) + '" class="danger">Delete</button>' +
      '</div>' +
      '<div class="muted">' + esc(o.goal || "(no goal)") + '</div>' +
      (pct != null ? '<div class="bar"><div style="width:' + pct + '%"></div></div>' : "") +
      '<div class="stats">' +
        '<span class="stat"><b data-testid="orch-actions">' + (o.stats && o.stats.actions || 0) + '</b> <span class="muted">actions</span></span>' +
        '<span class="stat"><b>' + (o.stats && o.stats.fires || 0) + '</b> <span class="muted">fires</span></span>' +
        '<span class="stat"><b>' + ((o.pending_triggers || []).length) + '</b> <span class="muted">pending</span></span>' +
        '<span class="stat"><span class="muted">goal:</span> ' + esc(gs.state || "in_progress") +
          (pct != null ? " (" + pct + "%)" : "") + '</span>' +
        '<span class="stat" data-testid="orch-eta"><span class="muted">ETA:</span> ' + esc(fmtEta(gs)) + esc(driftArrow(o)) + '</span>' +
      '</div>' +
      '<div class="muted">' +
        'brain: ' + (o.session_id ? esc(o.session_id) : "(created on first fire)") +
        (o.stats && o.stats.last_fired_at ? " · last fired " + esc(o.stats.last_fired_at.replace("T", " ").slice(0, 16)) : "") +
        (o.stats && o.stats.next_due_at ? " · next " + esc(o.stats.next_due_at.replace("T", " ").slice(0, 16)) : "") +
        (gs.note ? " · " + esc(gs.note) : "") +
      '</div>' +
      '<div id="dry-' + esc(o.id) + '"></div>' +
      '<details><summary>Activity (' + feed.length + ')</summary><ul class="feed" data-testid="orch-feed">' +
        feed.map((e) => '<li><span class="k">' + esc((e.ts || "").replace("T", " ").slice(0, 19)) +
          '</span><span class="k">' + esc(e.kind) + '</span>' + esc(e.detail) + '</li>').join("") +
      '</ul></details>' +
      (managed.length || hats.length ?
        '<details><summary>Managed sessions (' + managed.length + ')</summary><ul class="feed">' +
          managed.map((s) => {
            const h = (o.hats || {})[s];
            return '<li><span class="k">' + esc(s) + '</span>' +
              (h ? 'hat: ' + esc(h.hat) + ' — ' + esc(h.responsibilities) : '<span class="muted">no hat</span>') + '</li>';
          }).join("") + '</ul></details>' : "") +
    '</div>';
  }).join("");
}

function editorHtml(o) {
  const sess = PICKERS.sessions.filter((s) => !s.is_worker);
  const watchSet = new Set((o.watch && o.watch.sessions) || []);
  return '<h2 style="margin:0 0 4px;font-size:15px">' + (o.id ? "Edit" : "New") + ' orchestrator</h2>' +
    '<label>Name</label><input type="text" id="f-name" data-testid="orch-name" value="' + esc(o.name || "") + '">' +
    '<label>Preset — fills Goal + Trigger prompt below (both stay editable)</label>' +
    '<select id="f-preset" data-testid="orch-preset"><option value="">— none —</option>' +
      PRESETS.map((p) => '<option value="' + esc(p.id) + '">' + esc(p.label) + '</option>').join("") +
    '</select>' +
    '<label>Goal — the requirements to drive to completion</label>' +
    '<textarea id="f-goal" data-testid="orch-goal">' + esc(o.goal || "") + '</textarea>' +
    '<label>Trigger prompt (optional; vars: {{trigger}} {{goal}} {{eta}} {{pending}} {{standards}} {{session_name}} {{outcome}} {{watched_busy_count}})</label>' +
    '<textarea id="f-prompt">' + esc(o.prompt || "") + '</textarea>' +
    '<div class="grid">' +
      '<div><label>Folder (brain lives here)</label><select id="f-folder" data-testid="orch-folder">' +
        '<option value="">— choose —</option>' +
        PICKERS.folders.map((f) => '<option value="' + esc(f.id) + '"' +
          (o.folder_id === f.id ? " selected" : "") + '>' + esc(f.name) + '</option>').join("") +
      '</select></div>' +
      '<div><label>Model</label><select id="f-model"><option value="">default</option>' +
        PICKERS.models.map((m) => { const id = m.id || m.model_id || ""; return '<option value="' + esc(id) + '"' +
          (o.model === id ? " selected" : "") + '>' + esc(m.display_name || id) + '</option>'; }).join("") +
      '</select></div>' +
      '<div><label>Schedule: every N minutes (blank = off)</label>' +
        '<input type="number" id="f-every" min="1" value="' + (o.schedule && o.schedule.every_minutes || "") + '"></div>' +
      '<div><label>Watchdog minutes (0 = off)</label>' +
        '<input type="number" id="f-watchdog" min="0" value="' + (o.watchdog_minutes != null ? o.watchdog_minutes : 15) + '"></div>' +
      '<div><label>Cooldown seconds</label>' +
        '<input type="number" id="f-cooldown" min="0" value="' + (o.cooldown_secs != null ? o.cooldown_secs : 60) + '"></div>' +
      '<div><label>Max fires / hour</label>' +
        '<input type="number" id="f-capfires" min="1" value="' + (o.caps && o.caps.max_fires_per_hour || 12) + '"></div>' +
      '<div><label>Max sessions created</label>' +
        '<input type="number" id="f-capsess" min="0" value="' + (o.caps && o.caps.max_sessions_created || 10) + '"></div>' +
    '</div>' +
    '<label>Standards the brain must enforce</label>' +
    '<div class="checks">' +
      '<label><input type="checkbox" id="f-std-dev"' + ((o.standards || {}).dev !== false ? " checked" : "") + '> development</label>' +
      '<label><input type="checkbox" id="f-std-testing"' + ((o.standards || {}).testing !== false ? " checked" : "") + '> testing (unit + e2e)</label>' +
      '<label><input type="checkbox" id="f-std-ux"' + ((o.standards || {}).ux !== false ? " checked" : "") + '> UX (browser + ui-gauge)</label>' +
    '</div>' +
    '<label>Custom standards (optional)</label>' +
    '<input type="text" id="f-std-custom" value="' + esc((o.standards || {}).custom || "") + '">' +
    '<label>Watch sessions (fire when one goes idle)</label>' +
    '<div class="watchbox">' + (sess.length ? sess.map((s) =>
      '<label><input type="checkbox" class="f-watch" value="' + esc(s.session_id) + '"' +
        (watchSet.has(s.session_id) ? " checked" : "") + '> ' + esc(s.name) +
        ' <span class="muted">' + esc(s.session_id) + '</span></label>').join("") :
      '<span class="muted">no sessions found</span>') + '</div>' +
    '<div class="row" style="margin-top:12px">' +
      '<button class="primary" id="f-save" data-testid="orch-save">Save</button>' +
      '<button id="f-cancel">Cancel</button>' +
    '</div>';
}

async function loadPickers() {
  PICKERS = await api("GET", BASE + "/pickers");
}
async function openEditor(o) {
  editing = o.id || "";
  const ed = document.getElementById("editor");
  ed.style.display = "";
  ed.innerHTML = '<div class="muted">Loading folders, models, and sessions…</div>';
  // ALWAYS fetch pickers right before rendering: a slow or failed boot-time
  // fetch must never leave the dropdowns permanently empty, and any failure
  // is surfaced instead of swallowed.
  try { banner(""); await loadPickers(); }
  catch (e) { banner("Failed to load folder/model/session lists: " + e.message); }
  ed.innerHTML = editorHtml(o);
  document.getElementById("f-preset").onchange = (ev) => {
    const p = PRESETS.find((x) => x.id === ev.target.value);
    if (!p) return;
    document.getElementById("f-goal").value = p.goal;
    document.getElementById("f-prompt").value = p.prompt;
  };
  document.getElementById("f-cancel").onclick = () => { editing = null; ed.style.display = "none"; };
  document.getElementById("f-save").onclick = async () => {
    const num = (id) => { const v = document.getElementById(id).value; return v === "" ? null : Number(v); };
    const body = {
      name: document.getElementById("f-name").value,
      goal: document.getElementById("f-goal").value,
      prompt: document.getElementById("f-prompt").value,
      folder_id: document.getElementById("f-folder").value,
      model: document.getElementById("f-model").value,
      every_minutes: num("f-every"),
      watchdog_minutes: num("f-watchdog") ?? 15,
      cooldown_secs: num("f-cooldown") ?? 60,
      max_fires_per_hour: num("f-capfires") ?? 12,
      max_sessions_created: num("f-capsess") ?? 10,
      standards: {
        dev: document.getElementById("f-std-dev").checked,
        testing: document.getElementById("f-std-testing").checked,
        ux: document.getElementById("f-std-ux").checked,
        custom: document.getElementById("f-std-custom").value,
      },
      watch_sessions: Array.from(document.querySelectorAll(".f-watch:checked")).map((el) => el.value),
    };
    try {
      banner("");
      if (editing) await api("POST", BASE + "/orchestrators/" + editing, body);
      else await api("POST", BASE + "/orchestrators", body);
      editing = null;
      ed.style.display = "none";
      await refresh();
    } catch (e) { banner(e.message); }
  };
}

document.getElementById("new-btn").onclick = () => openEditor({});
document.getElementById("pause-all").onclick = async () => {
  try { await api("POST", BASE + "/pause-all", { paused: !DATA.global_paused }); await refresh(); }
  catch (e) { banner(e.message); }
};
document.getElementById("list").addEventListener("click", async (ev) => {
  const btn = ev.target.closest("button[data-act]");
  if (!btn) return;
  const id = btn.dataset.id, act = btn.dataset.act;
  const o = DATA.orchestrators.find((x) => x.id === id);
  try {
    banner("");
    if (act === "run") await api("POST", BASE + "/orchestrators/" + id + "/run");
    else if (act === "pause") await api("POST", BASE + "/orchestrators/" + id + "/pause", { paused: !o.paused });
    else if (act === "del") {
      if (!confirm("Delete orchestrator \"" + o.name + "\"? Its brain session stays.")) return;
      await api("POST", BASE + "/orchestrators/" + id + "/delete");
    } else if (act === "edit") { openEditor(o); return; }
    else if (act === "dry") {
      const r = await api("POST", BASE + "/orchestrators/" + id + "/dry-run");
      document.getElementById("dry-" + id).innerHTML =
        '<pre class="prompt" data-testid="orch-dryrun">' + esc(r.rendered_prompt) + '</pre>';
      return;
    }
    await refresh();
  } catch (e) { banner(e.message); }
});

async function refresh() {
  try {
    DATA = await api("GET", BASE + "/orchestrators");
    render();
  } catch (e) { banner(e.message); }
}
async function boot() {
  try { await loadPickers(); }
  catch (e) { banner("Failed to load folder/model/session lists: " + e.message); }
  await refresh();
  setInterval(refresh, 5000);
  // Keep pickers fresh; retry fast while they are still empty.
  setInterval(async () => {
    try { await loadPickers(); } catch (_) {}
  }, 30000);
  setInterval(async () => {
    if (!PICKERS.folders.length) { try { await loadPickers(); } catch (_) {} }
  }, 5000);
}
boot();
</script>
</body>
</html>
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_fields_validates_and_sets() {
        let mut o: Orchestrator = serde_json::from_value(json!({
            "id": "o", "name": "n", "folder_id": "f"
        }))
        .unwrap();
        assert!(apply_fields(&mut o, &json!({ "name": "  " })).is_err());
        apply_fields(
            &mut o,
            &json!({
                "name": "Ship", "goal": "G", "every_minutes": 30,
                "standards": { "ux": false, "custom": "no TODOs" },
                "watch_sessions": ["s1", "s2"],
                "max_fires_per_hour": 0,
            }),
        )
        .unwrap();
        assert_eq!(o.name, "Ship");
        assert_eq!(o.schedule.every_minutes, Some(30));
        assert!(!o.standards.ux);
        assert_eq!(o.standards.custom, "no TODOs");
        assert_eq!(o.watch.sessions, vec!["s1", "s2"]);
        assert_eq!(o.caps.max_fires_per_hour, 1, "cap floors at 1");
    }

    #[test]
    fn page_html_has_bridge_and_testids() {
        assert!(PAGE_HTML.contains("plugin-ui-fetch"));
        assert!(PAGE_HTML.contains("plugin-ui-fetch-result"));
        assert!(PAGE_HTML.contains("data-testid=\"orch-preset\""));
        assert!(PAGE_HTML.contains("PROJECT_DEFINITION.md"));
        assert!(PAGE_HTML.contains("data-testid=\"orch-card\""));
        assert!(PAGE_HTML.contains("data-testid=\"orch-actions\""));
        assert!(PAGE_HTML.contains("data-testid=\"orch-eta\""));
        assert!(PAGE_HTML.contains("/api/plugin-ui/session-control"));
    }
}
