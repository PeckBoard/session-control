//! Orchestrator records, plugin-store access, and the plugin-side clock.
//!
//! Everything durable lives in the plugin document store:
//! - `orchestrators/<id>` — one [`Orchestrator`] per record.
//! - `engine/clock` — the last `timer.tick` timestamp. wasm32-unknown-unknown
//!   has no time source, so "now" is always host-supplied; hooks that carry no
//!   timestamp (tool invokes, `session.agent.ended`) read this instead, at
//!   ~30s granularity.
//! - `engine/pause_all` — the global kill switch.
//! - `busy/<session_id>` — best-effort busy map: set when the engine sends (or
//!   `session.message.before` fires), cleared on `session.agent.ended`, and
//!   ignored once stale (see [`BUSY_STALE_MINUTES`]).

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::host::{HostFn, call_host};

pub const ORCH_COLLECTION: &str = "orchestrators";
pub const ENGINE_COLLECTION: &str = "engine";
pub const BUSY_COLLECTION: &str = "busy";

pub const LOG_CAP: usize = 200;
pub const ETA_HISTORY_CAP: usize = 50;
/// A busy mark older than this is treated as idle — the guard against a
/// missed `session.agent.ended` wedging an orchestrator's brain "busy"
/// forever. Delivery queues host-side anyway, so a wrong "idle" is harmless.
pub const BUSY_STALE_MINUTES: i64 = 30;
/// Consecutive delivery failures before an orchestrator disables itself.
pub const MAX_CONSECUTIVE_FAILURES: u32 = 3;

