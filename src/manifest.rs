//! Plugin manifest: identity, hooks, MCP tools, UI surfaces, and host
//! permissions.

/// Inline SVG (lucide "network") for the sidebar entry; rendered sandboxed.
const ICON: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\" fill=\"none\" \
stroke=\"currentColor\" stroke-width=\"2\" stroke-linecap=\"round\" stroke-linejoin=\"round\">\
<rect x=\"16\" y=\"16\" width=\"6\" height=\"6\" rx=\"1\"/><rect x=\"2\" y=\"16\" width=\"6\" height=\"6\" rx=\"1\"/>\
<rect x=\"9\" y=\"2\" width=\"6\" height=\"6\" rx=\"1\"/><path d=\"M5 16v-3a1 1 0 0 1 1-1h12a1 1 0 0 1 1 1v3\"/>\
<path d=\"M12 12V8\"/></svg>";

/// Build the manifest JSON string returned by the `manifest` export.
pub fn manifest_json() -> String {
    let manifest = serde_json::json!({
        "description": env!("CARGO_PKG_DESCRIPTION"),
        "version": env!("CARGO_PKG_VERSION"),
        "repository": env!("CARGO_PKG_REPOSITORY"),

        "hooks": [
            "mcp.tool.invoke",
            // Orchestrator engine: the ~30s scheduler tick, watched sessions
            // going idle, and (observed only) user turns starting.
            "timer.tick",
            "session.agent.ended",
            "session.message.before",
            // The Orchestrators page: public HTML shell + authed JSON routes.
            "http.request.before",
            "http.request.authed",
        ],

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
            },
            {
                "name": "create_session",
                "title": "Create a session for more work",
                "description": "Create a new session in your folder when more work is required, optionally wearing a hat (a named scope of responsibility written into its system prompt). When you are an orchestrator brain session the new session is auto-watched — you are re-engaged when its turns end — and it counts against your max_sessions_created cap. Send it work with send_message.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Display name for the new session." },
                        "model": { "type": "string", "description": "Optional model id (provider-prefixed, e.g. \"claude:...\"); defaults to the instance default." },
                        "hat": { "type": "string", "description": "Optional hat name, e.g. \"QA\", \"Frontend\", \"Reviewer\"." },
                        "responsibilities": { "type": "string", "description": "What the hat covers — required to be meaningful when 'hat' is set." }
                    },
                    "required": ["name"],
                    "additionalProperties": false
                }
            },
            {
                "name": "assign_hat",
                "title": "Assign a hat to a session",
                "description": "Give a session a hat: a named scope of responsibility written into its system prompt (takes effect on its next turn). Use it to divide a goal between sessions — e.g. \"Backend\", \"QA\", \"Docs\" — so each stays inside its mandate.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string", "description": "The session to assign the hat to." },
                        "hat": { "type": "string", "description": "The hat name." },
                        "responsibilities": { "type": "string", "description": "The scope of responsibility this hat covers." }
                    },
                    "required": ["session_id", "hat", "responsibilities"],
                    "additionalProperties": false
                }
            },
            {
                "name": "watch_session",
                "title": "Watch a session",
                "description": "Orchestrator brains only: add a session to your watch list, so you are re-engaged whenever its agent turn ends.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string", "description": "The session to watch." }
                    },
                    "required": ["session_id"],
                    "additionalProperties": false
                }
            },
            {
                "name": "unwatch_session",
                "title": "Stop watching a session",
                "description": "Orchestrator brains only: remove a session from your watch list (including auto-watched created sessions).",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string", "description": "The session to stop watching." }
                    },
                    "required": ["session_id"],
                    "additionalProperties": false
                }
            },
            {
                "name": "list_managed_sessions",
                "title": "List managed sessions",
                "description": "Orchestrator brains only: your watched + created sessions with their hats, busy state, and your current goal status. Use it to decide what needs attention before acting.",
                "input_schema": { "type": "object", "properties": {}, "required": [], "additionalProperties": false }
            },
            {
                "name": "update_goal_status",
                "title": "Update goal status + ETA",
                "description": "Orchestrator brains only: report progress on your goal. 'state' is in_progress | blocked | done; 'eta_minutes' — your estimate of minutes until the ENTIRE requirements are implemented — is REQUIRED while not done (re-estimate on every call; it is shown to the user with drift over time). state=done stops the autonomy watchdog: call it only when every requirement is implemented and verified.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "state": { "type": "string", "enum": ["in_progress", "blocked", "done"], "description": "Goal state." },
                        "note": { "type": "string", "description": "One-line status note shown on the Orchestrators page." },
                        "percent": { "type": "integer", "minimum": 0, "maximum": 100, "description": "Optional completion percentage." },
                        "eta_minutes": { "type": "integer", "minimum": 0, "description": "Estimated minutes until the entire goal is done. Required unless state=done." }
                    },
                    "required": ["state"],
                    "additionalProperties": false
                }
            },
            {
                "name": "orchestrator_report",
                "title": "Report orchestrator activity",
                "description": "Orchestrator brains only: log a one-line summary of what you just did. Shown in the Orchestrators page activity feed — call it after every burst of work so the user can follow along.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "summary": { "type": "string", "description": "What you did, one line." }
                    },
                    "required": ["summary"],
                    "additionalProperties": false
                }
            }
        ],

        // The Orchestrators page: global sidebar entry → public HTML shell →
        // authed JSON under /api/plugin-ui/session-control/*.
        "sidebar_items": [
            { "id": "orchestrators", "label": "Orchestrators", "icon": ICON, "path": "/plugin-api/v1/orchestrators" }
        ],
        "http_routes": ["GET /plugin-api/v1/orchestrators"],
        "ui_routes": [
            "GET /api/plugin-ui/session-control/orchestrators",
            "POST /api/plugin-ui/session-control/orchestrators",
            "POST /api/plugin-ui/session-control/orchestrators/:id",
            "POST /api/plugin-ui/session-control/orchestrators/:id/delete",
            "POST /api/plugin-ui/session-control/orchestrators/:id/run",
            "POST /api/plugin-ui/session-control/orchestrators/:id/dry-run",
            "POST /api/plugin-ui/session-control/orchestrators/:id/pause",
            "POST /api/plugin-ui/session-control/pause-all",
            "GET /api/plugin-ui/session-control/pickers"
        ],

        "permissions": [
            "provide_mcp_tools",
            "session_control",
            "ask_user",
            "data_store",
            // Orchestrators (0.4.0):
            "session_orchestrate", // unattended fire: send/create/prompt/state from lifecycle hooks
            "session_write",       // create_session tool (caller-scoped twin)
            "models_read",         // model picker on the page
            "user_authority",      // authed page routes
            "contribute_sidebar"   // the Orchestrators sidebar entry
        ],
    });
    manifest.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_declares_orchestrator_surface() {
        let m: serde_json::Value = serde_json::from_str(&manifest_json()).unwrap();
        let hooks: Vec<&str> = m["hooks"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|h| h.as_str())
            .collect();
        for h in [
            "timer.tick",
            "session.agent.ended",
            "http.request.before",
            "http.request.authed",
            "mcp.tool.invoke",
        ] {
            assert!(hooks.contains(&h), "missing hook {h}");
        }
        let perms: Vec<&str> = m["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|p| p.as_str())
            .collect();
        for p in [
            "session_orchestrate",
            "session_write",
            "user_authority",
            "contribute_sidebar",
            "session_control",
        ] {
            assert!(perms.contains(&p), "missing permission {p}");
        }
        let tools: Vec<&str> = m["mcp_tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        for t in [
            "create_session",
            "assign_hat",
            "watch_session",
            "unwatch_session",
            "list_managed_sessions",
            "update_goal_status",
            "orchestrator_report",
            "interrupt_session",
            "find_session",
        ] {
            assert!(tools.contains(&t), "missing tool {t}");
        }
        assert_eq!(
            m["sidebar_items"][0]["path"],
            "/plugin-api/v1/orchestrators"
        );
    }
}
