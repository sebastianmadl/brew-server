//! Fleet Control phase 2: remote updates (`update-v1`). Protocol and behaviour:
//! `docs/Fleet-Control.md` section 8.
//!
//! An update run (`Job`) updates a list of stations either **parallel** (all
//! at once) or **rolling** (one after another: the next starts only when the
//! previous one finished, so the network is never fully off the air). The run
//! lives on the server: it goes on with the browser closed and survives a
//! server restart (it is saved with the Fleet Manager's file).
//!
//! Each station updates from its own OTA source; the server only sends
//! `StartUpdate`. A station counts as updated when it comes back with other
//! software after the restart. Limits: 45 min from the start to the restart
//! (build limit), 5 min from the restart to the station's return. A station
//! that fails is marked red (alarm) and skipped, the run goes on. A station
//! that is offline when its turn comes moves to the end of the queue once;
//! offline again, it is skipped. A call on the station delays the start until
//! the call ends, at most the chosen interrupt time (or at once with "update
//! during call").
//!
//! This module is pure state: `fleet.rs` feeds it the station events and
//! sends the `StartUpdate` commands it asks for (`run`).

use serde::{Deserialize, Serialize};

/// From the start until the station reports the restart.
pub const BUILD_LIMIT_MS: u64 = 45 * 60 * 1000;
/// From the restart until the station is back with the new software.
pub const RETURN_LIMIT_MS: u64 = 5 * 60 * 1000;
/// How often a start is tried again while a call is in progress.
pub const CALL_RETRY_MS: u64 = 10 * 1000;
/// Longest interrupt time that can be chosen.
pub const MAX_INTERRUPT_MIN: u32 = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// All stations at once.
    Parallel,
    /// One after another.
    Rolling,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Parallel => "parallel",
            Mode::Rolling => "rolling",
        }
    }
}

/// Options chosen in the update dialog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Options {
    pub mode: Mode,
    /// Start even while a call is in progress.
    pub during_call: bool,
    /// Without `during_call`: wait at most this long for a call to end, then
    /// start anyway (minutes).
    pub interrupt_min: u32,
}

/// Where one station of a run is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ItemState {
    /// Not started yet.
    Pending,
    /// `StartUpdate` sent, answer outstanding.
    Starting { at_ms: u64 },
    /// The station had a call; the start is tried again every 10 s.
    WaitingCall { since_ms: u64, last_try_ms: u64 },
    /// The OTA runs on the station.
    Running { started_ms: u64, stage: String, percent: u8, message: String },
    /// Built and installed, the station restarts.
    Restarting { since_ms: u64 },
    /// Back with other software.
    Done { version: String },
    /// Alarm: failed, the run went on without it.
    Failed { reason: String },
    /// Not updated (up to date, offline, cancelled).
    Skipped { reason: String },
}

impl ItemState {
    /// Taking part in the run right now (blocks the next start in rolling mode).
    fn active(&self) -> bool {
        matches!(self, Self::Starting { .. } | Self::WaitingCall { .. } | Self::Running { .. } | Self::Restarting { .. })
    }
    fn open(&self) -> bool {
        matches!(self, Self::Pending) || self.active()
    }
}

/// One station of a run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    /// Control login.
    pub id: String,
    pub label: String,
    /// Software when the run started.
    pub from_version: String,
    pub state: ItemState,
    /// Already moved to the end of the queue once (it was offline).
    #[serde(default)]
    pub requeued: bool,
}

/// An update run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: u64,
    pub started_ms: u64,
    /// Dashboard user who started it.
    pub user: Option<String>,
    pub options: Options,
    /// In queue order.
    pub items: Vec<Item>,
    /// "Cancel all": no further station starts.
    #[serde(default)]
    pub cancelled: bool,
    #[serde(default)]
    pub finished_ms: Option<u64>,
}

/// Something for the change log.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub station: String,
    pub label: String,
    /// `update`, `update_done`, `update_alarm`, `update_skip`, `update_end`.
    pub kind: &'static str,
    pub text: String,
    /// Dashboard user (`None`: the server / the station).
    pub user: Option<String>,
}

