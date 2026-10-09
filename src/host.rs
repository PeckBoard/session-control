//! FFI layer: Peckboard core host functions this plugin calls.
//!
//! Every host function is JSON-string-in / JSON-string-out and returns an
//! `{"error": "..."}` envelope instead of trapping; [`call_host`] turns that
//! envelope into an `Err(String)` so tool code can use `?`.
//!
//! The FFI exists only on `wasm32` (Extism host imports are unavailable on
//! the host target used for `cargo test`), so host builds get an
//! `unimplemented!()` stub the tests never reach.

/// Which host function a [`call_host`] targets.
pub enum HostFn {
    InterruptSession,
    TerminateAgent,
    ClearSession,
    SendMessage,
    /// Read another session's event tail (cross-folder gated like the
    /// mutating actions; needs peckboard core with
    /// `peckboard_read_session_events`).
    ReadSessionEvents,
    ListSessions,
    CallerScope,
    AskUser,
    GetAnswer,
    StorePut,
    StoreGet,
    StoreList,
    StoreDelete,
    /// Atomic put-if-absent — the cross-instance lease behind
    /// `state::with_engine_lock` (0.4.3, needs peckboard ≥ 0.0.189).
    StorePutIfAbsent,
    // Orchestrator surface (0.4.0): unattended control + pickers.
    OrchestrateSend,
    OrchestrateCreateSession,
    OrchestrateSetPrompt,
    OrchestrateSessionState,
    /// Stop a session's agent, transcript kept (0.5.0) — the orchestrator
    /// delete stops its brain through this.
    OrchestrateTerminate,
    CreateSession,
    ListFolders,
    ListModels,
}

#[cfg(target_arch = "wasm32")]
mod imp {
    use super::HostFn;
    use extism_pdk::*;

    #[host_fn]
    extern "ExtismHost" {
        fn peckboard_interrupt_session(input: String) -> String;
        fn peckboard_terminate_agent(input: String) -> String;
        fn peckboard_clear_session(input: String) -> String;
        fn peckboard_read_session_events(input: String) -> String;
        fn peckboard_list_all_sessions(input: String) -> String;
        fn peckboard_send_message(input: String) -> String;
        fn peckboard_caller_scope(input: String) -> String;
        fn peckboard_ask_user(input: String) -> String;
        fn peckboard_store_put(input: String) -> String;
        fn peckboard_store_get(input: String) -> String;
        fn peckboard_store_list(input: String) -> String;
        fn peckboard_store_delete(input: String) -> String;
        fn peckboard_store_put_if_absent(input: String) -> String;
        fn peckboard_get_answer(input: String) -> String;
        fn peckboard_orchestrate_send(input: String) -> String;
        fn peckboard_orchestrate_create_session(input: String) -> String;
        fn peckboard_orchestrate_set_prompt(input: String) -> String;
        fn peckboard_orchestrate_session_state(input: String) -> String;
        fn peckboard_orchestrate_terminate(input: String) -> String;
        fn peckboard_create_session(input: String) -> String;
        fn peckboard_list_folders(input: String) -> String;
        fn peckboard_list_models(input: String) -> String;
    }

    /// Invoke a host function with a JSON value, parse its JSON reply, and
    /// surface an `{"error": ...}` envelope (or a trap) as `Err(String)`.
    pub fn call_host(
        which: HostFn,
        input: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let s = input.to_string();
        let out = unsafe {
            match which {
                HostFn::InterruptSession => peckboard_interrupt_session(s),
                HostFn::TerminateAgent => peckboard_terminate_agent(s),
                HostFn::ClearSession => peckboard_clear_session(s),
                HostFn::ReadSessionEvents => peckboard_read_session_events(s),
                HostFn::SendMessage => peckboard_send_message(s),
                HostFn::ListSessions => peckboard_list_all_sessions(s),
                HostFn::CallerScope => peckboard_caller_scope(s),
                HostFn::AskUser => peckboard_ask_user(s),
                HostFn::GetAnswer => peckboard_get_answer(s),
                HostFn::StorePut => peckboard_store_put(s),
                HostFn::StoreGet => peckboard_store_get(s),
                HostFn::StoreList => peckboard_store_list(s),
                HostFn::StoreDelete => peckboard_store_delete(s),
                HostFn::StorePutIfAbsent => peckboard_store_put_if_absent(s),
                HostFn::OrchestrateSend => peckboard_orchestrate_send(s),
                HostFn::OrchestrateCreateSession => peckboard_orchestrate_create_session(s),
                HostFn::OrchestrateSetPrompt => peckboard_orchestrate_set_prompt(s),
                HostFn::OrchestrateSessionState => peckboard_orchestrate_session_state(s),
                HostFn::OrchestrateTerminate => peckboard_orchestrate_terminate(s),
                HostFn::CreateSession => peckboard_create_session(s),
                HostFn::ListFolders => peckboard_list_folders(s),
                HostFn::ListModels => peckboard_list_models(s),
            }
        }
        .map_err(|e| e.to_string())?;
        parse_envelope(&out)
    }

    fn parse_envelope(out: &str) -> Result<serde_json::Value, String> {
        let v: serde_json::Value =
            serde_json::from_str(out).map_err(|e| format!("host returned invalid json: {e}"))?;
        if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
            return Err(err.to_string());
        }
        Ok(v)
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use super::HostFn;

