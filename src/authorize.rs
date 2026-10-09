//! User approval gates for session-control tools.
//!
//! - Cross-folder session control: same-folder targets proceed immediately;
//!   cross-folder targets ask the caller’s user (*Approve once* / *Approve
//!   always* / *Deny*), mirroring common-tools `run_command`. Grants are
//!   written into this plugin’s document store so the Peckboard host can
//!   enforce them.
//! - Orchestrator management ([`ensure_user_approved`]): every gated create /
//!   update / delete asks *Approve once* / *Deny* — no standing grant, since
//!   each one starts or reshapes autonomous agents.
use serde_json::{Value, json};

use crate::host::{HostFn, call_host};

pub const ALWAYS_COLLECTION: &str = "cross_folder_always";
pub const ONCE_COLLECTION: &str = "cross_folder_once";
pub const PENDING_COLLECTION: &str = "cross_folder_pending";

const APPROVE_ONCE: &str = "Approve once";
const APPROVE_ALWAYS: &str = "Approve always";
const DENY: &str = "Deny";

pub enum Gate {
    /// Caller may invoke the host control action now.
    Ready,
    /// User prompt emitted; return this JSON as the tool result.
    Awaiting(Value),
}

/// Ensure cross-folder control is allowed (or ask). On `Ready`, any needed
/// Once/Always grant has already been written for the host to see.
pub fn ensure_cross_folder_allowed(target_id: &str, action_label: &str) -> Result<Gate, String> {
    let scope = call_host(HostFn::CallerScope, &json!({}))?;
    let caller_session = scope
        .get("session_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let caller_folder = scope
        .get("folder_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let target = lookup_session(target_id)?;
    let target_folder = target
        .get("folder_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let target_name = target
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(target_id);

    // Self-control or same folder → free.
    if caller_session.as_deref() == Some(target_id) {
        return Ok(Gate::Ready);
    }
    if let Some(cf) = caller_folder.as_deref()
        && cf == target_folder
    {
        return Ok(Gate::Ready);
    }

    let Some(caller_id) = caller_session else {
        return Err("cross-folder session control requires a caller session \
             (invoke from a session tool call)"
            .into());
    };

    // Persisted Always for this caller session.
    if store_has_always(&caller_id)? {
        return Ok(Gate::Ready);
    }

    let pending_key = format!("{caller_id}|{target_id}");
    match store_get(PENDING_COLLECTION, &pending_key)? {
        None => {
            let token = new_token(&caller_id, target_id);
            store_put(
                PENDING_COLLECTION,
                &pending_key,
                json!({ "token": token, "target_id": target_id, "action": action_label }),
            )?;
            let question = format!(
                "Allow this session to {action_label} session \"{target_name}\" \
                 ({target_id}) in another folder?"
            );
            call_host(
                HostFn::AskUser,
                &json!({
                    "question": question,
                    "options": [APPROVE_ONCE, APPROVE_ALWAYS, DENY],
                    "token": token,
                }),
            )?;
            Ok(Gate::Awaiting(json!({
                "status": "awaiting_approval",
                "message": format!(
                    "Waiting for user approval to {action_label} cross-folder session \
                     \"{target_name}\". Re-call this tool after they answer \
                     (Approve once / Approve always / Deny)."
                ),
                "target_session_id": target_id,
            })))
        }
        Some(pending) => {
            let token = pending
                .get("token")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "corrupt pending approval record".to_string())?;
            let ans = call_host(HostFn::GetAnswer, &json!({ "token": token }))?;
            let status = ans.get("status").and_then(|v| v.as_str()).unwrap_or("");
            if status != "answered" {
                return Ok(Gate::Awaiting(json!({
                    "status": "awaiting_approval",
                    "message": format!(
                        "Still waiting for user approval to {action_label} \
                         cross-folder session \"{target_name}\"."
                    ),
                    "target_session_id": target_id,
                })));
            }
            let rejected = ans
                .get("rejected")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let answer = ans.get("answer").and_then(|v| v.as_str()).unwrap_or("");
            let _ = store_delete(PENDING_COLLECTION, &pending_key);

            if rejected || answer == DENY {
                return Err(format!(
                    "the user denied {action_label} on cross-folder session \"{target_name}\""
                ));
            }
            if answer.starts_with(APPROVE_ALWAYS) {
                store_put(ALWAYS_COLLECTION, &caller_id, json!({ "approved": true }))?;
                return Ok(Gate::Ready);
            }
            if answer.starts_with(APPROVE_ONCE) {
                let once_key = format!("{caller_id}|{target_id}");
                store_put(ONCE_COLLECTION, &once_key, json!({ "approved": true }))?;
                return Ok(Gate::Ready);
            }
            Err(format!(
                "the user did not approve {action_label} on \"{target_name}\" (answer: {answer})"
            ))
        }
    }
}

pub const ORCH_PENDING_COLLECTION: &str = "orch_approval_pending";

/// Ask the calling session's user to approve one orchestrator-management
/// action. `action_key` names the action + target (one pending ask per
/// caller per key); `fingerprint` is the exact request — a re-call with
/// different arguments drops the old ask and asks afresh, so an approval
/// never carries over to a request the user did not see. Returns `Ready`
/// only after an *Approve once* answer to this very request.
pub fn ensure_user_approved(
    action_key: &str,
    fingerprint: &str,
    question: &str,
) -> Result<Gate, String> {
    let scope = call_host(HostFn::CallerScope, &json!({}))?;
    let Some(caller_id) = scope
        .get("session_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
    else {
        return Err(
            "this orchestrator change needs user approval, which requires a caller \
             session (invoke from a session tool call)"
                .into(),
        );
    };

    let pending_key = format!("{caller_id}|{action_key}");
    let ask = || -> Result<Gate, String> {
        let token = new_token(&caller_id, action_key);
        store_put(
            ORCH_PENDING_COLLECTION,
            &pending_key,
            json!({ "token": token, "fingerprint": fingerprint }),
        )?;
        call_host(
            HostFn::AskUser,
            &json!({
                "question": question,
                "options": [APPROVE_ONCE, DENY],
                "token": token,
            }),
        )?;
        Ok(Gate::Awaiting(json!({
            "status": "awaiting_approval",
            "message": "Waiting for the user to approve this orchestrator change. Re-call \
                        this tool with the SAME arguments after they answer \
                        (Approve once / Deny).",
        })))
    };

    let Some(pending) = store_get(ORCH_PENDING_COLLECTION, &pending_key)? else {
        return ask();
    };
    if pending.get("fingerprint").and_then(|v| v.as_str()) != Some(fingerprint) {
        return ask();
    }
    let token = pending
        .get("token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "corrupt pending approval record".to_string())?;
    let ans = call_host(HostFn::GetAnswer, &json!({ "token": token }))?;
    match ans.get("status").and_then(|v| v.as_str()).unwrap_or("") {
        "answered" => {}
        // The question is gone (e.g. the session was cleared) — ask again.
        "unknown" => return ask(),
        _ => {
            return Ok(Gate::Awaiting(json!({
                "status": "awaiting_approval",
                "message": "Still waiting for the user to approve this orchestrator change.",
            })));
        }
    }
    let _ = store_delete(ORCH_PENDING_COLLECTION, &pending_key);
    let rejected = ans
        .get("rejected")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let answer = ans.get("answer").and_then(|v| v.as_str()).unwrap_or("");
    if !rejected && answer.starts_with(APPROVE_ONCE) {
        Ok(Gate::Ready)
    } else {
        Err(format!(
            "the user did not approve this orchestrator change (answer: {answer})"
        ))
    }
}

fn lookup_session(target_id: &str) -> Result<Value, String> {
    // list_all_sessions with query=id; prefer exact id match.
    let listed = call_host(HostFn::ListSessions, &json!({ "query": target_id }))?;
    let sessions = listed
        .get("sessions")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    sessions
        .into_iter()
        .find(|s| s.get("session_id").and_then(|v| v.as_str()) == Some(target_id))
        .ok_or_else(|| format!("session not found: {target_id}"))
}

fn store_has_always(caller_id: &str) -> Result<bool, String> {
    match store_get(ALWAYS_COLLECTION, caller_id)? {
        Some(v) => Ok(v.get("approved").and_then(|a| a.as_bool()).unwrap_or(true)),
        None => Ok(false),
    }
}

fn store_put(collection: &str, key: &str, data: Value) -> Result<(), String> {
    call_host(
        HostFn::StorePut,
        &json!({ "collection": collection, "key": key, "data": data }),
    )?;
    Ok(())
}

fn store_get(collection: &str, key: &str) -> Result<Option<Value>, String> {
    let out = call_host(
        HostFn::StoreGet,
        &json!({ "collection": collection, "key": key }),
    )?;
    // Host returns {"value": null} or {"value": {...}} / decoded doc.
    match out.get("value") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => Ok(Some(v.clone())),
    }
}

fn store_delete(collection: &str, key: &str) -> Result<(), String> {
    let _ = call_host(
        HostFn::StoreDelete,
        &json!({ "collection": collection, "key": key }),
    );
    Ok(())
}

fn new_token(caller: &str, target: &str) -> String {
    // Opaque correlation id. Avoid `SystemTime` — it traps on
    // wasm32-unknown-unknown without WASI. A counter + inputs is enough
    // to distinguish concurrent asks in one plugin instance.
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("sc-{caller}-{target}-{n}")
}
