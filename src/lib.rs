//! Peckboard session-control plugin (WASM / Extism).
//!
//! Lets one session take full control of another, provided to Peckboard as MCP
//! tools via the `mcp.tool.invoke` hook:
//!
//! - **interrupt_session** — stop the target's in-flight turn (cancel the
//!   current run without deleting anything).
//! - **terminate_agent** — kill the target's long-lived agent process; the next
//!   message starts a fresh one.
//! - **clear_session** — wipe the target's transcript / todos / attachments and
//!   reset its conversation.
//! - **send_message** — deliver a text message to the target and resume it.
//! - **send_image** — deliver an image (base64) to the target as an attachment,
//!   with an optional caption.
//!
//! There is intentionally no boundary: any session can be controlled by id (the
//! operator grants this by approving the plugin's `session_control` permission).
//! Use the core `list_sessions` / `search_sessions` MCP tools to discover the
//! session ids to act on. The actual actions are performed host-side in
//! Peckboard core's session-control host functions; this plugin declares the
//! `session_control` permission and shapes the tool I/O.
//!
//! ## Plugin interface
//!
//! Core expects four exports (`peckboard/src/plugin/manager.rs`):
//! - `manifest` — declares the hook handled and the MCP tools provided.
//! - `init` — called once on load with the plugin's config block; a no-op here.
//! - `handle` — called per hook with `{ "hook", "payload" }`; returns a Verdict.
//! - `shutdown` — teardown; a no-op here.

#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code, unused_imports))]

mod control;
mod host;
mod manifest;

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
        match call.hook.as_str() {
            "mcp.tool.invoke" => Ok(handle_invoke(call.payload)),
            _ => Ok(skip()),
        }
    }
}

/// The `{ "hook", "payload" }` envelope core passes to `handle`.
#[derive(Debug, Deserialize)]
struct HookCall {
    hook: String,
    #[serde(default)]
    payload: serde_json::Value,
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

    let result: Result<serde_json::Value, String> = match tool.as_str() {
        "interrupt_session" => control::interrupt_session_tool(args),
        "terminate_agent" => control::terminate_agent_tool(args),
        "clear_session" => control::clear_session_tool(args),
        "send_message" => control::send_message_tool(args),
        "send_image" => control::send_image_tool(args),
        other => return cancel(&format!("session-control does not provide tool '{other}'")),
    };

    match result {
        Ok(value) => allow(value),
        Err(reason) => cancel(&reason),
    }
}

// ── Verdict helpers (mirror core's `Verdict` enum) ────────────────────

fn allow(value: serde_json::Value) -> String {
    serde_json::json!({ "verdict": "allow", "payload": value }).to_string()
}

fn cancel(reason: &str) -> String {
    serde_json::json!({ "verdict": "cancel", "reason": reason }).to_string()
}

fn skip() -> String {
    serde_json::json!({ "verdict": "skip" }).to_string()
}
