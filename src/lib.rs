//! Peckboard session-control plugin (WASM / Extism).
//!
//! Two surfaces:
//!
//! **Session control** (MCP tools via `mcp.tool.invoke`):
//! - **interrupt_session** / **terminate_agent** / **clear_session** /
//!   **send_message** / **send_image** — mutating actions.
//! - **find_session** — folder-blind discovery (no approval prompt).
//!
//! Same-folder targets run immediately. Cross-folder mutating actions ask the
//! user (*Approve once* / *Approve always* / *Deny*); Always is remembered for
//! that controlling session. The Peckboard host enforces the same grants so a
//! bypass cannot skip the gate.
//!
//! **Orchestrators** (0.4.0): goal-driven configs that autonomously drive
//! requirements to completion through a dedicated "brain" session. The engine
//! (`engine.rs`) fires on `timer.tick` schedules/watchdogs and on watched
//! sessions going idle (`session.agent.ended`); brains get extra tools
//! (`orch_tools.rs`: create_session, assign_hat, watch/unwatch,
//! list_managed_sessions, update_goal_status + ETA, orchestrator_report); the
//! management page (`page.rs`) is served from the global sidebar.
//!
//! ## Plugin interface
//!
//! Core expects four exports (`peckboard/src/plugin/manager.rs`):
//! - `manifest` — declares the hooks handled, MCP tools, and UI surfaces.
//! - `init` — called once on load with the plugin's config block; a no-op here.
//! - `handle` — called per hook with `{ "hook", "payload" }`; returns a Verdict.
//! - `shutdown` — teardown; a no-op here.

#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code, unused_imports))]

mod authorize;
mod control;
mod engine;
mod host;
mod manifest;
mod orch_tools;
mod page;
mod state;

use serde::Deserialize;

#[cfg(target_arch = "wasm32")]
mod entry {
    use super::*;
    use extism_pdk::*;

    #[plugin_fn]
    pub fn manifest() -> FnResult<String> {
        Ok(crate::manifest::manifest_json())
    }

    #[plugin_fn]
    pub fn init(_config: String) -> FnResult<String> {
        Ok(serde_json::json!({ "ok": true }).to_string())
    }

    #[plugin_fn]
    pub fn shutdown() -> FnResult<String> {
        Ok(serde_json::json!({ "ok": true }).to_string())
    }

    #[plugin_fn]
    pub fn handle(input: String) -> FnResult<String> {
        let call: HookCall = serde_json::from_str(&input)?;
        Ok(dispatch_hook(&call.hook, call.payload))
    }
}

/// The `{ "hook", "payload" }` envelope core passes to `handle`.
#[derive(Debug, Deserialize)]
struct HookCall {
    hook: String,
    #[serde(default)]
    payload: serde_json::Value,
}

/// Route one hook dispatch. Engine hooks are notifications — their errors
/// become an allow-with-error payload (verdicts on notifications are ignored
/// anyway) so a hiccup never cancels anything host-side.
fn dispatch_hook(hook: &str, payload: serde_json::Value) -> String {
    match hook {
        "mcp.tool.invoke" => handle_invoke(payload),
        "timer.tick" => notification(engine::on_timer_tick(payload)),
        "session.agent.ended" => notification(engine::on_agent_ended(payload)),
        "session.message.before" => {
            // Observed only (busy tracking) — never rewrite the message.
            let _ = engine::on_message_before(payload);
            skip()
        }
        "http.request.before" => match page::serve_public(payload) {
            Ok(resp) => allow(resp),
            Err(e) => cancel(&e),
        },
        "http.request.authed" => match page::serve_authed(payload) {
            Ok(resp) => allow(resp),
            Err(e) => cancel(&e),
        },
        _ => skip(),
    }
}

fn notification(result: Result<serde_json::Value, String>) -> String {
    match result {
        Ok(v) => allow(v),
        Err(e) => allow(serde_json::json!({ "ok": false, "error": e })),
    }
}

/// Dispatch an `mcp.tool.invoke` to the right tool. A tool's `Err` becomes a
/// `Verdict::Cancel` (surfaced to the worker as an MCP tool error); an unknown
/// tool is also a Cancel. Success is a `Verdict::Allow` carrying the value.
fn handle_invoke(payload: serde_json::Value) -> String {
    let tool = payload
        .get("tool")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let args = payload
        .get("arguments")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let target = args
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let result: Result<serde_json::Value, String> = match tool.as_str() {
        "interrupt_session" => control::interrupt_session_tool(args),
        "terminate_agent" => control::terminate_agent_tool(args),
        "clear_session" => control::clear_session_tool(args),
        "send_message" => control::send_message_tool(args),
        "send_image" => control::send_image_tool(args),
        "find_session" => control::find_session_tool(args),
        "create_session" => orch_tools::create_session_tool(args),
        "assign_hat" => orch_tools::assign_hat_tool(args),
        "watch_session" => orch_tools::watch_session_tool(args, true),
        "unwatch_session" => orch_tools::watch_session_tool(args, false),
        "list_managed_sessions" => orch_tools::list_managed_sessions_tool(args),
        "update_goal_status" => orch_tools::update_goal_status_tool(args),
        "orchestrator_report" => orch_tools::orchestrator_report_tool(args),
        other => return cancel(&format!("session-control does not provide tool '{other}'")),
    };

    match result {
        Ok(value) => {
            // Count control actions taken by an orchestrator brain toward its
            // action total + activity feed (the orchestrator tools log
            // themselves; awaiting_approval gates don't count as actions).
            let mutating = matches!(
                tool.as_str(),
                "interrupt_session"
                    | "terminate_agent"
                    | "clear_session"
                    | "send_message"
                    | "send_image"
            );
            let awaiting =
                value.get("status").and_then(|s| s.as_str()) == Some("awaiting_approval");
            if mutating && !awaiting {
                orch_tools::attribute_action(&tool, target.as_deref());
            }
            allow(value)
        }
        Err(reason) => cancel(&reason),
    }
}

fn allow(value: serde_json::Value) -> String {
    serde_json::json!({ "verdict": "allow", "payload": value }).to_string()
}

fn cancel(reason: &str) -> String {
    serde_json::json!({ "verdict": "cancel", "reason": reason }).to_string()
}

fn skip() -> String {
    serde_json::json!({ "verdict": "skip" }).to_string()
}
