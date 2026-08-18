//! Plugin manifest: identity, hooks, MCP tools, and host permissions.

/// Build the manifest JSON string returned by the `manifest` export.
pub fn manifest_json() -> String {
    let manifest = serde_json::json!({
        "description": env!("CARGO_PKG_DESCRIPTION"),
        "version": env!("CARGO_PKG_VERSION"),
        "repository": env!("CARGO_PKG_REPOSITORY"),

        "hooks": ["mcp.tool.invoke"],

        "mcp_tools": [
            {
                "name": "interrupt_session",
                "title": "Interrupt a session",
                "description": "Stop another session's in-flight agent turn (cancel the current run) without deleting the session or its history. Same-folder targets run immediately; cross-folder targets ask the user (Approve once / Approve always / Deny) and return status awaiting_approval until answered — then re-call with the same session_id. Discover ids with find_session / list_sessions.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string", "description": "The session to interrupt." }
                    },
                    "required": ["session_id"],
                    "additionalProperties": false
                }
            },
            {
                "name": "terminate_agent",
                "title": "Terminate a session's agent",
                "description": "Kill another session's long-lived agent process. Unlike interrupt (which stops the current turn), this tears the process down entirely; the next message to that session starts a fresh agent. The transcript is preserved. Same-folder immediate; cross-folder asks Approve once / Always / Deny.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string", "description": "The session whose agent to terminate." }
                    },
                    "required": ["session_id"],
                    "additionalProperties": false
                }
            },
            {
                "name": "clear_session",
                "title": "Clear a session",
                "description": "Wipe another session: cancel any running turn, delete its entire event history and todos, drop its attachments, and reset its conversation so it starts fresh. Destructive and irreversible. Same-folder immediate; cross-folder asks Approve once / Always / Deny.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string", "description": "The session to clear." }
                    },
                    "required": ["session_id"],
                    "additionalProperties": false
                }
            },
            {
                "name": "send_message",
                "title": "Send a message to a session",
                "description": "Deliver a text message to another session as if it were a user message, and resume that session. Same-folder immediate; cross-folder asks Approve once / Always / Deny (re-call after the user answers).",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string", "description": "The session to message." },
                        "text": { "type": "string", "description": "The message text to deliver." }
                    },
                    "required": ["session_id", "text"],
                    "additionalProperties": false
                }
            },
            {
                "name": "send_image",
                "title": "Send an image to a session",
                "description": "Deliver an image to another session as an attachment on a (resumed) user message, with an optional caption. Same-folder immediate; cross-folder asks Approve once / Always / Deny.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string", "description": "The session to send the image to." },
                        "image_base64": { "type": "string", "description": "The image bytes, base64-encoded (standard, not data-URL)." },
                        "mime_type": { "type": "string", "description": "The image mime type, e.g. \"image/png\" or \"image/jpeg\"." },
                        "filename": { "type": "string", "description": "Optional file name for the attachment (default \"image\")." },
                        "caption": { "type": "string", "description": "Optional text delivered alongside the image." }
                    },
                    "required": ["session_id", "image_base64", "mime_type"],
                    "additionalProperties": false
                }
            },
            {
                "name": "find_session",
                "title": "Find sessions across all folders",
                "description": "List sessions anywhere in this Peckboard instance -- every folder and project -- so you can resolve a target for the other session-control tools. Discovery does not require approval; acting on a cross-folder match does. Returns session_id, name, folder_id, project_id, conversation_id, model, worker/expert flags, card_id, and last_activity, newest first. Optional 'query' filters by case-insensitive substring.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Optional case-insensitive substring to match against session id, name, conversation_id, model, or folder_id. Omit to list every session." }
                    },
                    "required": [],
                    "additionalProperties": false
                }
            }
        ],

        "permissions": [
            "provide_mcp_tools",
            "session_control",
            "ask_user",
            "data_store"
        ],
    });
    manifest.to_string()
}