    #[cfg(test)]
    pub fn call_host(
        which: HostFn,
        input: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        super::fake::call(which, input)
    }

    #[cfg(not(test))]
    pub fn call_host(
        _which: HostFn,
        _input: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        unimplemented!("host calls are only available on wasm32")
    }
}

/// In-memory stand-in for the host, per test thread: the document store,
/// the caller scope, and `ask_user` / `get_answer` (questions are recorded;
/// a test answers one with [`fake::answer`]). `orchestrate_terminate` calls
/// are recorded ([`fake::terminated`]) and can be made to fail
/// ([`fake::fail_terminate`]). Every other host call is an `{ok: true}`
/// no-op.
#[cfg(all(test, not(target_arch = "wasm32")))]
pub mod fake {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    use serde_json::{Value, json};

    use super::HostFn;

    thread_local! {
        static STORE: RefCell<BTreeMap<(String, String), Value>> =
            const { RefCell::new(BTreeMap::new()) };
        static SCOPE: RefCell<Value> = RefCell::new(json!({}));
        /// token → (question, answer once answered).
        static QUESTIONS: RefCell<BTreeMap<String, (String, Option<String>)>> =
            const { RefCell::new(BTreeMap::new()) };
        /// Session ids `orchestrate_terminate` was called on.
        static TERMINATED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
        static FAIL_TERMINATE: RefCell<bool> = const { RefCell::new(false) };
    }

    pub fn set_caller(session_id: &str, folder_id: &str) {
        SCOPE.with(|s| {
            *s.borrow_mut() = json!({ "session_id": session_id, "folder_id": folder_id })
        });
    }

    pub fn terminated() -> Vec<String> {
        TERMINATED.with(|t| t.borrow().clone())
    }

    pub fn fail_terminate(fail: bool) {
        FAIL_TERMINATE.with(|f| *f.borrow_mut() = fail);
    }

    /// The (token, question) pairs asked so far that have no answer yet.
    pub fn open_questions() -> Vec<(String, String)> {
        QUESTIONS.with(|q| {
            q.borrow()
                .iter()
                .filter(|(_, (_, a))| a.is_none())
                .map(|(t, (qs, _))| (t.clone(), qs.clone()))
                .collect()
        })
    }

    pub fn answer(token: &str, answer: &str) {
        QUESTIONS.with(|q| {
            if let Some(entry) = q.borrow_mut().get_mut(token) {
                entry.1 = Some(answer.to_string());
            }
        });
    }

    fn key(input: &Value) -> (String, String) {
        let s = |k: &str| {
            input
                .get(k)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        (s("collection"), s("key"))
    }

    pub fn call(which: HostFn, input: &Value) -> Result<Value, String> {
        Ok(match which {
            HostFn::StorePut => {
                STORE.with(|s| s.borrow_mut().insert(key(input), input["data"].clone()));
                json!({ "ok": true })
            }
            HostFn::StorePutIfAbsent => {
                let acquired = STORE.with(|s| match s.borrow_mut().entry(key(input)) {
                    std::collections::btree_map::Entry::Vacant(e) => {
                        e.insert(input["data"].clone());
                        true
                    }
                    std::collections::btree_map::Entry::Occupied(_) => false,
                });
                json!({ "acquired": acquired })
            }
            HostFn::StoreGet => {
                let v = STORE.with(|s| s.borrow().get(&key(input)).cloned());
                json!({ "value": v })
            }
            HostFn::StoreDelete => {
                STORE.with(|s| s.borrow_mut().remove(&key(input)));
                json!({ "ok": true })
            }
            HostFn::StoreList => {
                let coll = key(input).0;
                let items: Vec<Value> = STORE.with(|s| {
                    s.borrow()
                        .iter()
                        .filter(|((c, _), _)| *c == coll)
                        .map(|((_, k), v)| json!({ "key": k, "value": v }))
                        .collect()
                });
                json!({ "items": items })
            }
            HostFn::CallerScope => SCOPE.with(|s| s.borrow().clone()),
            HostFn::AskUser => {
                let token = input["token"].as_str().unwrap_or("").to_string();
                let question = input["question"].as_str().unwrap_or("").to_string();
                QUESTIONS.with(|q| q.borrow_mut().insert(token, (question, None)));
                json!({ "ok": true })
            }
            HostFn::GetAnswer => {
                let token = input["token"].as_str().unwrap_or("");
                match QUESTIONS.with(|q| q.borrow().get(token).cloned()) {
                    None => json!({ "status": "unknown" }),
                    Some((_, None)) => json!({ "status": "pending" }),
                    Some((_, Some(a))) => {
                        json!({ "status": "answered", "answer": a, "rejected": false })
                    }
                }
            }
            HostFn::OrchestrateTerminate => {
                if FAIL_TERMINATE.with(|f| *f.borrow()) {
                    return Err("live control unavailable".into());
                }
                let sid = input["session_id"].as_str().unwrap_or("").to_string();
                TERMINATED.with(|t| t.borrow_mut().push(sid));
                json!({ "ok": true })
            }
            _ => json!({ "ok": true }),
        })
    }
}

pub use imp::call_host;
