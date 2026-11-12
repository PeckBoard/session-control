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
    /// User prompt emitted (or still open); return this JSON as the tool result.
    Awaiting(Value),
    /// The user said no to this request; return this JSON (`status: denied`)
    /// as the tool result. Reported once — the next call asks afresh.
    Denied(Value),
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
    // Emit a fresh prompt, replacing any pending record.
    let ask = || -> Result<Gate, String> {
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
        if let Err(e) = call_host(
            HostFn::AskUser,
            &json!({
                "question": question,
                "options": [APPROVE_ONCE, APPROVE_ALWAYS, DENY],
                "token": token,
            }),
        ) {
            // No prompt reached the user: don't leave a record that would
            // make every later call "still wait" on it.
            let _ = store_delete(PENDING_COLLECTION, &pending_key);
            return Err(e);
        }
        Ok(Gate::Awaiting(json!({
            "status": "awaiting_approval",
            "message": format!(
                "Waiting for user approval to {action_label} cross-folder session \
                 \"{target_name}\". Re-call this tool after they answer \
                 (Approve once / Approve always / Deny)."
            ),
            "target_session_id": target_id,
        })))
    };

    let Some(pending) = store_get(PENDING_COLLECTION, &pending_key)? else {
        return ask();
    };
    // A corrupt record can't be resolved — ask afresh rather than fail forever.
    let Some(token) = pending.get("token").and_then(|v| v.as_str()) else {
        return ask();
    };
    let ans = call_host(HostFn::GetAnswer, &json!({ "token": token }))?;
    match ans.get("status").and_then(|v| v.as_str()).unwrap_or("") {
        "answered" => {}
        // No question with this token is in the caller's session: the prompt
        // never landed or is gone (session cleared, emit lost). Nothing is on
        // screen to answer, so waiting would never end — ask again.
        "unknown" => return ask(),
        _ => {
            return Ok(Gate::Awaiting(json!({
                "status": "awaiting_approval",
                "message": format!(
                    "Still waiting for user approval to {action_label} \
                     cross-folder session \"{target_name}\"."
                ),
                "target_session_id": target_id,
            })));
        }
    }
    // Decided: consume the one-shot pending record so the next call asks
    // afresh instead of replaying this answer.
    let _ = store_delete(PENDING_COLLECTION, &pending_key);
    let rejected = ans
        .get("rejected")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let answer = ans.get("answer").and_then(|v| v.as_str()).unwrap_or("");
    if !rejected && answer.starts_with(APPROVE_ALWAYS) {
        store_put(ALWAYS_COLLECTION, &caller_id, json!({ "approved": true }))?;
        return Ok(Gate::Ready);
    }
    if !rejected && answer.starts_with(APPROVE_ONCE) {
        let once_key = format!("{caller_id}|{target_id}");
        store_put(ONCE_COLLECTION, &once_key, json!({ "approved": true }))?;
        return Ok(Gate::Ready);
    }
    let message = if rejected {
        format!(
            "The user dismissed the approval prompt to {action_label} session \
             \"{target_name}\" without approving. Don't retry unless the user asks."
        )
    } else {
        format!(
            "The user denied permission to {action_label} session \"{target_name}\". \
             Don't retry unless the user asks."
        )
    };
    Ok(Gate::Denied(json!({
        "status": "denied",
        "message": message,
        "target_session_id": target_id,
    })))
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

/// Collection holding the persisted approval-token sequence (one doc).
pub const TOKEN_SEQ_COLLECTION: &str = "approval_token_seq";

fn new_token(caller: &str, target: &str) -> String {
    // Opaque correlation id. Avoid `SystemTime` — it traps on
    // wasm32-unknown-unknown without WASI. Tokens must never repeat one an
    // older prompt in the session's event log already carries:
    // `peckboard_get_answer` resolves a token to the FIRST question holding
    // it, so a reused token reads that old question (its stale answer, or
    // "pending" forever) instead of the prompt the user is looking at. A
    // per-instance counter alone repeats after every restart / reload and
    // across pooled instances (manifest `concurrency`), so it is paired with
    // a sequence persisted in the store, which survives both.
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let persisted = store_get(TOKEN_SEQ_COLLECTION, "next")
        .ok()
        .flatten()
        .and_then(|v| v.get("n").and_then(|n| n.as_u64()))
        .unwrap_or(0)
        + 1;
    let _ = store_put(TOKEN_SEQ_COLLECTION, "next", json!({ "n": persisted }));
    format!("sc-{caller}-{target}-{persisted}-{n}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::fake;

    fn gate() -> Value {
        match ensure_cross_folder_allowed("t1", "send a message to").unwrap() {
            Gate::Ready => json!({ "status": "ready" }),
            Gate::Awaiting(v) | Gate::Denied(v) => v,
        }
    }

    fn open() -> Vec<String> {
        fake::open_questions().into_iter().map(|(t, _)| t).collect()
    }

    /// Deny is reported once as `denied`; the next call asks afresh with a
    /// new token; a pending record whose prompt is gone re-asks instead of
    /// waiting forever.
    #[test]
    fn deny_reports_denied_once_then_asks_afresh() {
        fake::set_caller("c1", "fa");
        fake::add_session("t1", "Target", "fb");

        assert_eq!(gate()["status"], "awaiting_approval");
        let first = open();
        assert_eq!(first.len(), 1);
        let r = gate();
        assert!(
            r["message"].as_str().unwrap().contains("Still waiting"),
            "{r}"
        );
        assert_eq!(open(), first, "no second prompt while one is open");

        fake::answer(&first[0], DENY);
        let r = gate();
        assert_eq!(r["status"], "denied", "{r}");
        assert!(r["message"].as_str().unwrap().contains("Target"), "{r}");

        let r = gate();
        assert_eq!(r["status"], "awaiting_approval", "{r}");
        assert!(
            !r["message"].as_str().unwrap().contains("Still waiting"),
            "{r}"
        );
        let second = open();
        assert_eq!(second.len(), 1, "a fresh prompt is open");
        assert_ne!(second, first, "fresh token");

        // The prompt vanished (session cleared / emit lost) → re-ask.
        fake::forget_question(&second[0]);
        let r = gate();
        assert_eq!(r["status"], "awaiting_approval", "{r}");
        let third = open();
        assert_eq!(third.len(), 1, "stale pending → new prompt");
        assert_ne!(third, second);

        fake::answer(&third[0], APPROVE_ONCE);
        assert_eq!(gate()["status"], "ready");
        assert!(
            store_get(ONCE_COLLECTION, "c1|t1").unwrap().is_some(),
            "once grant written for the host"
        );
    }
}
