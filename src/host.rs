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
    ListSessions,
    CallerScope,
    AskUser,
    GetAnswer,
    StorePut,
    StoreGet,
    StoreList,
    StoreDelete,
    // Orchestrator surface (0.4.0): unattended control + pickers.
    OrchestrateSend,
    OrchestrateCreateSession,
    OrchestrateSetPrompt,
    OrchestrateSessionState,
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
        fn peckboard_list_all_sessions(input: String) -> String;
        fn peckboard_send_message(input: String) -> String;
        fn peckboard_caller_scope(input: String) -> String;
        fn peckboard_ask_user(input: String) -> String;
        fn peckboard_get_answer(input: String) -> String;
        fn peckboard_store_put(input: String) -> String;
        fn peckboard_store_get(input: String) -> String;
        fn peckboard_store_list(input: String) -> String;
        fn peckboard_store_delete(input: String) -> String;
        fn peckboard_orchestrate_send(input: String) -> String;
        fn peckboard_orchestrate_create_session(input: String) -> String;
        fn peckboard_orchestrate_set_prompt(input: String) -> String;
        fn peckboard_orchestrate_session_state(input: String) -> String;
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
                HostFn::SendMessage => peckboard_send_message(s),
                HostFn::ListSessions => peckboard_list_all_sessions(s),
                HostFn::CallerScope => peckboard_caller_scope(s),
                HostFn::AskUser => peckboard_ask_user(s),
                HostFn::GetAnswer => peckboard_get_answer(s),
                HostFn::StorePut => peckboard_store_put(s),
                HostFn::StoreGet => peckboard_store_get(s),
                HostFn::StoreList => peckboard_store_list(s),
                HostFn::StoreDelete => peckboard_store_delete(s),
                HostFn::OrchestrateSend => peckboard_orchestrate_send(s),
                HostFn::OrchestrateCreateSession => peckboard_orchestrate_create_session(s),
                HostFn::OrchestrateSetPrompt => peckboard_orchestrate_set_prompt(s),
                HostFn::OrchestrateSessionState => peckboard_orchestrate_session_state(s),
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
mod imp {
    use super::HostFn;

    pub fn call_host(
        _which: HostFn,
        _input: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        unimplemented!("host calls are only available on wasm32")
    }
}

pub use imp::call_host;
