//! FFI layer: the Peckboard core session-control host functions this plugin
//! calls, as thin JSON wrappers.
//!
//! Every host function is JSON-string-in / JSON-string-out and returns an
//! `{"error": "..."}` envelope instead of trapping; [`call_host`] turns that
//! envelope into an `Err(String)` so tool code can use `?`.
//!
//! The FFI exists only on `wasm32` (the Extism host imports are unavailable on
//! the host target used for `cargo test`), so host builds get an
//! `unimplemented!()` stub the tests never reach.

/// Which host function a [`call_host`] targets. Each is gated host-side on the
/// `session_control` permission this plugin declares (see `manifest.rs`).
pub enum HostFn {
    InterruptSession,
    TerminateAgent,
    ClearSession,
    SendMessage,
    ListSessions,
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

// Host-target stub so the crate links for `cargo test` (no host imports exist
// off-wasm; no test calls a host-backed tool).
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
