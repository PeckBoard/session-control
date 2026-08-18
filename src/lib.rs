//! Peckboard session-control plugin (WASM / Extism).
//!
//! Lets one session control another via MCP tools (`mcp.tool.invoke`):
//!
//! - **interrupt_session** / **terminate_agent** / **clear_session** /
//!   **send_message** / **send_image** — mutating actions.
//! - **find_session** — folder-blind discovery (no approval prompt).
//!
//! Same-folder targets run immediately. Cross-folder mutating actions ask the
//! user (*Approve once* / *Approve always* / *Deny*); Always is remembered for
//! that controlling session. The Peckboard host enforces the same grants so a
//! bypass cannot skip the gate.
//!
//! ## Plugin interface
//!
//! Core expects four exports (`peckboard/src/plugin/manager.rs`):
//! - `manifest` — declares the hook handled and the MCP tools provided.
//! - `init` — called once on load with the plugin's config block; a no-op here.
//! - `handle` — called per hook with `{ "hook", "payload" }`; returns a Verdict.
//! - `shutdown` — teardown; a no-op here.

#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code, unused_imports))]

mod authorize;
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
        "find_session" => control::find_session_tool(args),
        other => return cancel(&format!("session-control does not provide tool '{other}'")),
    };

    match result {
        Ok(value) => allow(value),
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