/// A `StartUpdate` the caller must send.
#[derive(Debug, Clone, PartialEq)]
pub struct Start {
    pub id: String,
    pub during_call: bool,
}

/// How a `StartUpdate` went.
#[derive(Debug, Clone, PartialEq)]
pub enum StartOutcome {
    /// `ok` (or `busy`: an update was already running there, it is followed).
    Started,
    /// `call_active`.
    CallActive,
    /// The station is not connected, or did not answer.
    Unreachable,
    /// Any other refusal (`code`, `message`).
    Refused(String, String),
}

fn ev(item: &Item, kind: &'static str, text: String) -> Event {
    Event { station: item.id.clone(), label: item.label.clone(), kind, text, user: None }
}

impl Job {
    /// A run over `stations` (control login, label, current software, whether its
    /// last check found a newer version), in this order. Stations without a newer
    /// version are skipped at once.
    pub fn new(id: u64, now_ms: u64, user: Option<String>, options: Options, stations: Vec<(String, String, String, bool)>) -> (Self, Vec<Event>) {
        let mut events = Vec::new();
        let items: Vec<Item> = stations
            .into_iter()
            .map(|(id, label, from_version, newer)| {
                let state = if newer { ItemState::Pending } else { ItemState::Skipped { reason: "up to date".into() } };
                Item { id, label, from_version, state, requeued: false }
            })
            .collect();
        for it in &items {
            if let ItemState::Skipped { reason } = &it.state {
                events.push(ev(it, "update_skip", format!("Basestation {}: update skipped ({reason})", it.label)));
            }
        }
        let job = Job { id, started_ms: now_ms, user: user.clone(), options, items, cancelled: false, finished_ms: None };
        let n = job.items.iter().filter(|i| i.state == ItemState::Pending).count();
        events.insert(
            0,
            Event {
                station: String::new(),
                label: String::new(),
                kind: "update",
                text: format!("Update run started ({}, {} station{})", job.mode_text(), n, if n == 1 { "" } else { "s" }),
                user,
            },
        );
        (job, events)
    }

    fn mode_text(&self) -> String {
        let call = if self.options.during_call {
            "update during call".to_string()
        } else {
            format!("calls interrupted after {} min", self.options.interrupt_min)
        };
        format!("{}, {call}", self.options.mode.as_str())
    }

    pub fn is_finished(&self) -> bool {
        self.finished_ms.is_some()
    }

    fn item_mut(&mut self, id: &str) -> Option<&mut Item> {
        self.items.iter_mut().find(|i| i.id == id)
    }

    /// After a server restart: a start whose answer never came is tried again.
    pub fn after_restart(&mut self) {
        for it in &mut self.items {
            if matches!(it.state, ItemState::Starting { .. }) {
                it.state = ItemState::Pending;
            }
        }
    }

