//! MCP tools for orchestrator brain sessions: session creation with hats,
//! watch-list control, goal/ETA reporting, and the activity feed.
//!
//! Every tool is callable by any session (the generic surface), but the
//! orchestrator-specific effects — watch lists, goal status, action counts,
//! the activity feed — apply when the CALLER is an orchestrator's brain
//! session, resolved via `peckboard_caller_scope` (host-verified, never an
//! argument).

use serde_json::{Value, json};

use crate::engine;
use crate::host::{HostFn, call_host};
use crate::state::{self, HatAssignment, Orchestrator};

/// Pull a required, non-empty string argument.
fn require_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("'{key}' is required"))
}

fn opt_str(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
}

/// The caller's session id, from the host-verified scope.
fn caller_session() -> Result<Option<String>, String> {
    let scope = call_host(HostFn::CallerScope, &json!({}))?;
    Ok(scope
        .get("session_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string))
}

/// The orchestrator whose brain is the calling session, if any.
fn caller_orchestrator() -> Result<Option<Orchestrator>, String> {
    let Some(sid) = caller_session()? else {
        return Ok(None);
    };
    Ok(state::list_orchestrators()?
        .into_iter()
        .find(|o| o.session_id.as_deref() == Some(sid.as_str())))
}

fn require_caller_orchestrator() -> Result<Orchestrator, String> {
    caller_orchestrator()?.ok_or_else(|| {
        "this tool is for orchestrator brain sessions; the calling session is not one".into()
    })
}

fn now() -> String {
    state::clock().unwrap_or_else(|| "1970-01-01T00:00:00Z".into())
}

/// The hat system prompt written onto a session by `assign_hat` /
/// `create_session {hat}`.
pub fn hat_prompt(hat: &str, responsibilities: &str) -> String {
    format!(
        "You are wearing the \"{hat}\" hat — your assigned scope of responsibility.\n\
Responsibilities: {responsibilities}\n\
Stay inside this scope: do the work it covers well, and when you hit something \
outside it, report that back instead of expanding your own mandate. An orchestrator \
session manages you and reviews your output when your turn ends."
    )
}

// ── Tools ─────────────────────────────────────────────────────────────

/// `create_session { name, model?, hat?, responsibilities? }` — spawn a
/// worker-for-more-work session in the caller's folder. When the caller is a
/// brain: capped, auto-watched, hat recorded, action counted.
pub fn create_session_tool(args: Value) -> Result<Value, String> {
    let name = require_str(&args, "name")?;
    let model = opt_str(&args, "model");
    let hat = opt_str(&args, "hat");
    let responsibilities = opt_str(&args, "responsibilities").unwrap_or_default();

    // The whole check→create→record cycle holds the engine lease: the cap
    // check and the counter bump must see each other across instances.
    state::try_with_engine_lock(|| {
        let mut orch = caller_orchestrator()?;
        if let Some(o) = &orch
            && o.stats.sessions_created >= o.caps.max_sessions_created
        {
            return Err(format!(
                "session cap reached ({} of max_sessions_created={}) — raise the cap on the \
                 Orchestrators page if more are truly needed",
                o.stats.sessions_created, o.caps.max_sessions_created
            ));
        }

        let system_prompt = hat.as_ref().map(|h| hat_prompt(h, &responsibilities));
        let created = call_host(
            HostFn::CreateSession,
            &json!({ "name": name, "model": model, "system_prompt": system_prompt }),
        )?;
        let sid = created
            .get("session")
            .and_then(|s| s.get("id"))
            .and_then(|v| v.as_str())
            .ok_or("create_session returned no session id")?
            .to_string();

        if let Some(o) = orch.as_mut() {
            let ts = now();
            o.stats.sessions_created += 1;
            o.stats.actions += 1;
            if o.watch.auto_watch_created {
                o.created_sessions.push(sid.clone());
            }
            if let Some(h) = &hat {
                o.hats.insert(
                    sid.clone(),
                    HatAssignment {
                        hat: h.clone(),
                        responsibilities: responsibilities.clone(),
                        assigned_at: ts.clone(),
                    },
                );
            }
            o.push_log(
                &ts,
                "created_session",
                format!(
                    "created session \"{name}\" ({sid}){}",
                    hat.as_ref()
                        .map(|h| format!(" wearing hat \"{h}\""))
                        .unwrap_or_default()
                ),
            );
            state::save_orchestrator(o)?;
        }
        Ok(json!({ "ok": true, "session_id": sid, "watched": orch.is_some() }))
    })?
    .ok_or_else(|| state::BUSY_MSG.to_string())
}
/// `assign_hat { session_id, hat, responsibilities }` — write the hat as the
/// target session's system prompt (takes effect on its next turn).
pub fn assign_hat_tool(args: Value) -> Result<Value, String> {
    let session_id = require_str(&args, "session_id")?;
    let hat = require_str(&args, "hat")?;
    let responsibilities = require_str(&args, "responsibilities")?;

    call_host(
        HostFn::OrchestrateSetPrompt,
        &json!({
            "session_id": session_id,
            "system_prompt": hat_prompt(&hat, &responsibilities),
        }),
    )?;

    state::try_with_engine_lock(|| {
        if let Some(mut o) = caller_orchestrator()? {
            let ts = now();
            o.stats.actions += 1;
            o.hats.insert(
                session_id.clone(),
                HatAssignment {
                    hat: hat.clone(),
                    responsibilities: responsibilities.clone(),
                    assigned_at: ts.clone(),
                },
            );
            o.push_log(
                &ts,
                "hat_assigned",
                format!("hat \"{hat}\" → session {session_id}"),
            );
            state::save_orchestrator(&o)?;
        }
        Ok(json!({ "ok": true, "session_id": session_id, "hat": hat }))
    })?
    .ok_or_else(|| state::BUSY_MSG.to_string())
}

/// `watch_session { session_id }` / `unwatch_session { session_id }`.
pub fn watch_session_tool(args: Value, watch: bool) -> Result<Value, String> {
    let session_id = require_str(&args, "session_id")?;
    state::try_with_engine_lock(|| {
        let mut o = require_caller_orchestrator()?;
        let ts = now();
        if watch {
            if !o.watch.sessions.contains(&session_id) {
                o.watch.sessions.push(session_id.clone());
            }
        } else {
            o.watch.sessions.retain(|s| s != &session_id);
            o.created_sessions.retain(|s| s != &session_id);
        }
        o.stats.actions += 1;
        o.push_log(
            &ts,
            "watch_changed",
            format!(
                "{} {session_id}",
                if watch {
                    "now watching"
                } else {
                    "stopped watching"
                }
            ),
        );
        state::save_orchestrator(&o)?;
        Ok(json!({ "ok": true, "watching": watch, "session_id": session_id }))
    })?
    .ok_or_else(|| state::BUSY_MSG.to_string())
}

/// `list_managed_sessions {}` — watched + created sessions with hats and
/// best-effort busy state.
pub fn list_managed_sessions_tool(_args: Value) -> Result<Value, String> {
    let o = require_caller_orchestrator()?;
    let ts = now();
    // Names/folders via folder-blind discovery, one call.
    let all = call_host(HostFn::ListSessions, &json!({ "query": "" }))?;
    let all = all
        .get("sessions")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut ids: Vec<&String> = o
        .watch
        .sessions
        .iter()
        .chain(o.created_sessions.iter())
        .collect();
    ids.dedup();
    let sessions: Vec<Value> = ids
        .into_iter()
        .map(|id| {
            let info = all
                .iter()
                .find(|s| s.get("session_id").and_then(|v| v.as_str()) == Some(id.as_str()));
            json!({
                "session_id": id,
                "name": info.and_then(|s| s.get("name")).cloned().unwrap_or(Value::Null),
                "folder_id": info.and_then(|s| s.get("folder_id")).cloned().unwrap_or(Value::Null),
                "exists": info.is_some(),
                "busy": state::is_busy(id, &ts),
                "hat": o.hats.get(id.as_str()).map(|h| json!({
                    "hat": h.hat, "responsibilities": h.responsibilities,
                })).unwrap_or(Value::Null),
                "created_by_orchestrator": o.created_sessions.contains(id),
            })
        })
        .collect();
    Ok(json!({ "orchestrator": o.name, "goal_status": o.goal_status, "sessions": sessions }))
}

/// `update_goal_status { state, note?, percent?, eta_minutes }` — progress +
/// the ETA for the ENTIRE requirements. `eta_minutes` is required while not
/// done; `state=done` stops the autonomy watchdog.
pub fn update_goal_status_tool(args: Value) -> Result<Value, String> {
    let goal_state = require_str(&args, "state")?;
    if !matches!(goal_state.as_str(), "in_progress" | "blocked" | "done") {
        return Err("'state' must be one of: in_progress, blocked, done".into());
    }
    let note = opt_str(&args, "note").unwrap_or_default();
    let percent = args
        .get("percent")
        .and_then(|v| v.as_u64())
        .map(|p| p.min(100) as u8);
    let eta_minutes = args.get("eta_minutes").and_then(|v| v.as_u64());
    let eta_minutes = match (goal_state.as_str(), eta_minutes) {
        ("done", m) => m.unwrap_or(0),
        (_, Some(m)) => m,
        (_, None) => {
            return Err(
                "'eta_minutes' is required while the goal is not done — your current \
                 estimate of minutes until the entire requirements are implemented"
                    .into(),
            );
        }
    };

    state::try_with_engine_lock(|| {
        let mut o = require_caller_orchestrator()?;
        let ts = now();
        o.goal_status.state = goal_state.clone();
        o.goal_status.note = note.clone();
        o.goal_status.percent = percent;
        o.goal_status.updated_at = ts.clone();
        o.goal_status.eta = Some(state::Eta {
            minutes_remaining: eta_minutes,
            projected_at: state::add_minutes(&ts, eta_minutes as i64),
            updated_at: ts.clone(),
        });
        engine::push_eta_history(&mut o, &ts, eta_minutes);
        o.stats.actions += 1;
        o.push_log(
            &ts,
            "goal_update",
            format!(
                "state={goal_state}{} eta={eta_minutes}min{}",
                percent.map(|p| format!(" {p}%")).unwrap_or_default(),
                if note.is_empty() {
                    String::new()
                } else {
                    format!(" — {note}")
                }
            ),
        );
        state::save_orchestrator(&o)?;
        Ok(json!({ "ok": true, "state": goal_state, "eta_minutes": eta_minutes }))
    })?
    .ok_or_else(|| state::BUSY_MSG.to_string())
}

/// `orchestrator_report { summary }` — one-line activity entry for the page.
pub fn orchestrator_report_tool(args: Value) -> Result<Value, String> {
    let summary = require_str(&args, "summary")?;
    state::try_with_engine_lock(|| {
        let mut o = require_caller_orchestrator()?;
        let ts = now();
        o.stats.actions += 1;
        o.push_log(&ts, "report", summary.clone());
        state::save_orchestrator(&o)?;
        Ok(json!({ "ok": true }))
    })?
    .ok_or_else(|| state::BUSY_MSG.to_string())
}

/// Attribute a successful control-tool call (`send_message`, `interrupt_…`,
/// …) to the calling brain's action count + feed. Best-effort — never fails
/// the tool call it decorates, and a contended lease just drops the
/// attribution.
pub fn attribute_action(tool: &str, target_session: Option<&str>) {
    let _ = state::try_with_engine_lock(|| {
        let Some(mut o) = caller_orchestrator()? else {
            return Ok(());
        };
        let ts = now();
        o.stats.actions += 1;
        o.push_log(
            &ts,
            "tool_call",
            match target_session {
                Some(t) => format!("{tool} → {t}"),
                None => tool.to_string(),
            },
        );
        let _ = state::save_orchestrator(&o);
        Ok(())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hat_prompt_names_hat_and_scope() {
        let p = hat_prompt("QA", "test everything");
        assert!(p.contains("\"QA\" hat"));
        assert!(p.contains("test everything"));
        assert!(p.contains("Stay inside this scope"));
    }

    #[test]
    fn require_str_rejects_blank() {
        assert!(require_str(&json!({ "state": " " }), "state").is_err());
        assert_eq!(
            require_str(&json!({ "state": "done" }), "state").unwrap(),
            "done"
        );
    }
}
