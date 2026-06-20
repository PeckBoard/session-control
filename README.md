# Peckboard Session-Control Plugin

A Peckboard WASM plugin that lets one session take **full control of another
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

All tools take a `session_id`. Discover ids with the core `list_sessions` /
`search_sessions` MCP tools.

## Boundary

There is **no folder/project boundary** — any session can be controlled by id.
This power is granted by the operator approving the plugin's single privileged
permission, `session_control`. The actual actions run host-side in Peckboard
core (cancel/interrupt/clear orchestration, message dispatch); this plugin only
shapes the tool I/O.

## Permissions

- `provide_mcp_tools` — declare the MCP tools above.
- `session_control` — call the core session-control host functions
  (`peckboard_interrupt_session`, `peckboard_terminate_agent`,
  `peckboard_clear_session`, `peckboard_send_message`).

## Build

```bash
./build.sh
# → target/wasm32-unknown-unknown/release/peckboard_session_control_plugin.wasm
```

Install via the Peckboard plugin registry (id `session-control`), or drop the
`.wasm` into `<dataDir>/plugins/session-control.wasm` and restart.