    /// Advance the run: limits, call retries, next starts. `online`: is the
    /// station's control channel up.
    pub fn tick(&mut self, now_ms: u64, online: &dyn Fn(&str) -> bool) -> (Vec<Start>, Vec<Event>) {
        let mut starts = Vec::new();
        let mut events = Vec::new();
        if self.is_finished() {
            return (starts, events);
        }
        let interrupt_ms = self.options.interrupt_min as u64 * 60_000;
        let during_call = self.options.during_call;
        for it in &mut self.items {
            match it.state.clone() {
                ItemState::Running { started_ms, .. } if now_ms.saturating_sub(started_ms) > BUILD_LIMIT_MS => {
                    it.state = ItemState::Failed { reason: "no restart within 45 min (build limit)".into() };
                    events.push(ev(it, "update_alarm", format!("ALARM Basestation {}: update did not finish within 45 min, skipped", it.label)));
                }
                ItemState::Restarting { since_ms } if now_ms.saturating_sub(since_ms) > RETURN_LIMIT_MS => {
                    it.state = ItemState::Failed { reason: "not back 5 min after the restart".into() };
                    events.push(ev(it, "update_alarm", format!("ALARM Basestation {}: not back 5 min after the restart, skipped", it.label)));
                }
                ItemState::WaitingCall { since_ms, last_try_ms } if !self.cancelled && now_ms.saturating_sub(last_try_ms) >= CALL_RETRY_MS => {
                    let force = during_call || now_ms.saturating_sub(since_ms) >= interrupt_ms;
                    if force && !during_call {
                        events.push(ev(it, "update", format!("Basestation {}: call still in progress after {} min, updating anyway", it.label, self.options.interrupt_min)));
                    }
                    // Keep the call's start time: `start_result` reads it back.
                    it.state = ItemState::WaitingCall { since_ms, last_try_ms: now_ms };
                    starts.push(Start { id: it.id.clone(), during_call: force });
                }
                _ => {}
            }
        }
        if !self.cancelled {
            loop {
                let active = self.items.iter().filter(|i| i.state.active()).count();
                if self.options.mode == Mode::Rolling && active > 0 {
                    break;
                }
                let fresh_pending = self.items.iter().any(|i| i.state == ItemState::Pending && !i.requeued);
                let Some(idx) = self.items.iter().position(|i| i.state == ItemState::Pending && (!i.requeued || (active == 0 && !fresh_pending)))
                else {
                    break;
                };
                if !online(&self.items[idx].id) {
                    self.offline(idx, &mut events);
                    continue;
                }
                let it = &mut self.items[idx];
                it.state = ItemState::Starting { at_ms: now_ms };
                starts.push(Start { id: it.id.clone(), during_call });
                if self.options.mode == Mode::Rolling {
                    break;
                }
            }
        }
        self.check_finished(now_ms, &mut events);
        (starts, events)
    }

    /// The station at `idx` is offline when its turn comes: to the end of the
    /// queue once, then skipped.
    fn offline(&mut self, idx: usize, events: &mut Vec<Event>) {
        let mut it = self.items.remove(idx);
        if it.requeued {
            it.state = ItemState::Skipped { reason: "offline".into() };
            events.push(ev(&it, "update_skip", format!("Basestation {}: still offline, update skipped", it.label)));
        } else {
            it.requeued = true;
            it.state = ItemState::Pending;
            events.push(ev(&it, "update", format!("Basestation {}: offline, moved to the end of the queue", it.label)));
        }
        self.items.push(it);
    }

    fn check_finished(&mut self, now_ms: u64, events: &mut Vec<Event>) {
        if self.is_finished() || self.items.iter().any(|i| i.state.open()) {
            return;
        }
        self.finished_ms = Some(now_ms);
        let count = |f: &dyn Fn(&ItemState) -> bool| self.items.iter().filter(|i| f(&i.state)).count();
        let done = count(&|s| matches!(s, ItemState::Done { .. }));
        let failed = count(&|s| matches!(s, ItemState::Failed { .. }));
        let skipped = count(&|s| matches!(s, ItemState::Skipped { .. }));
        events.push(Event {
            station: String::new(),
            label: String::new(),
            kind: "update_end",
            text: format!("Update run {}: {done} updated, {failed} failed, {skipped} skipped", if self.cancelled { "cancelled" } else { "finished" }),
            user: None,
        });
    }

    /// The answer to a `StartUpdate`.
    pub fn start_result(&mut self, id: &str, outcome: StartOutcome, now_ms: u64) -> Vec<Event> {
        let mut events = Vec::new();
        let Some(idx) = self.items.iter().position(|i| i.id == id) else { return events };
        let it = &mut self.items[idx];
        if !matches!(it.state, ItemState::Starting { .. } | ItemState::WaitingCall { .. }) {
            return events;
        }
        let call_since = match it.state {
            ItemState::WaitingCall { since_ms, .. } => Some(since_ms),
            _ => None,
        };
        match outcome {
            StartOutcome::Started => {
                it.state = ItemState::Running { started_ms: now_ms, stage: "download".into(), percent: 0, message: String::new() };
                events.push(ev(it, "update", format!("Basestation {}: update started", it.label)));
            }
            StartOutcome::CallActive => {
                if call_since.is_none() {
                    events.push(ev(it, "update", format!("Basestation {}: call in progress, waiting for it to end", it.label)));
                }
                it.state = ItemState::WaitingCall { since_ms: call_since.unwrap_or(now_ms), last_try_ms: now_ms };
            }
            StartOutcome::Unreachable => self.offline(idx, &mut events),
            StartOutcome::Refused(code, message) => {
                it.state = ItemState::Failed { reason: format!("{code}: {message}") };
                events.push(ev(it, "update_alarm", format!("ALARM Basestation {}: update refused ({message}), skipped", it.label)));
            }
        }
        self.check_finished(now_ms, &mut events);
        events
    }