fn default_true() -> bool {
    true
}
fn default_watchdog() -> u64 {
    15
}
fn default_cooldown() -> u64 {
    60
}
fn default_state() -> String {
    "in_progress".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Schedule {
    #[serde(default)]
    pub every_minutes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Watch {
    #[serde(default)]
    pub sessions: Vec<String>,
    #[serde(default)]
    pub folders: Vec<String>,
    #[serde(default = "default_true")]
    pub auto_watch_created: bool,
}

impl Default for Watch {
    fn default() -> Self {
        Self {
            sessions: Vec::new(),
            folders: Vec::new(),
            auto_watch_created: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Standards {
    #[serde(default = "default_true")]
    pub dev: bool,
    #[serde(default = "default_true")]
    pub testing: bool,
    #[serde(default = "default_true")]
    pub ux: bool,
    #[serde(default)]
    pub custom: String,
}

impl Default for Standards {
    fn default() -> Self {
        Self {
            dev: true,
            testing: true,
            ux: true,
            custom: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Caps {
    #[serde(default = "Caps::default_fires")]
    pub max_fires_per_hour: u32,
    #[serde(default = "Caps::default_sessions")]
    pub max_sessions_created: u32,
}

impl Caps {
    fn default_fires() -> u32 {
        12
    }
    fn default_sessions() -> u32 {
        10
    }
}

impl Default for Caps {
    fn default() -> Self {
        Self {
            max_fires_per_hour: Self::default_fires(),
            max_sessions_created: Self::default_sessions(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Eta {
    pub minutes_remaining: u64,
    pub projected_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalStatus {
    #[serde(default = "default_state")]
    pub state: String, // in_progress | blocked | done
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub percent: Option<u8>,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub eta: Option<Eta>,
}

impl Default for GoalStatus {
    fn default() -> Self {
        Self {
            state: default_state(),
            note: String::new(),
            percent: None,
            updated_at: String::new(),
            eta: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityEvent {
    pub ts: String,
    /// fired | skipped_busy | coalesced | cap_hit | backoff | tool_call |
    /// report | goal_update | standards_check | error | created_session |
    /// hat_assigned | watch_changed
    pub kind: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingTrigger {
    pub ts: String,
    pub kind: String, // session_idle | schedule | watchdog | manual
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub session_name: Option<String>,
    #[serde(default)]
    pub outcome: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Stats {
    #[serde(default)]
    pub actions: u64,
    #[serde(default)]
    pub fires: u64,
    #[serde(default)]
    pub last_fired_at: Option<String>,
    #[serde(default)]
    pub next_due_at: Option<String>,
    #[serde(default)]
    pub sessions_created: u32,
    /// Rolling fire timestamps for the hourly cap (pruned each check).
    #[serde(default)]
    pub fire_times: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HatAssignment {
    pub hat: String,
    pub responsibilities: String,
    pub assigned_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Orchestrator {
    pub id: String,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub paused: bool,
    /// The requirements this orchestrator autonomously drives to completion.
    #[serde(default)]
    pub goal: String,
    #[serde(default)]
    pub goal_status: GoalStatus,
    /// Capped history of `{ts, minutes_remaining, percent}` for drift display.
    #[serde(default)]
    pub eta_history: Vec<Value>,
    #[serde(default)]
    pub standards: Standards,
    /// Trigger prompt template. Vars: {{trigger}} {{session_name}} {{outcome}}
    /// {{goal}} {{watched_busy_count}} {{pending}} {{eta}} {{standards}}.
    #[serde(default)]
    pub prompt: String,
    /// Where the brain session (and sessions it creates by default) live.
    pub folder_id: String,
    #[serde(default)]
    pub model: Option<String>,
    /// The brain session, created lazily on first fire.
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub schedule: Schedule,
    #[serde(default = "default_watchdog")]
    pub watchdog_minutes: u64,
    #[serde(default)]
    pub watch: Watch,
    #[serde(default = "default_cooldown")]
    pub cooldown_secs: u64,
    #[serde(default)]
    pub caps: Caps,
    #[serde(default)]
    pub consecutive_failures: u32,
    #[serde(default)]
    pub pending_triggers: Vec<PendingTrigger>,
    #[serde(default)]
    pub stats: Stats,
    /// session_id → hat, as assigned via the `assign_hat` tool.
    #[serde(default)]
    pub hats: BTreeMap<String, HatAssignment>,
    #[serde(default)]
    pub created_sessions: Vec<String>,
    /// Capped activity feed, newest last.
    #[serde(default)]
    pub log: Vec<ActivityEvent>,
    #[serde(default)]
    pub last_rendered_prompt: String,
    /// Set when backoff auto-disabled the orchestrator; cleared on success.
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub created_at: String,
}

impl Orchestrator {
    pub fn push_log(&mut self, now: &str, kind: &str, detail: impl Into<String>) {
        self.log.push(ActivityEvent {
            ts: now.to_string(),
            kind: kind.to_string(),
            detail: detail.into(),
        });
        if self.log.len() > LOG_CAP {
            let drop = self.log.len() - LOG_CAP;
            self.log.drain(..drop);
        }
    }

    pub fn watches_session(&self, session_id: &str) -> bool {
        self.watch.sessions.iter().any(|s| s == session_id)
            || self.created_sessions.iter().any(|s| s == session_id)
    }

    pub fn watches_folder(&self, folder_id: &str) -> bool {
        self.watch.folders.iter().any(|f| f == folder_id)
    }
}

// ── Store access ──────────────────────────────────────────────────────

pub fn store_put(collection: &str, key: &str, data: Value) -> Result<(), String> {
    call_host(
        HostFn::StorePut,
        &json!({ "collection": collection, "key": key, "data": data }),
    )?;
    Ok(())
}

pub fn store_get(collection: &str, key: &str) -> Result<Option<Value>, String> {
    let out = call_host(
        HostFn::StoreGet,
        &json!({ "collection": collection, "key": key }),
    )?;
    match out.get("value") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => Ok(Some(v.clone())),
    }
}

pub fn store_list(collection: &str) -> Result<Vec<(String, Value)>, String> {
    let out = call_host(HostFn::StoreList, &json!({ "collection": collection }))?;
    Ok(out
        .get("items")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|it| {
                    let key = it.get("key")?.as_str()?.to_string();
                    let value = it.get("value")?.clone();
                    Some((key, value))
                })
                .collect()
        })
        .unwrap_or_default())
}

pub fn store_delete(collection: &str, key: &str) -> Result<(), String> {
    let _ = call_host(
        HostFn::StoreDelete,
        &json!({ "collection": collection, "key": key }),
    );
    Ok(())
}

pub fn load_orchestrator(id: &str) -> Result<Option<Orchestrator>, String> {
    Ok(
        store_get(ORCH_COLLECTION, id)?
            .and_then(|v| serde_json::from_value::<Orchestrator>(v).ok()),
    )
}

pub fn save_orchestrator(o: &Orchestrator) -> Result<(), String> {
    store_put(
        ORCH_COLLECTION,
        &o.id,
        serde_json::to_value(o).map_err(|e| e.to_string())?,
    )
}

pub fn list_orchestrators() -> Result<Vec<Orchestrator>, String> {
    let mut out: Vec<Orchestrator> = store_list(ORCH_COLLECTION)?
        .into_iter()
        .filter_map(|(_, v)| serde_json::from_value(v).ok())
        .collect();
    out.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    Ok(out)
}

// ── Clock (host-supplied "now") ───────────────────────────────────────

pub fn set_clock(now: &str) {
    let _ = store_put(ENGINE_COLLECTION, "clock", json!({ "now": now }));
}

/// The last `timer.tick` timestamp, or None before the first tick.
pub fn clock() -> Option<String> {
    store_get(ENGINE_COLLECTION, "clock")
        .ok()
        .flatten()
        .and_then(|v| v.get("now").and_then(|n| n.as_str()).map(str::to_string))
}

pub fn parse_ts(ts: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// `ts + minutes`, RFC 3339. Falls back to `ts` on parse failure.
pub fn add_minutes(ts: &str, minutes: i64) -> String {
    parse_ts(ts)
        .map(|dt| (dt + Duration::minutes(minutes)).to_rfc3339())
        .unwrap_or_else(|| ts.to_string())
}

/// Whole seconds from `earlier` to `later`; None if either fails to parse.
pub fn seconds_between(earlier: &str, later: &str) -> Option<i64> {
    Some((parse_ts(later)? - parse_ts(earlier)?).num_seconds())
}

// ── Global pause + busy map ───────────────────────────────────────────

pub fn global_paused() -> bool {
    store_get(ENGINE_COLLECTION, "pause_all")
        .ok()
        .flatten()
        .and_then(|v| v.get("paused").and_then(|p| p.as_bool()))
        .unwrap_or(false)
}

pub fn set_global_paused(paused: bool) {
    let _ = store_put(ENGINE_COLLECTION, "pause_all", json!({ "paused": paused }));
}

pub fn mark_busy(session_id: &str, now: &str) {
    let _ = store_put(
        BUSY_COLLECTION,
        session_id,
        json!({ "busy": true, "since": now }),
    );
}

pub fn mark_idle(session_id: &str) {
    let _ = store_delete(BUSY_COLLECTION, session_id);
}

/// Best-effort: true only for a fresh busy mark (see [`BUSY_STALE_MINUTES`]).
pub fn is_busy(session_id: &str, now: &str) -> bool {
    let Some(v) = store_get(BUSY_COLLECTION, session_id).ok().flatten() else {
        return false;
    };
    if !v.get("busy").and_then(|b| b.as_bool()).unwrap_or(false) {
        return false;
    }
    let since = v.get("since").and_then(|s| s.as_str()).unwrap_or("");
    match seconds_between(since, now) {
        Some(secs) => secs < BUSY_STALE_MINUTES * 60,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orchestrator_defaults_fill_from_minimal_json() {
        let o: Orchestrator = serde_json::from_value(json!({
            "id": "o1", "name": "Ship it", "folder_id": "f1"
        }))
        .unwrap();
        assert!(o.enabled);
        assert!(!o.paused);
        assert_eq!(o.watchdog_minutes, 15);
        assert_eq!(o.cooldown_secs, 60);
        assert_eq!(o.caps.max_fires_per_hour, 12);
        assert!(o.watch.auto_watch_created);
        assert_eq!(o.goal_status.state, "in_progress");
        assert!(o.standards.dev && o.standards.testing && o.standards.ux);
    }

    #[test]
    fn push_log_caps_the_feed() {
        let mut o: Orchestrator =
            serde_json::from_value(json!({ "id": "o", "name": "n", "folder_id": "f" })).unwrap();
        for i in 0..(LOG_CAP + 25) {
            o.push_log("2026-01-01T00:00:00Z", "fired", format!("e{i}"));
        }
        assert_eq!(o.log.len(), LOG_CAP);
        assert_eq!(o.log.last().unwrap().detail, format!("e{}", LOG_CAP + 24));
    }

    #[test]
    fn time_math_parses_and_adds() {
        let t = "2026-08-29T10:00:00+00:00";
        assert_eq!(add_minutes(t, 30), "2026-08-29T10:30:00+00:00");
        assert_eq!(
            seconds_between("2026-08-29T10:00:00Z", "2026-08-29T10:01:30Z"),
            Some(90)
        );
        assert_eq!(seconds_between("garbage", t), None);
    }

    #[test]
    fn watches_session_covers_watch_list_and_created() {
        let mut o: Orchestrator =
            serde_json::from_value(json!({ "id": "o", "name": "n", "folder_id": "f" })).unwrap();
        o.watch.sessions.push("s1".into());
        o.created_sessions.push("s2".into());
        assert!(o.watches_session("s1"));
        assert!(o.watches_session("s2"));
        assert!(!o.watches_session("s3"));
    }
}
