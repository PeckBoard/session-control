//! The orchestrator trigger engine.
//!
//! Three ways in:
//! - `timer.tick` (~30s): due schedules, autonomy watchdogs, pending-trigger
//!   flushes, and the stored clock everything else reads.
//! - `session.agent.ended`: a watched session went idle → fire (or coalesce);
//!   the brain's own turn ending flushes coalesced triggers.
//! - the management page: manual Run now / dry-run.
//!
//! A fire delivers the orchestrator's rendered prompt to its brain session
//! via the `session_orchestrate` host quartet. Guards, in order: global
//! pause, per-orchestrator enable/pause, hourly fire cap (auto-pause),
//! cooldown, brain-busy coalescing, and consecutive-failure backoff
//! (auto-disable).

use serde_json::{Value, json};

use crate::host::{HostFn, call_host};
use crate::state::{self, ETA_HISTORY_CAP, MAX_CONSECUTIVE_FAILURES, Orchestrator, PendingTrigger};

/// What set a fire off — rendered into `{{trigger}}` and the activity feed.
#[derive(Debug, Clone)]
pub struct Trigger {
    pub kind: String, // schedule | watchdog | session_idle | manual | coalesced
    pub session_id: Option<String>,
    pub session_name: Option<String>,
    pub outcome: Option<String>,
    /// Coalesced pending triggers folded into this fire.
    pub pending: Vec<PendingTrigger>,
}

impl Trigger {
    pub fn simple(kind: &str) -> Self {
        Self {
            kind: kind.to_string(),
            session_id: None,
            session_name: None,
            outcome: None,
            pending: Vec::new(),
        }
    }

    pub fn session_idle(session_id: &str, session_name: &str, outcome: &str) -> Self {
        Self {
            kind: "session_idle".into(),
            session_id: Some(session_id.to_string()),
            session_name: Some(session_name.to_string()),
            outcome: Some(outcome.to_string()),
            pending: Vec::new(),
        }
    }
}

// ── Hook entry points ─────────────────────────────────────────────────

/// `timer.tick`: store the clock, then walk every orchestrator once. The
/// walk runs under the cross-instance engine lease — contended means another
/// instance is already mid-mutation, and the next tick (~30s) retries.
pub fn on_timer_tick(payload: Value) -> Result<Value, String> {
    let now = payload
        .get("now")
        .and_then(|v| v.as_str())
        .ok_or("timer.tick payload missing 'now'")?
        .to_string();
    state::set_clock(&now);
    if state::global_paused() {
        return Ok(json!({ "ok": true, "paused": true }));
    }
    let walked = state::try_with_engine_lock(|| {
        let mut fired = 0u32;
        for mut o in state::list_orchestrators()? {
            if !o.enabled || o.paused {
                continue;
            }
            let mut changed = false;

            // Pending flush first: coalesced triggers deliver as soon as the
            // brain is idle again, ahead of new schedule/watchdog fires.
            if !o.pending_triggers.is_empty() && brain_is_idle(&o, &now) {
                let pending = std::mem::take(&mut o.pending_triggers);
                let mut t = Trigger::simple("coalesced");
                t.pending = pending;
                fired += u32::from(fire(&mut o, t, &now));
                changed = true;
            } else if schedule_due(&o, &now) {
                let t = Trigger::simple("schedule");
                // Advance the slot even when the fire is skipped by a guard —
                // a blocked slot shouldn't burst later.
                o.stats.next_due_at = o
                    .schedule
                    .every_minutes
                    .map(|m| state::add_minutes(&now, m as i64));
                fired += u32::from(fire(&mut o, t, &now));
                changed = true;
            } else if watchdog_due(&o, &now) {
                fired += u32::from(fire(&mut o, Trigger::simple("watchdog"), &now));
                changed = true;
            }

            if changed {
                state::save_orchestrator(&o)?;
            }
        }
        Ok(fired)
    })?;
    match walked {
        Some(fired) => Ok(json!({ "ok": true, "fired": fired })),
        None => Ok(json!({ "ok": true, "skipped": "busy" })),
    }
}