    /// An `UpdateProgress` from the station.
    pub fn progress(&mut self, id: &str, stage: &str, percent: u8, message: &str, now_ms: u64) -> Vec<Event> {
        let mut events = Vec::new();
        let Some(it) = self.item_mut(id) else { return events };
        match &it.state {
            ItemState::Starting { .. } | ItemState::WaitingCall { .. } | ItemState::Running { .. } => {}
            _ => return events,
        }
        let started_ms = match &it.state {
            ItemState::Running { started_ms, .. } => *started_ms,
            _ => now_ms,
        };
        match stage {
            "restart" => {
                it.state = ItemState::Restarting { since_ms: now_ms };
                events.push(ev(it, "update", format!("Basestation {}: built and installed, restarting", it.label)));
            }
            "up_to_date" => {
                it.state = ItemState::Skipped { reason: "already up to date".into() };
                events.push(ev(it, "update_skip", format!("Basestation {}: already up to date, skipped", it.label)));
            }
            "failed" => {
                let reason = if message.is_empty() { "the update failed".to_string() } else { message.to_string() };
                events.push(ev(it, "update_alarm", format!("ALARM Basestation {}: update failed ({reason}), skipped", it.label)));
                it.state = ItemState::Failed { reason };
            }
            _ => it.state = ItemState::Running { started_ms, stage: stage.to_string(), percent, message: message.to_string() },
        }
        self.check_finished(now_ms, &mut events);
        events
    }

    /// A hello from the station: back with other software means updated.
    pub fn hello(&mut self, id: &str, version: &str, now_ms: u64) -> Vec<Event> {
        let mut events = Vec::new();
        let Some(it) = self.item_mut(id) else { return events };
        if !matches!(it.state, ItemState::Running { .. } | ItemState::Restarting { .. }) {
            return events;
        }
        if version != it.from_version {
            events.push(ev(it, "update_done", format!("Basestation {}: update finished, software {} → {version}", it.label, it.from_version)));
            it.state = ItemState::Done { version: version.to_string() };
        } else if matches!(it.state, ItemState::Restarting { .. }) {
            it.state = ItemState::Failed { reason: "came back with the old software".into() };
            events.push(ev(it, "update_alarm", format!("ALARM Basestation {}: came back with the old software {version}, skipped", it.label)));
        }
        // Same software while building: a reconnect of the control channel, nothing changed.
        self.check_finished(now_ms, &mut events);
        events
    }

    /// "Cancel all": no further station starts; running ones finish.
    pub fn cancel_all(&mut self, user: Option<&str>, now_ms: u64) -> Vec<Event> {
        let mut events = Vec::new();
        if self.is_finished() {
            return events;
        }
        self.cancelled = true;
        events.push(Event { station: String::new(), label: String::new(), kind: "update", text: "Update run cancelled: no further stations start".into(), user: user.map(str::to_owned) });
        for it in &mut self.items {
            if matches!(it.state, ItemState::Pending | ItemState::WaitingCall { .. }) {
                it.state = ItemState::Skipped { reason: "cancelled".into() };
            }
        }
        self.check_finished(now_ms, &mut events);
        events
    }

