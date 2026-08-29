# Peckboard Session-Control Plugin

A Peckboard WASM plugin with two surfaces:

1. **Session control** — let one session take control of another: interrupt
   it, terminate its agent, clear it, send it messages or images. Served to
   agents as MCP tools via the `mcp.tool.invoke` hook.
2. **Orchestrators** (0.4.0) — goal-driven configs that autonomously drive
   requirements to completion through a dedicated "brain" session, managed
   from a global **Orchestrators** sidebar page.

## Orchestrators

An orchestrator holds a **goal** (the requirements), a trigger prompt
template, and triggers: an every-N-minutes schedule, watched sessions (fire
when one goes idle via `session.agent.ended`), and an autonomy **watchdog**
that re-engages the brain while the goal is not done. Fires deliver the
rendered prompt to the brain session (created lazily in the configured
folder with a standing system prompt carrying the goal, its powers, and the
enforced standards — development, testing, UX / ui-gauge, plus custom
rules).

Brain-facing tools: `create_session` (capped, auto-watched, optional hat),
`assign_hat` (a named scope of responsibility written into a session's
system prompt), `watch_session` / `unwatch_session`,
`list_managed_sessions`, `update_goal_status` (state, percent, note, and a
required `eta_minutes` estimate for the whole goal; `done` stops the
watchdog), and `orchestrator_report` (activity feed entry).

Guards: hourly fire cap (auto-pause), per-orchestrator cooldown,
busy-coalescing of triggers, consecutive-failure backoff (auto-disable),
and a global Pause-all kill switch. The page shows per-orchestrator action
counts, fires, pending triggers, goal progress + ETA with drift, the
activity feed, managed sessions with hats, a dry-run prompt preview, and
Run now.

Engine timing rides the core `timer.tick` hook (~30s); "now" is always
host-supplied because wasm32-unknown-unknown has no clock.
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
Upgrading to 0.4.0 adds hooks (`timer.tick`, `session.agent.ended`,
`session.message.before`, `http.request.before`, `http.request.authed`) and
permissions, so Peckboard will re-prompt for plugin approval.

## Permissions

- `provide_mcp_tools` — declare the MCP tools above.
- `session_control` — call the core session-control host functions.
- `ask_user` — prompt for cross-folder approval.
- `data_store` — orchestrator records, activity feeds, grants.
- `session_orchestrate` — unattended fire path: send / create / set prompt /
  session state from lifecycle hooks (standing grant, folder-blind).
- `session_write` — the caller-scoped `create_session` tool.
- `models_read` — the model picker on the page.
- `user_authority` + `contribute_sidebar` — the Orchestrators page.
## Build

```bash
./build.sh
# → target/wasm32-unknown-unknown/release/peckboard_session_control_plugin.wasm
```

Install via the Peckboard plugin registry (id `session-control`), or drop the
`.wasm` into `<dataDir>/plugins/session-control.wasm` and restart.