/// `session.agent.ended`: clear the busy mark, flush the brain's pending
/// triggers, and fire watchers of the ended session.
pub fn on_agent_ended(payload: Value) -> Result<Value, String> {
    let session_id = payload
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if session_id.is_empty() {
        return Ok(json!({ "ok": true }));
    }
    state::mark_idle(&session_id);
    let now = state::clock().unwrap_or_default();
    if now.is_empty() || state::global_paused() {
        // No tick yet (or paused): the next timer.tick picks the work up.
        return Ok(json!({ "ok": true }));
    }
    let session_name = payload
        .get("session_name")
        .and_then(|v| v.as_str())
        .unwrap_or(&session_id)
        .to_string();
    let outcome = payload
        .get("outcome")
        .and_then(|v| v.as_str())
        .unwrap_or("completed")
        .to_string();

    // Folder of the ended session, fetched once and only if some
    // orchestrator actually watches folders.
    let mut ended_folder: Option<Option<String>> = None;

    // Contended lease → skip: the busy mark is already cleared, pending
    // triggers flush on the next tick, and a missed watched-idle fire is
    // covered by the watchdog. Never blocks the hook.
    let walked = state::try_with_engine_lock(|| {
        for mut o in state::list_orchestrators()? {
            if !o.enabled || o.paused {
                continue;
            }
            let is_brain = o.session_id.as_deref() == Some(session_id.as_str());
            if is_brain {
                // Brain finished a turn → deliver anything that queued up.
                if !o.pending_triggers.is_empty() {
                    let pending = std::mem::take(&mut o.pending_triggers);
                    let mut t = Trigger::simple("coalesced");
                    t.pending = pending;
                    fire(&mut o, t, &now);
                    state::save_orchestrator(&o)?;
                }
                continue;
            }
            let watched = o.watches_session(&session_id) || {
                if o.watch.folders.is_empty() {
                    false
                } else {
                    let folder = ended_folder
                        .get_or_insert_with(|| session_folder(&session_id))
                        .clone();
                    folder.map(|f| o.watches_folder(&f)).unwrap_or(false)
                }
            };
            if watched {
                fire(
                    &mut o,
                    Trigger::session_idle(&session_id, &session_name, &outcome),
                    &now,
                );
                state::save_orchestrator(&o)?;
            }
        }
        Ok(())
    })?;
    Ok(json!({ "ok": true, "skipped": walked.is_none().then_some("busy") }))
}

/// `session.message.before` (observed, never rewritten): a user turn is
/// starting → best-effort busy mark so the engine coalesces instead of
/// stacking prompts.
pub fn on_message_before(payload: Value) -> Result<(), String> {
    if let Some(sid) = payload.get("session_id").and_then(|v| v.as_str()) {
        let now = state::clock().unwrap_or_default();
        if !now.is_empty() {
            state::mark_busy(sid, &now);
        }
    }
    Ok(())
}

// ── Manual controls (management page) ─────────────────────────────────

/// "Run now": fire regardless of schedule; cooldown and caps still apply.
pub fn run_now(id: &str) -> Result<Value, String> {
    let now = state::clock().ok_or("no engine clock yet — wait for the first timer tick")?;
    state::try_with_engine_lock(|| {
        let mut o = state::load_orchestrator(id)?.ok_or(format!("orchestrator not found: {id}"))?;
        let sent = fire(&mut o, Trigger::simple("manual"), &now);
        state::save_orchestrator(&o)?;
        Ok(json!({ "ok": true, "sent": sent }))
    })?
    .ok_or_else(|| state::BUSY_MSG.to_string())
}

/// "Test fire": render the prompt with current context; deliver nothing.
pub fn dry_run(id: &str) -> Result<Value, String> {
    let now = state::clock().unwrap_or_else(|| "1970-01-01T00:00:00Z".into());
    let o = state::load_orchestrator(id)?.ok_or(format!("orchestrator not found: {id}"))?;
    let t = Trigger::simple("dry-run");
    Ok(json!({
        "rendered_prompt": render_prompt(&o, &t, &now),
        "standing_prompt": standing_prompt(&o),
    }))
}

// ── Guards + fire ─────────────────────────────────────────────────────

fn schedule_due(o: &Orchestrator, now: &str) -> bool {
    let Some(_every) = o.schedule.every_minutes else {
        return false;
    };
    match &o.stats.next_due_at {
        // Never scheduled yet → due immediately.
        None => true,
        Some(due) => state::seconds_between(due, now)
            .map(|s| s >= 0)
            .unwrap_or(true),
    }
}

/// The autonomy loop: while the goal isn't done, re-engage an idle brain
/// after `watchdog_minutes` of silence — no user input required.
fn watchdog_due(o: &Orchestrator, now: &str) -> bool {
    if o.goal_status.state == "done" || o.watchdog_minutes == 0 {
        return false;
    }
    let since = match &o.stats.last_fired_at {
        None => return true, // never fired → produce the initial ETA + plan
        Some(t) => t,
    };
    state::seconds_between(since, now)
        .map(|s| s >= (o.watchdog_minutes as i64) * 60)
        .unwrap_or(false)
        && brain_is_idle(o, now)
}

