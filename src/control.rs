//! Session-control tools. Mutating tools gate cross-folder targets through
//! [`authorize::ensure_cross_folder_allowed`] before calling the host.

use serde_json::{Value, json};

use crate::authorize::{self, Gate};
use crate::host::{HostFn, call_host};

/// Pull a required, non-empty string argument.
fn require_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("'{key}' is required"))
}

/// Optional string argument, defaulting to `default`.
fn opt_str(args: &Value, key: &str, default: &str) -> String {
    args.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or(default)
        .to_string()
}

fn gated_action(
    target_id: &str,
    action_label: &str,
    then: impl FnOnce() -> Result<Value, String>,
) -> Result<Value, String> {
    match authorize::ensure_cross_folder_allowed(target_id, action_label)? {
        Gate::Awaiting(payload) => Ok(payload),
        Gate::Ready => then(),
    }
}

pub fn interrupt_session_tool(args: Value) -> Result<Value, String> {
    let session_id = require_str(&args, "session_id")?;
    gated_action(&session_id, "interrupt", || {
        call_host(
            HostFn::InterruptSession,
            &json!({ "session_id": session_id }),
        )
    })
}

pub fn terminate_agent_tool(args: Value) -> Result<Value, String> {
    let session_id = require_str(&args, "session_id")?;
    gated_action(&session_id, "terminate the agent of", || {
        call_host(HostFn::TerminateAgent, &json!({ "session_id": session_id }))
    })
}

pub fn clear_session_tool(args: Value) -> Result<Value, String> {
    let session_id = require_str(&args, "session_id")?;
    gated_action(&session_id, "clear", || {
        call_host(HostFn::ClearSession, &json!({ "session_id": session_id }))
    })
}

pub fn send_message_tool(args: Value) -> Result<Value, String> {
    let session_id = require_str(&args, "session_id")?;
    let text = require_str(&args, "text")?;
    gated_action(&session_id, "send a message to", || {
        call_host(
            HostFn::SendMessage,
            &json!({ "session_id": session_id, "text": text }),
        )
    })
}

pub fn find_session_tool(args: Value) -> Result<Value, String> {
    // Discovery stays folder-blind — no approval prompt.
    let query = opt_str(&args, "query", "");
    call_host(HostFn::ListSessions, &json!({ "query": query }))
}

pub fn send_image_tool(args: Value) -> Result<Value, String> {
    let session_id = require_str(&args, "session_id")?;
    let data_base64 = require_str(&args, "image_base64")?;
    let mime_type = require_str(&args, "mime_type")?;
    let filename = opt_str(&args, "filename", "image");
    let caption = opt_str(&args, "caption", "");
    gated_action(&session_id, "send an image to", || {
        call_host(
            HostFn::SendMessage,
            &json!({
                "session_id": session_id,
                "text": caption,
                "attachments": [{
                    "filename": filename,
                    "mime_type": mime_type,
                    "data_base64": data_base64,
                }],
            }),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn require_str_rejects_missing_and_blank() {
        let args = json!({ "session_id": "   " });
        assert!(require_str(&args, "session_id").is_err());
        assert!(require_str(&args, "nope").is_err());
        let ok = json!({ "session_id": "s1" });
        assert_eq!(require_str(&ok, "session_id").unwrap(), "s1");
    }
}