    /// Take one station out of the run (only before it started).
    pub fn cancel_one(&mut self, id: &str, user: Option<&str>, now_ms: u64) -> Option<Vec<Event>> {
        let it = self.item_mut(id)?;
        if !matches!(it.state, ItemState::Pending | ItemState::WaitingCall { .. }) {
            return None;
        }
        it.state = ItemState::Skipped { reason: "cancelled".into() };
        let mut e = ev(it, "update_skip", format!("Basestation {}: update cancelled", it.label));
        e.user = user.map(str::to_owned);
        let mut events = vec![e];
        self.check_finished(now_ms, &mut events);
        Some(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: u64 = 60_000;

    fn opts(mode: Mode) -> Options {
        Options { mode, during_call: false, interrupt_min: 10 }
    }
    fn stations(n: usize) -> Vec<(String, String, String, bool)> {
        (1..=n).map(|i| (format!("bs{i}"), format!("BS {i}"), "v1".to_string(), true)).collect()
    }
    fn all_online(_: &str) -> bool {
        true
    }
    fn ids(s: &[Start]) -> Vec<&str> {
        s.iter().map(|x| x.id.as_str()).collect()
    }
    /// Takes station `id` through a good update: started, restart, back with v2.
    fn update_ok(job: &mut Job, id: &str, t: u64) {
        job.start_result(id, StartOutcome::Started, t);
        job.progress(id, "build", 50, "Compiling", t + 1);
        job.progress(id, "restart", 100, "", t + 2);
        job.hello(id, "v2", t + 3);
    }

    #[test]
    fn rolling_updates_one_after_another_across_ten_stations() {
        let (mut job, ev) = Job::new(1, 0, Some("oe4sxm".into()), opts(Mode::Rolling), stations(10));
        assert_eq!(ev[0].text, "Update run started (rolling, calls interrupted after 10 min, 10 stations)");
        let mut t = 0;
        for i in 1..=10 {
            let (s, _) = job.tick(t, &all_online);
            assert_eq!(ids(&s), vec![format!("bs{i}").as_str()], "only the next station starts");
            let (again, _) = job.tick(t + 1, &all_online);
            assert!(again.is_empty(), "nothing else starts while bs{i} runs");
            update_ok(&mut job, &format!("bs{i}"), t + 2);
            t += 20 * MIN;
        }
        assert!(job.is_finished());
        assert!(job.items.iter().all(|i| i.state == ItemState::Done { version: "v2".into() }));
    }

    #[test]
    fn parallel_starts_everything_at_once() {
        let (mut job, _) = Job::new(1, 0, None, opts(Mode::Parallel), stations(10));
        let (s, _) = job.tick(0, &all_online);
        assert_eq!(s.len(), 10);
        for i in 1..=10 {
            update_ok(&mut job, &format!("bs{i}"), 1);
        }
        assert!(job.is_finished());
    }

    #[test]
    fn up_to_date_stations_are_skipped() {
        let mut st = stations(3);
        st[1].3 = false;
        let (mut job, ev) = Job::new(1, 0, None, opts(Mode::Parallel), st);
        assert!(ev.iter().any(|e| e.kind == "update_skip" && e.station == "bs2"));
        let (s, _) = job.tick(0, &all_online);
        assert_eq!(ids(&s), vec!["bs1", "bs3"]);
        // A station that turns out to be up to date when it runs is skipped too.
        job.start_result("bs1", StartOutcome::Started, 1);
        job.progress("bs1", "up_to_date", 100, "", 2);
        assert!(matches!(job.items[0].state, ItemState::Skipped { .. }));
    }

    #[test]
    fn failure_is_an_alarm_and_the_run_goes_on() {
        let (mut job, _) = Job::new(1, 0, None, opts(Mode::Rolling), stations(3));
        job.tick(0, &all_online);
        job.start_result("bs1", StartOutcome::Started, 1);
        let ev = job.progress("bs1", "failed", 0, "ERROR: cargo build failed", 2);
        assert_eq!(ev[0].kind, "update_alarm");
        let (s, _) = job.tick(3, &all_online);
        assert_eq!(ids(&s), vec!["bs2"]);
    }

    #[test]
    fn not_back_five_minutes_after_the_restart() {
        let (mut job, _) = Job::new(1, 0, None, opts(Mode::Rolling), stations(2));
        job.tick(0, &all_online);
        job.start_result("bs1", StartOutcome::Started, 0);
        job.progress("bs1", "restart", 100, "", 10 * MIN);
        let (s, ev) = job.tick(10 * MIN + 4 * MIN, &all_online);
        assert!(s.is_empty() && ev.is_empty(), "still within 5 min");
        let (s, ev) = job.tick(10 * MIN + 5 * MIN + 1, &all_online);
        assert_eq!(ev[0].kind, "update_alarm");
        assert!(ev[0].text.contains("not back 5 min"));
        assert_eq!(ids(&s), vec!["bs2"], "skipped, the next one starts");
        // Coming back later with the same software no longer changes anything.
        assert!(job.hello("bs1", "v1", 20 * MIN).is_empty());
    }

    #[test]
    fn build_limit_45_minutes() {
        let (mut job, _) = Job::new(1, 0, None, opts(Mode::Rolling), stations(2));
        job.tick(0, &all_online);
        job.start_result("bs1", StartOutcome::Started, 0);
        job.progress("bs1", "build", 40, "", 30 * MIN);
        let (_, ev) = job.tick(45 * MIN, &all_online);
        assert!(ev.is_empty());
        let (s, ev) = job.tick(45 * MIN + 1, &all_online);
        assert_eq!(ev[0].kind, "update_alarm");
        assert_eq!(ids(&s), vec!["bs2"]);
    }

    #[test]
    fn back_with_the_old_software_is_an_alarm() {
        let (mut job, _) = Job::new(1, 0, None, opts(Mode::Rolling), stations(1));
        job.tick(0, &all_online);
        job.start_result("bs1", StartOutcome::Started, 0);
        assert!(job.hello("bs1", "v1", 1).is_empty(), "a reconnect while building is not the end");
        job.progress("bs1", "restart", 100, "", 2);
        let ev = job.hello("bs1", "v1", 3);
        assert_eq!(ev[0].kind, "update_alarm");
        assert!(job.is_finished());
    }

    #[test]
    fn offline_station_goes_to_the_end_then_is_skipped() {
        let (mut job, _) = Job::new(1, 0, None, opts(Mode::Rolling), stations(3));
        let off = |id: &str| id != "bs1";
        let (s, ev) = job.tick(0, &off);
        assert_eq!(ids(&s), vec!["bs2"]);
        assert!(ev[0].text.contains("moved to the end"));
        assert_eq!(job.items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), vec!["bs2", "bs3", "bs1"]);
        update_ok(&mut job, "bs2", 1);
        let (s, _) = job.tick(10, &off);
        assert_eq!(ids(&s), vec!["bs3"]);
        update_ok(&mut job, "bs3", 11);
        let (s, ev) = job.tick(20, &off);
        assert!(s.is_empty());
        assert!(ev.iter().any(|e| e.kind == "update_skip" && e.text.contains("still offline")));
        assert!(job.is_finished());
    }

    #[test]
    fn offline_station_comes_back_in_time() {
        let (mut job, _) = Job::new(1, 0, None, opts(Mode::Parallel), stations(2));
        let (s, _) = job.tick(0, &|id: &str| id != "bs1");
        assert_eq!(ids(&s), vec!["bs2"]);
        let (s, _) = job.tick(1, &all_online);
        assert!(s.is_empty(), "a requeued station waits until the others are through");
        update_ok(&mut job, "bs2", 2);
        let (s, _) = job.tick(10, &all_online);
        assert_eq!(ids(&s), vec!["bs1"]);
    }

    #[test]
    fn unreachable_at_start_counts_as_offline() {
        let (mut job, _) = Job::new(1, 0, None, opts(Mode::Rolling), stations(2));
        job.tick(0, &all_online);
        job.start_result("bs1", StartOutcome::Unreachable, 1);
        assert!(job.items.last().unwrap().requeued);
        let (s, _) = job.tick(2, &all_online);
        assert_eq!(ids(&s), vec!["bs2"]);
    }

    #[test]
    fn call_delays_the_start_until_the_interrupt_time() {
        let (mut job, _) = Job::new(1, 0, None, opts(Mode::Rolling), stations(2));
        job.tick(0, &all_online);
        let ev = job.start_result("bs1", StartOutcome::CallActive, 0);
        assert!(ev[0].text.contains("waiting"));
        let (s, _) = job.tick(5_000, &all_online);
        assert!(s.is_empty(), "retry every 10 s, and bs2 waits in rolling mode");
        let (s, _) = job.tick(10_000, &all_online);
        assert_eq!(s, vec![Start { id: "bs1".into(), during_call: false }]);
        assert!(job.start_result("bs1", StartOutcome::CallActive, 10_000).is_empty(), "logged once");
        let (s, ev) = job.tick(10 * MIN, &all_online);
        assert_eq!(s, vec![Start { id: "bs1".into(), during_call: true }], "after 10 min the call is interrupted");
        assert!(ev[0].text.contains("updating anyway"));
        job.start_result("bs1", StartOutcome::Started, 10 * MIN);
        assert!(matches!(job.items[0].state, ItemState::Running { .. }));
    }

    #[test]
    fn update_during_call_starts_at_once() {
        let mut o = opts(Mode::Parallel);
        o.during_call = true;
        let (mut job, _) = Job::new(1, 0, None, o, stations(1));
        let (s, _) = job.tick(0, &all_online);
        assert_eq!(s, vec![Start { id: "bs1".into(), during_call: true }]);
    }

    #[test]
    fn cancel_all_lets_the_running_one_finish() {
        let (mut job, _) = Job::new(1, 0, None, opts(Mode::Rolling), stations(3));
        job.tick(0, &all_online);
        job.start_result("bs1", StartOutcome::Started, 0);
        let ev = job.cancel_all(Some("alice"), 1);
        assert_eq!(ev[0].user.as_deref(), Some("alice"));
        assert!(!job.is_finished(), "bs1 still runs");
        let (s, _) = job.tick(2, &all_online);
        assert!(s.is_empty());
        job.progress("bs1", "restart", 100, "", 3);
        let ev = job.hello("bs1", "v2", 4);
        assert!(job.is_finished());
        assert_eq!(ev.last().unwrap().text, "Update run cancelled: 1 updated, 0 failed, 2 skipped");
    }

    #[test]
    fn cancel_one_station_only_before_it_starts() {
        let (mut job, _) = Job::new(1, 0, None, opts(Mode::Rolling), stations(3));
        job.tick(0, &all_online);
        assert!(job.cancel_one("bs1", None, 1).is_none(), "bs1 already started");
        assert!(job.cancel_one("bs2", Some("bob"), 1).is_some());
        job.start_result("bs1", StartOutcome::Started, 2);
        update_ok(&mut job, "bs1", 3);
        let (s, _) = job.tick(10, &all_online);
        assert_eq!(ids(&s), vec!["bs3"]);
    }

    #[test]
    fn refused_start_is_an_alarm() {
        let (mut job, _) = Job::new(1, 0, None, opts(Mode::Parallel), stations(1));
        job.tick(0, &all_online);
        let ev = job.start_result("bs1", StartOutcome::Refused("fleet_control_off".into(), "Fleet Control is switched off".into()), 1);
        assert_eq!(ev[0].kind, "update_alarm");
        assert!(job.is_finished());
    }

    #[test]
    fn survives_a_restart_as_json() {
        let (mut job, _) = Job::new(7, 0, None, opts(Mode::Rolling), stations(2));
        job.tick(0, &all_online);
        let mut back: Job = serde_json::from_str(&serde_json::to_string(&job).unwrap()).unwrap();
        assert_eq!(back, job);
        back.after_restart();
        assert_eq!(back.items[0].state, ItemState::Pending, "an unanswered start is tried again");
        let (s, _) = back.tick(1, &all_online);
        assert_eq!(ids(&s), vec!["bs1"]);
    }
}