fn brain_is_idle(o: &Orchestrator, now: &str) -> bool {
    match &o.session_id {
        Some(sid) => !state::is_busy(sid, now),
        None => true,
    }
}

fn in_cooldown(o: &Orchestrator, now: &str) -> bool {
    match &o.stats.last_fired_at {
        Some(t) => state::seconds_between(t, now)
            .map(|s| s < o.cooldown_secs as i64)
            .unwrap_or(false),
        None => false,
    }
}

/// Prune the rolling window and check the hourly cap.
fn over_fire_cap(o: &mut Orchestrator, now: &str) -> bool {
    o.stats.fire_times.retain(|t| {
        state::seconds_between(t, now)
            .map(|s| s < 3600)
            .unwrap_or(false)
    });
    o.stats.fire_times.len() as u32 >= o.caps.max_fires_per_hour
}

/// Deliver one fire. Returns true when a prompt was actually sent. Mutates
/// `o` (stats, log, pending, error state) — the caller persists it.
fn fire(o: &mut Orchestrator, trigger: Trigger, now: &str) -> bool {
    if over_fire_cap(o, now) {
        o.paused = true;
        o.push_log(
            now,
            "cap_hit",
            format!(
                "{} fires in the last hour reached max_fires_per_hour={} — auto-paused",
                o.stats.fire_times.len(),
                o.caps.max_fires_per_hour
            ),
        );
        return false;
    }
    if in_cooldown(o, now) {
        // Event triggers queue; timer triggers just wait for the next tick.
        if trigger.kind == "session_idle" || trigger.kind == "coalesced" {
            queue_pending(o, trigger, now);
        }
        return false;
    }
    // Ensure the brain exists (create lazily, with its standing prompt).
    if let Err(e) = ensure_brain(o, now) {
        return record_failure(o, now, &e);
    }
    let brain = o.session_id.clone().expect("ensure_brain sets session_id");
    if state::is_busy(&brain, now) {
        queue_pending(o, trigger, now);
        o.push_log(now, "skipped_busy", "brain busy — trigger coalesced");
        return false;
    }
    let prompt = render_prompt(o, &trigger, now);
    match call_host(
        HostFn::OrchestrateSend,
        &json!({ "session_id": brain, "text": prompt }),
    ) {
        Ok(_) => {
            o.consecutive_failures = 0;
            o.error = None;
            o.stats.fires += 1;
            o.stats.actions += 1;
            o.stats.last_fired_at = Some(now.to_string());
            o.stats.fire_times.push(now.to_string());
            o.last_rendered_prompt = prompt;
            state::mark_busy(&brain, now);
            o.push_log(now, "fired", fire_detail(&trigger));
            true
        }
        Err(e) => record_failure(o, now, &e),
    }
}

fn fire_detail(t: &Trigger) -> String {
    match t.kind.as_str() {
        "session_idle" => format!(
            "watched session \"{}\" went idle ({})",
            t.session_name.as_deref().unwrap_or("?"),
            t.outcome.as_deref().unwrap_or("?")
        ),
        "coalesced" => format!("delivered {} coalesced trigger(s)", t.pending.len().max(1)),
        other => format!("trigger: {other}"),
    }
}

fn queue_pending(o: &mut Orchestrator, trigger: Trigger, now: &str) {
    // Coalesce: unwrap an already-coalesced trigger back into its parts,
    // and drop duplicates for the same session.
    let mut parts = if trigger.pending.is_empty() {
        vec![PendingTrigger {
            ts: now.to_string(),
            kind: trigger.kind.clone(),
            session_id: trigger.session_id.clone(),
            session_name: trigger.session_name.clone(),
            outcome: trigger.outcome.clone(),
        }]
    } else {
        trigger.pending
    };
    parts.retain(|p| {
        p.session_id.is_none()
            || !o
                .pending_triggers
                .iter()
                .any(|q| q.session_id == p.session_id)
    });
    o.pending_triggers.extend(parts);
    const PENDING_CAP: usize = 20;
    if o.pending_triggers.len() > PENDING_CAP {
        let drop = o.pending_triggers.len() - PENDING_CAP;
        o.pending_triggers.drain(..drop);
    }
}

