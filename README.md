# Peckboard Session-Control Plugin

A Peckboard WASM plugin that lets one session take **control of another
session** — interrupt it, terminate its agent, clear it, and send it messages
or images. Served to agents as MCP tools via the `mcp.tool.invoke` hook.

## Tools

| Tool | What it does |
| ---- | ------------ |
| `interrupt_session` | Stop the target's in-flight turn (cancel the current run); history kept. |
| `terminate_agent` | Kill the target's long-lived agent process; next message starts fresh. |
| `clear_session` | Wipe the target's events, todos, and attachments and reset its conversation. **Destructive.** |
| `send_message` | Deliver a text message to the target and resume it. |
| `send_image` | Deliver a base64 image (with optional caption) to the target as an attachment. |
| `find_session` | List sessions across every folder/project (no boundary) to resolve a target id; optional substring `query`. |

Every mutating action takes a `session_id`. Discover ids with this plugin's own
`find_session` tool, or the core `list_sessions` / `search_sessions` MCP tools.

## Boundary

| Target | Behavior |
| ------ | -------- |
| Same folder as the caller | Act immediately |
| Other folder | Ask the user: **Approve once** / **Approve always** / **Deny** |
| After Always | This controlling session may act on any foreign-folder session without asking again |
| `find_session` | Always folder-blind (discovery only; no prompt) |

Peckboard core enforces the same Always/Once grants on the host functions, so
skipping the ask path cannot bypass the gate. Always grants are stored in this
plugin's document store (`cross_folder_always`); there is no Settings revoke UI
yet — clear plugin data or remove the grant keys to reset.

Upgrading to 0.3.0 adds the `ask_user` and `data_store` permissions, so
Peckboard will re-prompt for plugin approval.

## Permissions

- `provide_mcp_tools` — declare the MCP tools above.
- `session_control` — call the core session-control host functions.
- `ask_user` — prompt for cross-folder approval.
- `data_store` — persist Always / Once / pending approval records.

## Build

```bash
./build.sh
# → target/wasm32-unknown-unknown/release/peckboard_session_control_plugin.wasm
```

Install via the Peckboard plugin registry (id `session-control`), or drop the
`.wasm` into `<dataDir>/plugins/session-control.wasm` and restart.
