//! MCP tools that manage orchestrators themselves — list / get / create /
//! update / delete — for any session (the brain-only tools live in
//! `orch_tools.rs`).
//!
//! Mutations reuse the Orchestrators page's own code path
//! (`page::{create,update,delete}_orchestrator` + `page::apply_fields`), so
//! tools and UI share one set of rules, and the store writes push the same
//! `plugin-data` refresh to open pages.
//!
//! Orchestrators spawn autonomous agents, so create and delete always ask
//! the caller's user (`authorize::ensure_user_approved`), and so does an
//! update that widens what an orchestrator does (goal / prompt / model /
//! folder / watch lists, enabling, un-pausing, raising a cap) or that one
//! orchestrator's brain makes to ANOTHER orchestrator. Narrowing edits
//! (rename, disable, pause, schedule/standards tweaks, lowering a cap) run
//! immediately.

use serde_json::{Value, json};

use crate::authorize::{self, Gate};
use crate::host::{HostFn, call_host};
use crate::page;
use crate::state::{self, Orchestrator};

fn require_id(args: &Value) -> Result<String, String> {
    args.get("id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "'id' is required".to_string())
}

fn load_existing(id: &str) -> Result<Orchestrator, String> {
    state::load_orchestrator(id)?.ok_or(format!("orchestrator not found: {id}"))
}

/// The tool arguments as a page-form body (everything but `id`).
fn body_of(args: &Value) -> Value {
    let mut body = args.clone();
    if let Some(map) = body.as_object_mut() {
        map.remove("id");
    }
    body
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

fn caller_scope() -> Result<(Option<String>, Option<String>), String> {
    let scope = call_host(HostFn::CallerScope, &json!({}))?;
    let get = |k: &str| {
        scope
            .get(k)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    Ok((get("session_id"), get("folder_id")))
}

fn gated(
    action_key: &str,
    args: &Value,
    question: &str,
    then: impl FnOnce() -> Result<Value, String>,
) -> Result<Value, String> {
    match authorize::ensure_user_approved(action_key, &args.to_string(), question)? {
        Gate::Awaiting(payload) => Ok(payload),
        Gate::Ready => then(),
    }
}

// ── Read-only ─────────────────────────────────────────────────────────

/// `orchestrator_list {}` — compact summary of every orchestrator.
pub fn list_tool(_args: Value) -> Result<Value, String> {
    let now = state::clock().unwrap_or_default();
    let orchs: Vec<Value> = state::list_orchestrators()?
        .iter()
        .map(|o| {
            json!({
                "id": o.id,
                "name": o.name,
                "enabled": o.enabled,
                "paused": o.paused,
                "goal_state": o.goal_status.state,
                "goal_percent": o.goal_status.percent,
                "eta_minutes": o.goal_status.eta.as_ref().map(|e| e.minutes_remaining),
                "model": o.model,
                "folder_id": o.folder_id,
                "brain_session_id": o.session_id,
                "brain_busy": o.session_id.as_deref()
                    .map(|s| !now.is_empty() && state::is_busy(s, &now))
                    .unwrap_or(false),
                "watched_sessions": o.watch.sessions.len(),
                "created_sessions": o.created_sessions.len(),
                "fires": o.stats.fires,
                "actions": o.stats.actions,
                "error": o.error,
            })
        })
        .collect();
    Ok(json!({ "orchestrators": orchs, "global_paused": state::global_paused() }))
}

/// `orchestrator_get { id }` — the full record, as the page sees it.
pub fn get_tool(args: Value) -> Result<Value, String> {
    let o = load_existing(&require_id(&args)?)?;
    let now = state::clock().unwrap_or_default();
    Ok(page::with_liveness(&o, &now))
}

// ── Mutating (approval-gated) ─────────────────────────────────────────

/// `orchestrator_create { name, folder_id?, goal?, … }` — always asks the
/// user. `folder_id` defaults to the caller's folder.
pub fn create_tool(args: Value) -> Result<Value, String> {
    let mut body = body_of(&args);
    if body.get("folder_id").and_then(|v| v.as_str()).is_none()
        && let (_, Some(folder)) = caller_scope()?
        && let Some(map) = body.as_object_mut()
    {
        map.insert("folder_id".into(), json!(folder));
    }
    // Validate before asking — never prompt the user about a request the
    // page's rules would reject anyway.
    let mut probe: Orchestrator = serde_json::from_value(json!({
        "id": "", "name": "", "folder_id": "",
    }))
    .map_err(|e| e.to_string())?;
    page::apply_fields(&mut probe, &body)?;
    if probe.name.is_empty() {
        return Err("name is required".into());
    }
    if probe.folder_id.is_empty() {
        return Err("folder_id is required (no caller folder to default to)".into());
    }
    let question = format!(
        "Create orchestrator \"{}\" in folder {} (model: {})? It runs an autonomous brain \
         session.\nGoal: {}",
        probe.name,
        probe.folder_id,
        probe.model.as_deref().unwrap_or("default"),
        clip(&probe.goal, 400),
    );
    gated("create", &body, &question, || {
        let o = page::create_orchestrator(&body)?;
        serde_json::to_value(&o).map_err(|e| e.to_string())
    })
}

/// Why an update to `o` needs approval (empty = it runs immediately).
fn update_reasons(o: &Orchestrator, body: &Value) -> Vec<&'static str> {
    let mut why = Vec::new();
    let str_changes = |key: &str, cur: &str| {
        body.get(key)
            .and_then(|v| v.as_str())
            .is_some_and(|v| v != cur)
    };
    if str_changes("goal", &o.goal) {
        why.push("goal");
    }
    if str_changes("prompt", &o.prompt) {
        why.push("trigger prompt");
    }
    if str_changes("folder_id", &o.folder_id) {
        why.push("folder");
    }
    if let Some(m) = body.get("model") {
        let new = m.as_str().filter(|s| !s.is_empty());
        if new != o.model.as_deref() {
            why.push("model");
        }
    }
    if body.get("enabled").and_then(|v| v.as_bool()) == Some(true) && !o.enabled {
        why.push("enable");
    }
    if body.get("paused").and_then(|v| v.as_bool()) == Some(false) && o.paused {
        why.push("resume");
    }
    let raises = |key: &str, cur: u32| {
        body.get(key)
            .and_then(|v| v.as_u64())
            .is_some_and(|v| v > cur as u64)
    };
    if raises("max_fires_per_hour", o.caps.max_fires_per_hour) {
        why.push("raise max_fires_per_hour");
    }
    if raises("max_sessions_created", o.caps.max_sessions_created) {
        why.push("raise max_sessions_created");
    }
    let list_changes = |key: &str, cur: &[String]| {
        body.get(key).and_then(|v| v.as_array()).is_some_and(|v| {
            let new: Vec<&str> = v.iter().filter_map(|s| s.as_str()).collect();
            new != cur.iter().map(String::as_str).collect::<Vec<_>>()
        })
    };
    if list_changes("watch_sessions", &o.watch.sessions) {
        why.push("watched sessions");
    }
    if list_changes("watch_folders", &o.watch.folders) {
        why.push("watched folders");
    }
    why
}

/// `orchestrator_update { id, …partial fields }` — page-form fields plus
/// `enabled` / `paused`; absent fields are left alone.
pub fn update_tool(args: Value) -> Result<Value, String> {
    let id = require_id(&args)?;
    let o = load_existing(&id)?;
    let body = body_of(&args);
    let mut reasons = update_reasons(&o, &body);

    // One orchestrator's brain may never reshape another one unasked.
    let (caller, _) = caller_scope()?;
    let other_brain = caller.as_deref().is_some_and(|sid| {
        state::list_orchestrators()
            .unwrap_or_default()
            .iter()
            .any(|b| b.id != o.id && b.session_id.as_deref() == Some(sid))
    });
    if other_brain {
        reasons.insert(0, "requested by another orchestrator's brain");
    }

    let apply = || {
        let saved = page::update_orchestrator(&id, &body)?;
        serde_json::to_value(&saved).map_err(|e| e.to_string())
    };
    if reasons.is_empty() {
        return apply();
    }
    let question = format!(
        "Update orchestrator \"{}\" ({})? Changes: {}.\nRequest: {}",
        o.name,
        o.id,
        reasons.join(", "),
        clip(&body.to_string(), 500),
    );
    gated(&format!("update:{id}"), &body, &question, apply)
}

/// `orchestrator_delete { id }` — always asks the user; removes the record
/// and stops its brain's agent exactly as the page's Delete does (the brain
/// session and its transcript are kept).
pub fn delete_tool(args: Value) -> Result<Value, String> {
    let id = require_id(&args)?;
    let o = load_existing(&id)?;
    let question = format!(
        "Delete orchestrator \"{}\" ({})? It stops driving its goal and its brain \
         session's agent is stopped; the brain session and transcript are kept.",
        o.name, o.id
    );
    gated(
        &format!("delete:{id}"),
        &json!({ "id": id }),
        &question,
        || {
            let out = page::delete_orchestrator(&id)?;
            let mut result = json!({ "ok": true, "deleted": id, "name": o.name });
            result["brain_stopped"] = json!(out.brain_stopped);
            if let Some(e) = out.brain_stop_error {
                result["brain_stop_error"] = json!(e);
            }
            Ok(result)
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::fake;

    /// Answer the single open approval question, then re-call `f`.
    fn approve_and_recall(f: impl Fn() -> Result<Value, String>) -> Value {
        let first = f().unwrap();
        assert_eq!(first["status"], "awaiting_approval", "{first}");
        let open = fake::open_questions();
        assert_eq!(open.len(), 1, "exactly one question asked");
        fake::answer(&open[0].0, "Approve once");
        f().unwrap()
    }

    #[test]
    fn crud_via_tools_with_approval() {
        fake::set_caller("chat-1", "folder-a");

        // Create: gated, folder defaults to the caller's, page defaults apply.
        let args = json!({ "name": "Ship it", "goal": "Do the thing", "model": "mock:mcp" });
        let created = approve_and_recall(|| create_tool(args.clone()));
        let id = created["id"].as_str().unwrap().to_string();
        assert_eq!(created["folder_id"], "folder-a");
        assert_eq!(created["watchdog_minutes"], 15);
        assert_eq!(created["caps"]["max_fires_per_hour"], 12);

        // List + get see it.
        let listed = list_tool(json!({})).unwrap();
        assert_eq!(listed["orchestrators"][0]["id"], id.as_str());
        assert_eq!(listed["orchestrators"][0]["model"], "mock:mcp");
        let got = get_tool(json!({ "id": id })).unwrap();
        assert_eq!(got["goal"], "Do the thing");
        assert_eq!(got["brain_busy"], false);

        // Narrowing update (rename + pause) runs immediately.
        let upd = update_tool(json!({ "id": id, "name": "Renamed", "paused": true })).unwrap();
        assert_eq!(upd["name"], "Renamed");
        assert_eq!(upd["paused"], true);
        assert!(fake::open_questions().is_empty());

        // Widening update (goal change) is gated.
        let upd = approve_and_recall(|| update_tool(json!({ "id": id, "goal": "New goal" })));
        assert_eq!(upd["goal"], "New goal");

        // Page validation is shared: blank name is rejected.
        assert!(update_tool(json!({ "id": id, "name": " " })).is_err());
        // Unknown id is a clear error.
        let err = update_tool(json!({ "id": "nope", "name": "x" })).unwrap_err();
        assert!(err.contains("orchestrator not found: nope"), "{err}");

        // Delete: gated, then gone — and the brain's agent is stopped.
        let mut o = state::load_orchestrator(&id).unwrap().unwrap();
        o.session_id = Some("brain-1".into());
        state::save_orchestrator(&o).unwrap();
        let del = approve_and_recall(|| delete_tool(json!({ "id": id })));
        assert_eq!(del["deleted"], id.as_str());
        assert_eq!(del["brain_stopped"], true);
        assert!(del.get("brain_stop_error").is_none(), "{del}");
        assert_eq!(fake::terminated(), ["brain-1"]);
        assert!(get_tool(json!({ "id": id })).is_err());
    }

    #[test]
    fn delete_stop_failure_or_no_brain_still_deletes() {
        fake::set_caller("chat-1", "folder-a");
        // No brain yet: nothing to stop.
        let a = approve_and_recall(|| create_tool(json!({ "name": "A" })));
        let out = page::delete_orchestrator(a["id"].as_str().unwrap()).unwrap();
        assert!(out.existed && !out.brain_stopped && out.brain_stop_error.is_none());
        assert!(fake::terminated().is_empty());

        // A failing stop is reported but never blocks the delete.
        let b = approve_and_recall(|| create_tool(json!({ "name": "B" })));
        let b_id = b["id"].as_str().unwrap();
        let mut o = state::load_orchestrator(b_id).unwrap().unwrap();
        o.session_id = Some("brain-b".into());
        state::save_orchestrator(&o).unwrap();
        fake::fail_terminate(true);
        let out = page::delete_orchestrator(b_id).unwrap();
        assert!(out.existed && !out.brain_stopped);
        assert_eq!(
            out.brain_stop_error.as_deref(),
            Some("live control unavailable")
        );
        assert!(state::load_orchestrator(b_id).unwrap().is_none());
    }

    #[test]
    fn denied_or_changed_requests_do_not_apply() {
        fake::set_caller("chat-1", "folder-a");
        let args = json!({ "name": "A" });
        assert_eq!(
            create_tool(args.clone()).unwrap()["status"],
            "awaiting_approval"
        );
        // Re-calling with different arguments asks afresh instead of
        // reusing the pending question.
        let other = json!({ "name": "B" });
        assert_eq!(
            create_tool(other.clone()).unwrap()["status"],
            "awaiting_approval"
        );
        let open = fake::open_questions();
        assert_eq!(open.len(), 2);
        let newest = open.iter().find(|(_, q)| q.contains("\"B\"")).unwrap();
        fake::answer(&newest.0, "Deny");
        assert!(create_tool(other).is_err());
        assert!(state::list_orchestrators().unwrap().is_empty());
    }

    #[test]
    fn brain_editing_another_orchestrator_is_gated() {
        fake::set_caller("chat-1", "folder-a");
        let mine = approve_and_recall(|| create_tool(json!({ "name": "Mine" })));
        let other = approve_and_recall(|| create_tool(json!({ "name": "Other" })));
        // Make chat-1 the brain of "Mine".
        let mut o = state::load_orchestrator(mine["id"].as_str().unwrap())
            .unwrap()
            .unwrap();
        o.session_id = Some("chat-1".into());
        state::save_orchestrator(&o).unwrap();

        // Even a narrowing rename of ANOTHER orchestrator needs approval…
        let other_id = other["id"].as_str().unwrap();
        let r = update_tool(json!({ "id": other_id, "name": "Hijacked" })).unwrap();
        assert_eq!(r["status"], "awaiting_approval");
        // …while renaming its own runs immediately.
        let r = update_tool(json!({ "id": o.id, "name": "Still mine" })).unwrap();
        assert_eq!(r["name"], "Still mine");
    }

    #[test]
    fn mutations_need_a_caller_session() {
        fake::set_caller("", "folder-a");
        assert!(create_tool(json!({ "name": "x" })).is_err());
    }
}