/// Failure bookkeeping; auto-disables after [`MAX_CONSECUTIVE_FAILURES`].
fn record_failure(o: &mut Orchestrator, now: &str, err: &str) -> bool {
    o.consecutive_failures += 1;
    o.error = Some(err.to_string());
    if o.consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
        o.enabled = false;
        o.push_log(
            now,
            "backoff",
            format!(
                "{n} consecutive failures — auto-disabled. Last error: {err}",
                n = o.consecutive_failures
            ),
        );
    } else {
        o.push_log(now, "error", err.to_string());
    }
    false
}

// ── Brain session ─────────────────────────────────────────────────────

/// Make sure the brain session exists; (re)create it when missing/deleted.
fn ensure_brain(o: &mut Orchestrator, now: &str) -> Result<(), String> {
    if let Some(sid) = &o.session_id {
        let st = call_host(
            HostFn::OrchestrateSessionState,
            &json!({ "session_id": sid }),
        )?;
        if st.get("exists").and_then(|e| e.as_bool()).unwrap_or(false) {
            return Ok(());
        }
        o.push_log(
            now,
            "error",
            format!("brain session {sid} is gone — recreating"),
        );
        o.session_id = None;
    }
    let created = call_host(
        HostFn::OrchestrateCreateSession,
        &json!({
            "folder_id": o.folder_id,
            "name": format!("{} (orchestrator)", o.name),
            "model": o.model,
            "system_prompt": standing_prompt(o),
        }),
    )?;
    let sid = created
        .get("session")
        .and_then(|s| s.get("id"))
        .and_then(|v| v.as_str())
        .ok_or("create_session returned no session id")?
        .to_string();
    o.push_log(
        now,
        "created_session",
        format!("brain session created: {sid}"),
    );
    o.session_id = Some(sid);
    Ok(())
}

/// The brain's standing system prompt: goal, powers, operating rules,
/// standards. Written at creation; refreshed via the page's update route.
pub fn standing_prompt(o: &Orchestrator) -> String {
    format!(
        "You are the brain of the Peckboard orchestrator \"{name}\". You work fully \
autonomously: no user is watching, and you are re-engaged automatically whenever a \
watched session goes idle, on schedule, and by a watchdog while the goal below is \
not done.\n\n\
## Goal (drive these requirements to completion)\n{goal}\n\n\
## Your powers (MCP tools)\n\
- create_session: spawn a new session when more work is required (it is auto-watched).\n\
- assign_hat: give a session a hat — a named scope of responsibility written into its \
system prompt.\n\
- send_message / interrupt_session / terminate_agent: direct, unblock, or stop sessions.\n\
- watch_session / unwatch_session: adjust which sessions re-engage you when they go idle.\n\
- list_managed_sessions: your sessions, their hats, and busy state.\n\
- update_goal_status: report state (in_progress|blocked|done), percent, note, AND \
eta_minutes — your current estimate of minutes until the ENTIRE goal is implemented.\n\
- orchestrator_report: log a one-line summary of what you just did (shown to the user).\n\n\
## Operating rules\n\
1. FIRST ACTION on your first engagement: break the goal into remaining work and call \
update_goal_status with an initial eta_minutes estimate before doing anything else.\n\
2. Re-estimate eta_minutes on every update_goal_status call; keep it honest.\n\
3. After every burst of work, call orchestrator_report with what happened.\n\
4. Delegate real work to sessions you create; give each a hat. Review results when \
they go idle.\n\
5. Call update_goal_status with state=done ONLY when every requirement is implemented \
and verified.\n{standards}",
        name = o.name,
        goal = if o.goal.trim().is_empty() {
            "(no goal set)"
        } else {
            o.goal.trim()
        },
        standards = standards_text(o),
    )
}

/// The standards block for the standing prompt and `{{standards}}`.
pub fn standards_text(o: &Orchestrator) -> String {
    let s = &o.standards;
    if !s.dev && !s.testing && !s.ux && s.custom.trim().is_empty() {
        return String::new();
    }
    let mut lines =
        vec!["\n## Standards you must enforce before accepting any work as done".to_string()];
    if s.dev {
        lines.push(
            "- Development: code builds cleanly, passes the project's linters/formatters, and \
follows the repo's existing conventions."
                .into(),
        );
    }
    if s.testing {
        lines.push(
            "- Testing: unit/integration tests for new behaviour and an end-to-end test per \
user-visible flow, all passing (run the project's verification script when it has one)."
                .into(),
        );
    }
    if s.ux {
        lines.push(
            "- UX: UI changes are verified in a real browser, and when the ui-gauge plugin is \
installed, scored against the user's design baselines (ui_gauge_rubric → score the \
screenshots → ui_gauge_score) and at or above the bar; subpar scores create follow-up \
work you must drive."
                .into(),
        );
    }
    if !s.custom.trim().is_empty() {
        lines.push(format!("- Custom: {}", s.custom.trim()));
    }
    lines.push(
        "Work that misses a standard is NOT done: create follow-up sessions/cards and keep \
driving until it passes."
            .into(),
    );
    lines.join("\n")
}

// ── Prompt rendering ──────────────────────────────────────────────────

pub fn render_prompt(o: &Orchestrator, t: &Trigger, now: &str) -> String {
    let base = if o.prompt.trim().is_empty() {
        "Trigger: {{trigger}}. Review the state of your goal and managed sessions, act on \
anything that needs you, and report via orchestrator_report + update_goal_status."
            .to_string()
    } else {
        o.prompt.clone()
    };
    let pending_txt = if t.pending.is_empty() {
        String::from("none")
    } else {
        t.pending
            .iter()
            .map(|p| {
                format!(
                    "{} \"{}\" ({})",
                    p.kind,
                    p.session_name.as_deref().unwrap_or("?"),
                    p.outcome.as_deref().unwrap_or("-")
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    };
    let eta_txt = match &o.goal_status.eta {
        Some(e) => format!(
            "{} min remaining (projected {}, estimated at {})",
            e.minutes_remaining, e.projected_at, e.updated_at
        ),
        None => String::from("no estimate yet — produce one via update_goal_status"),
    };
    let watched_busy = o
        .watch
        .sessions
        .iter()
        .chain(o.created_sessions.iter())
        .filter(|s| state::is_busy(s, now))
        .count();
    let trigger_txt = match t.kind.as_str() {
        "session_idle" => format!(
            "watched session \"{}\" finished its turn ({})",
            t.session_name.as_deref().unwrap_or("?"),
            t.outcome.as_deref().unwrap_or("?")
        ),
        "coalesced" => format!(
            "{} events while you were busy: {pending_txt}",
            t.pending.len()
        ),
        other => other.to_string(),
    };
    base.replace("{{trigger}}", &trigger_txt)
        .replace("{{session_name}}", t.session_name.as_deref().unwrap_or(""))
        .replace("{{outcome}}", t.outcome.as_deref().unwrap_or(""))
        .replace("{{goal}}", o.goal.trim())
        .replace("{{watched_busy_count}}", &watched_busy.to_string())
        .replace("{{pending}}", &pending_txt)
        .replace("{{eta}}", &eta_txt)
        .replace("{{standards}}", &standards_text(o))
}

/// Session → folder, via the orchestrate state host fn (works in any dispatch).
fn session_folder(session_id: &str) -> Option<String> {
    let st = call_host(
        HostFn::OrchestrateSessionState,
        &json!({ "session_id": session_id }),
    )
    .ok()?;
    st.get("session")?
        .get("folder_id")?
        .as_str()
        .map(str::to_string)
}

/// Append an ETA sample, capped.
pub fn push_eta_history(o: &mut Orchestrator, now: &str, minutes_remaining: u64) {
    o.eta_history.push(json!({
        "ts": now,
        "minutes_remaining": minutes_remaining,
        "percent": o.goal_status.percent,
    }));
    if o.eta_history.len() > ETA_HISTORY_CAP {
        let drop = o.eta_history.len() - ETA_HISTORY_CAP;
        o.eta_history.drain(..drop);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn orch() -> Orchestrator {
        serde_json::from_value(json!({
            "id": "o1", "name": "Ship v2", "folder_id": "f1",
            "goal": "Build the widget",
        }))
        .unwrap()
    }

    #[test]
    fn schedule_due_never_scheduled_is_due() {
        let mut o = orch();
        o.schedule.every_minutes = Some(30);
        assert!(schedule_due(&o, "2026-08-29T10:00:00Z"));
        o.stats.next_due_at = Some("2026-08-29T10:30:00Z".into());
        assert!(!schedule_due(&o, "2026-08-29T10:29:00Z"));
        assert!(schedule_due(&o, "2026-08-29T10:30:00Z"));
    }

    #[test]
    fn no_schedule_is_never_due() {
        let o = orch();
        assert!(!schedule_due(&o, "2026-08-29T10:00:00Z"));
    }

    #[test]
    fn watchdog_stops_at_done_and_respects_window() {
        let mut o = orch();
        // Never fired → due (initial engagement).
        assert!(watchdog_due(&o, "2026-08-29T10:00:00Z"));
        o.stats.last_fired_at = Some("2026-08-29T10:00:00Z".into());
        assert!(!watchdog_due(&o, "2026-08-29T10:10:00Z")); // 10 < 15 min
        o.goal_status.state = "done".into();
        assert!(!watchdog_due(&o, "2026-08-29T11:00:00Z"));
        o.goal_status.state = "in_progress".into();
        o.watchdog_minutes = 0;
        assert!(!watchdog_due(&o, "2026-08-29T11:00:00Z")); // 0 disables
    }

    #[test]
    fn cooldown_window_math() {
        let mut o = orch();
        assert!(!in_cooldown(&o, "2026-08-29T10:00:00Z"));
        o.stats.last_fired_at = Some("2026-08-29T10:00:00Z".into());
        assert!(in_cooldown(&o, "2026-08-29T10:00:30Z"));
        assert!(!in_cooldown(&o, "2026-08-29T10:01:00Z"));
    }

    #[test]
    fn fire_cap_prunes_and_pauses() {
        let mut o = orch();
        o.caps.max_fires_per_hour = 2;
        o.stats.fire_times = vec![
            "2026-08-29T08:00:00Z".into(), // stale, pruned
            "2026-08-29T09:30:00Z".into(),
            "2026-08-29T09:45:00Z".into(),
        ];
        assert!(over_fire_cap(&mut o, "2026-08-29T10:00:00Z"));
        assert_eq!(o.stats.fire_times.len(), 2);
        o.caps.max_fires_per_hour = 3;
        assert!(!over_fire_cap(&mut o, "2026-08-29T10:00:00Z"));
    }

    #[test]
    fn queue_pending_dedupes_by_session_and_caps() {
        let mut o = orch();
        let now = "2026-08-29T10:00:00Z";
        queue_pending(&mut o, Trigger::session_idle("s1", "A", "completed"), now);
        queue_pending(&mut o, Trigger::session_idle("s1", "A", "completed"), now);
        queue_pending(&mut o, Trigger::session_idle("s2", "B", "crashed"), now);
        assert_eq!(o.pending_triggers.len(), 2);
    }

    #[test]
    fn render_prompt_substitutes_vars() {
        let mut o = orch();
        o.prompt = "T={{trigger}} G={{goal}} E={{eta}} P={{pending}}".into();
        let t = Trigger::session_idle("s1", "Builder", "completed");
        let out = render_prompt(&o, &t, "2026-08-29T10:00:00Z");
        assert!(out.contains("Builder"), "{out}");
        assert!(out.contains("G=Build the widget"), "{out}");
        assert!(out.contains("no estimate yet"), "{out}");
        assert!(out.contains("P=none"), "{out}");
    }

    #[test]
    fn standing_prompt_carries_goal_rules_and_standards() {
        let o = orch();
        let sp = standing_prompt(&o);
        assert!(sp.contains("Build the widget"));
        assert!(sp.contains("update_goal_status"));
        assert!(sp.contains("eta_minutes"));
        assert!(sp.contains("Standards you must enforce"));
        assert!(sp.contains("ui-gauge"));
    }

    #[test]
    fn standards_text_empty_when_all_off() {
        let mut o = orch();
        o.standards.dev = false;
        o.standards.testing = false;
        o.standards.ux = false;
        assert!(standards_text(&o).is_empty());
        o.standards.custom = "no unwrap()".into();
        assert!(standards_text(&o).contains("no unwrap()"));
    }

    #[test]
    fn record_failure_backs_off_after_three() {
        let mut o = orch();
        let now = "2026-08-29T10:00:00Z";
        record_failure(&mut o, now, "boom");
        record_failure(&mut o, now, "boom");
        assert!(o.enabled);
        record_failure(&mut o, now, "boom");
        assert!(!o.enabled);
        assert_eq!(o.error.as_deref(), Some("boom"));
        assert!(o.log.iter().any(|e| e.kind == "backoff"));
    }

    #[test]
    fn eta_history_caps() {
        let mut o = orch();
        for i in 0..(ETA_HISTORY_CAP + 5) {
            push_eta_history(&mut o, "2026-08-29T10:00:00Z", i as u64);
        }
        assert_eq!(o.eta_history.len(), ETA_HISTORY_CAP);
    }
}
