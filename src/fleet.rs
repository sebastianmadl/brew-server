//! Fleet Control: stations that let this server (the SwMI) manage them, over
//! the Basestation control channel. Protocol and behaviour: `docs/Fleet-Control.md`.
//!
//! The server offers `fleet-control` in `X-Brew-Extensions` (Brew and control
//! listener) whenever `[control]` is enabled. A FlowStation that sees it sends
//! `StationHello` after connecting and whenever its local switch changes; the
//! general answer to Fleet Control commands is `CommandResult`.
//!
//! The Fleet Manager lists every station whose last hello had the switch on,
//! keyed by its control login. A station stays listed while it is offline;
//! it leaves the list when it says the switch is off (at once while online,
//! else at its next login), with the INFO "Basestation XYZ switched off Fleet
//! Control", or when a user with Fleet Control management removes it. The
//! change log keeps 90 days. Both survive restarts in `<storage path>.fleet`.

use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
};

/// Name in `X-Brew-Extensions`.
pub const CAPABILITY: &str = "fleet-control";

/// Fleet Control protocol version this server speaks (`StationHello.protocol`).
pub const PROTOCOL: u32 = 1;

/// How long change log entries are kept.
pub const LOG_KEEP_MS: u64 = 90 * 24 * 3600 * 1000;

/// What a station reports in its hello (see `control::ControlResponse::StationHello`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u32,
    pub version: String,
    pub brew_login: Option<String>,
    pub mcc: u16,
    pub mnc: u16,
    pub fleet_control: bool,
    pub capabilities: Vec<String>,
}

/// One station in the Fleet Manager.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Station {
    /// Control login (the key).
    pub id: String,
    pub brew_login: Option<String>,
    pub version: String,
    pub mcc: u16,
    pub mnc: u16,
    pub protocol: u32,
    pub capabilities: Vec<String>,
    /// Control channel connected now (not persisted: false after a restart).
    #[serde(skip)]
    pub online: bool,
    pub first_seen_ms: u64,
    pub last_seen_ms: u64,
    /// Last update check (`CheckUpdate`), phase 2.
    #[serde(default)]
    pub check: Option<CheckInfo>,
    /// Last `UpdateProgress` of an OTA run (whoever started it), phase 2.
    #[serde(default)]
    pub progress: Option<ProgressInfo>,
}

/// Result of the last update check of a station.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckInfo {
    pub at_ms: u64,
    /// The check reached the repository.
    pub ok: bool,
    pub current: String,
    pub latest: Option<String>,
    pub update_available: bool,
    /// OTA source of the station.
    pub channel: String,
    pub repo: String,
    pub branch: String,
    pub message: String,
}

/// Last reported OTA progress of a station.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgressInfo {
    pub at_ms: u64,
    pub stage: String,
    pub percent: u8,
    pub message: String,
}

/// Server setting "Updates repo": the reference source the Fleet Manager
/// compares the stations' OTA sources with (empty: the product repository,
/// branch `main`). Phase 2 only shows a hint; stations keep their own source.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UpdateSettings {
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub branch: String,
}

/// Product repository and branch used when "Updates repo" is empty.
pub const DEFAULT_UPDATE_REPO: &str = "https://github.com/Aitorrio/bost-flowstation";
pub const DEFAULT_UPDATE_BRANCH: &str = "main";

impl UpdateSettings {
    /// Repository and branch in effect.
    pub fn effective(&self) -> (String, String) {
        let repo = if self.repo.trim().is_empty() { DEFAULT_UPDATE_REPO } else { self.repo.trim() };
        let branch = if self.branch.trim().is_empty() { DEFAULT_UPDATE_BRANCH } else { self.branch.trim() };
        (repo.to_owned(), branch.to_owned())
    }
}

/// Repository URLs compared without case, `.git`, trailing slash or credentials.
pub fn same_repo(a: &str, b: &str) -> bool {
    fn norm(u: &str) -> String {
        let u = u.trim().to_ascii_lowercase();
        let u = match u.split_once("://") {
            Some((scheme, rest)) => format!("{scheme}://{}", rest.rsplit_once('@').map(|(_, h)| h).unwrap_or(rest)),
            None => u,
        };
        let u = u.trim_end_matches('/');
        u.strip_suffix(".git").unwrap_or(u).to_owned()
    }
    norm(a) == norm(b)
}

/// One change log line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogEntry {
    /// Running number (the dashboard dismisses INFO popups by it).
    pub id: u64,
    pub at_ms: u64,
    /// Control login of the station.
    pub station: String,
    /// Name or number of the station at the time.
    pub label: String,
    /// Dashboard user who did it (`None`: the station itself).
    pub user: Option<String>,
    /// `fleet_on`, `fleet_off`, `version`, `removed`; phase 2: `update`,
    /// `update_done`, `update_alarm` (shown red), `update_skip`, `update_end`,
    /// `settings`.
    pub kind: String,
    pub text: String,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    stations: BTreeMap<String, Station>,
    log: VecDeque<LogEntry>,
    next_id: u64,
    /// The current or last update run (phase 2).
    #[serde(default)]
    job: Option<crate::fleet_update::Job>,
    #[serde(default)]
    next_job: u64,
    #[serde(default)]
    update_settings: UpdateSettings,
}

/// The Fleet Manager's data.
#[derive(Default)]
pub struct Fleet {
    path: Option<PathBuf>,
    data: File,
}

/// Name for log lines: the name given to the station on this server, else its
/// Brew number, else its control login.
pub fn label(names: &dyn Fn(&str) -> Option<String>, id: &str, brew_login: Option<&str>) -> String {
    brew_login.and_then(|l| names(l)).or_else(|| names(id)).unwrap_or_else(|| brew_login.unwrap_or(id).to_owned())
}

impl Fleet {
    /// Loads `path` (missing or broken: empty). `None`: memory only.
    pub fn load(path: Option<PathBuf>, now_ms: u64) -> Self {
        let data = path
            .as_deref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice::<File>(&b).ok())
            .unwrap_or_default();
        let mut fleet = Self { path, data };
        fleet.prune(now_ms);
        if let Some(job) = fleet.data.job.as_mut() {
            job.after_restart();
        }
        fleet
    }

    /// The file next to the history log.
    pub fn path_for(storage: &Path) -> PathBuf {
        let mut p = storage.as_os_str().to_owned();
        p.push(".fleet");
        p.into()
    }

    pub fn stations(&self) -> Vec<Station> {
        self.data.stations.values().cloned().collect()
    }

    pub fn log(&self) -> Vec<LogEntry> {
        self.data.log.iter().rev().cloned().collect()
    }

    #[cfg(test)]
    pub fn get(&self, id: &str) -> Option<&Station> {
        self.data.stations.get(id)
    }

    /// Drops log entries older than 90 days.
    pub fn prune(&mut self, now_ms: u64) {
        while self.data.log.front().is_some_and(|e| e.at_ms + LOG_KEEP_MS < now_ms) {
            self.data.log.pop_front();
        }
    }

    fn push(&mut self, now_ms: u64, station: &str, label: String, user: Option<&str>, kind: &str, text: String) {
        self.data.next_id += 1;
        tracing::info!(station, kind, "Fleet Control: {text}");
        self.data.log.push_back(LogEntry {
            id: self.data.next_id,
            at_ms: now_ms,
            station: station.to_owned(),
            label,
            user: user.map(str::to_owned),
            kind: kind.to_owned(),
            text,
        });
        self.prune(now_ms);
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        let Ok(bytes) = serde_json::to_vec_pretty(&self.data) else { return };
        let tmp = path.with_extension("fleet.tmp");
        if let Err(e) = std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, path)) {
            tracing::warn!(error = %e, path = %path.display(), "cannot write the Fleet Control file");
        }
    }

    /// A hello from the station logged in as `id`. Returns true when the list changed.
    pub fn hello(&mut self, id: &str, h: &Hello, now_ms: u64, names: &dyn Fn(&str) -> Option<String>) -> bool {
        let label = label(names, id, h.brew_login.as_deref());
        if !h.fleet_control {
            // Off: the station leaves the list (it was listed) with the INFO.
            if self.data.stations.remove(id).is_none() {
                return false;
            }
            self.push(now_ms, id, label.clone(), None, "fleet_off", format!("Basestation {label} switched off Fleet Control"));
            self.save();
            return true;
        }
        let entry = self.data.stations.get(id).cloned();
        let station = Station {
            id: id.to_owned(),
            brew_login: h.brew_login.clone(),
            version: h.version.clone(),
            mcc: h.mcc,
            mnc: h.mnc,
            protocol: h.protocol,
            capabilities: h.capabilities.clone(),
            online: true,
            first_seen_ms: entry.as_ref().map(|s| s.first_seen_ms).unwrap_or(now_ms),
            last_seen_ms: now_ms,
            // A check is only valid for the software it was made with.
            check: entry.as_ref().and_then(|s| s.check.clone()).filter(|c| c.current == h.version),
            progress: entry.as_ref().and_then(|s| s.progress.clone()),
        };
        // An update run that ends with this hello logs the software change itself.
        let events = self.data.job.as_mut().map(|j| j.hello(id, &h.version, now_ms)).unwrap_or_default();
        let run_logs_it = events.iter().any(|e| e.kind == "update_done");
        match &entry {
            None => self.push(now_ms, id, label.clone(), None, "fleet_on", format!("Basestation {label} switched on Fleet Control")),
            Some(_) if run_logs_it => {}
            Some(old) if old.version != h.version => {
                self.push(now_ms, id, label.clone(), None, "version", format!("Basestation {label}: software {} → {}", old.version, h.version))
            }
            Some(_) => {}
        }
        self.data.stations.insert(id.to_owned(), station);
        self.apply(now_ms, events);
        self.save();
        true
    }

    /// The control channel of `id` went up or down (no log line: not a change).
    pub fn set_online(&mut self, id: &str, online: bool, now_ms: u64) {
        if let Some(s) = self.data.stations.get_mut(id) {
            s.online = online;
            s.last_seen_ms = now_ms;
            if !online {
                self.save();
            }
        }
    }

    /// A user removes a station from the list (it comes back with its next
    /// hello if its switch is still on).
    pub fn remove(&mut self, id: &str, user: Option<&str>, now_ms: u64, names: &dyn Fn(&str) -> Option<String>) -> bool {
        let Some(s) = self.data.stations.remove(id) else { return false };
        let label = label(names, id, s.brew_login.as_deref());
        self.push(now_ms, id, label.clone(), user, "removed", format!("Basestation {label} removed from the Fleet Manager"));
        self.save();
        true
    }
}

/// Phase 2: update checks and runs.
impl Fleet {
    /// Write run events to the change log.
    fn apply(&mut self, now_ms: u64, events: Vec<crate::fleet_update::Event>) {
        for e in events {
            self.push(now_ms, &e.station, e.label, e.user.as_deref(), e.kind, e.text);
        }
    }

    pub fn job(&self) -> Option<&crate::fleet_update::Job> {
        self.data.job.as_ref()
    }

    pub fn update_settings(&self) -> &UpdateSettings {
        &self.data.update_settings
    }

    /// Admin changes "Updates repo".
    pub fn set_update_settings(&mut self, s: UpdateSettings, user: Option<&str>, now_ms: u64) {
        let (repo, branch) = s.effective();
        self.data.update_settings = s;
        self.push(now_ms, "", String::new(), user, "settings", format!("Updates repo set to {repo} ({branch})"));
        self.save();
    }

    /// Store the result of an update check.
    pub fn set_check(&mut self, id: &str, check: CheckInfo) {
        if let Some(s) = self.data.stations.get_mut(id) {
            s.check = Some(check);
            self.save();
        }
    }

    /// An `UpdateProgress` from station `id` (also for runs started on its own dashboard).
    pub fn progress(&mut self, id: &str, stage: &str, percent: u8, message: &str, _version: &str, now_ms: u64, names: &dyn Fn(&str) -> Option<String>) {
        let Some(s) = self.data.stations.get_mut(id) else { return };
        s.progress = Some(ProgressInfo { at_ms: now_ms, stage: stage.to_owned(), percent, message: message.to_owned() });
        let in_job = self.data.job.as_ref().is_some_and(|j| !j.is_finished() && j.items.iter().any(|i| i.id == id));
        let events = match self.data.job.as_mut() {
            Some(j) if in_job => j.progress(id, stage, percent, message, now_ms),
            _ => {
                // An update the server did not start (local dashboard): log its end only.
                let label = label(names, id, self.data.stations.get(id).and_then(|s| s.brew_login.as_deref()));
                match stage {
                    "restart" => vec![crate::fleet_update::Event { station: id.to_owned(), label: label.clone(), kind: "update", text: format!("Basestation {label}: local update built, restarting"), user: None }],
                    "failed" => vec![crate::fleet_update::Event { station: id.to_owned(), label: label.clone(), kind: "update_alarm", text: format!("ALARM Basestation {label}: local update failed ({message})"), user: None }],
                    _ => vec![],
                }
            }
        };
        let changed = !events.is_empty();
        self.apply(now_ms, events);
        if changed || matches!(stage, "restart" | "failed" | "up_to_date") {
            self.save();
        }
    }

    /// Stations that can be updated now: listed, online, `update-v1`, and their
    /// last check (for the software they run) found a newer version.
    pub fn updatable(&self, id: &str) -> bool {
        self.data.stations.get(id).is_some_and(|s| {
            s.online && s.capabilities.iter().any(|c| c == UPDATE_CAPABILITY) && s.check.as_ref().is_some_and(|c| c.ok && c.update_available && c.current == s.version)
        })
    }

    /// Start a run over `ids` (in this order). Err: a run is still going, or no station.
    pub fn start_job(&mut self, ids: &[String], options: crate::fleet_update::Options, user: Option<&str>, now_ms: u64, names: &dyn Fn(&str) -> Option<String>) -> Result<u64, &'static str> {
        if self.data.job.as_ref().is_some_and(|j| !j.is_finished()) {
            return Err("An update run is in progress: wait for it or cancel it");
        }
        let stations: Vec<(String, String, String, bool)> = ids
            .iter()
            .filter_map(|id| self.data.stations.get(id))
            .filter(|s| s.capabilities.iter().any(|c| c == UPDATE_CAPABILITY))
            .map(|s| (s.id.clone(), label(names, &s.id, s.brew_login.as_deref()), s.version.clone(), self.updatable(&s.id)))
            .collect();
        if stations.is_empty() {
            return Err("No station to update");
        }
        self.data.next_job += 1;
        let id = self.data.next_job;
        let (job, events) = crate::fleet_update::Job::new(id, now_ms, user.map(str::to_owned), options, stations);
        self.data.job = Some(job);
        self.apply(now_ms, events);
        self.save();
        Ok(id)
    }

    /// Advance the run; returns the `StartUpdate` commands to send.
    pub fn tick_job(&mut self, now_ms: u64) -> Vec<crate::fleet_update::Start> {
        let online: std::collections::HashSet<String> = self.data.stations.values().filter(|s| s.online).map(|s| s.id.clone()).collect();
        let Some(job) = self.data.job.as_mut() else { return Vec::new() };
        if job.is_finished() {
            return Vec::new();
        }
        let before = job.clone();
        let (starts, events) = job.tick(now_ms, &|id| online.contains(id));
        let changed = *job != before;
        self.apply(now_ms, events);
        if changed {
            self.save();
        }
        starts
    }

    pub fn start_result(&mut self, id: &str, outcome: crate::fleet_update::StartOutcome, now_ms: u64) {
        let Some(job) = self.data.job.as_mut() else { return };
        let events = job.start_result(id, outcome, now_ms);
        self.apply(now_ms, events);
        self.save();
    }

    /// Cancel the whole run (`station` None) or one station of it. False: nothing to cancel.
    pub fn cancel(&mut self, station: Option<&str>, user: Option<&str>, now_ms: u64) -> bool {
        let Some(job) = self.data.job.as_mut().filter(|j| !j.is_finished()) else { return false };
        let events = match station {
            None => job.cancel_all(user, now_ms),
            Some(id) => match job.cancel_one(id, user, now_ms) {
                Some(e) => e,
                None => return false,
            },
        };
        self.apply(now_ms, events);
        self.save();
        true
    }
}

/// Capability of phase 2 (remote update).
pub const UPDATE_CAPABILITY: &str = "update-v1";

/// Station as the Fleet Manager shows it.
#[derive(Serialize)]
struct StationView {
    #[serde(flatten)]
    station: Station,
    online: bool,
    label: String,
    /// Its last check found a newer version (the Update button is offered).
    updatable: bool,
    /// Its OTA source differs from the server's "Updates repo" (hint only).
    other_source: bool,
}

/// `GET /api/fleet` (Fleet Control visibility): the listed stations, the change
/// log (newest first, 90 days) and whether this login may manage them.
pub async fn api_list(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<crate::state::AppState>>,
    headers: axum::http::HeaderMap,
) -> axum::Json<serde_json::Value> {
    let a = crate::dashboard::access(&state, &headers);
    let names = crate::control::station_names(&state);
    let lookup = |k: &str| names.get(k).cloned();
    let mut fleet = state.fleet.lock().unwrap_or_else(|e| e.into_inner());
    fleet.prune(now_ms());
    let (ref_repo, ref_branch) = fleet.update_settings().effective();
    let stations: Vec<StationView> = fleet
        .stations()
        .into_iter()
        .map(|s| StationView {
            online: s.online,
            label: label(&lookup, &s.id, s.brew_login.as_deref()),
            updatable: fleet.updatable(&s.id),
            other_source: s.check.as_ref().is_some_and(|c| !c.repo.is_empty() && (!same_repo(&c.repo, &ref_repo) || c.branch != ref_branch)),
            station: s,
        })
        .collect();
    axum::Json(serde_json::json!({
        "offered": state.config.control.enabled,
        "protocol": PROTOCOL,
        "can_manage": a.manages_fleet(),
        "is_admin": a.admin,
        "stations": stations,
        "log": fleet.log(),
        "job": fleet.job(),
        "update_settings": fleet.update_settings(),
        "update_source": {"repo": ref_repo, "branch": ref_branch},
        "limits": {"build_min": crate::fleet_update::BUILD_LIMIT_MS / 60_000, "return_min": crate::fleet_update::RETURN_LIMIT_MS / 60_000, "max_interrupt_min": crate::fleet_update::MAX_INTERRUPT_MIN},
    }))
}

#[derive(Deserialize)]
pub struct IdRequest {
    pub id: String,
}

fn refuse() -> axum::response::Response {
    use axum::response::IntoResponse;
    (axum::http::StatusCode::FORBIDDEN, "This needs the right Fleet Control management").into_response()
}

/// `POST /api/fleet/remove` `{"id": "<control login>"}` (Fleet Control management).
pub async fn api_remove(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<crate::state::AppState>>,
    headers: axum::http::HeaderMap,
    axum::Json(req): axum::Json<IdRequest>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let a = crate::dashboard::access(&state, &headers);
    if !a.manages_fleet() {
        return refuse();
    }
    let names = crate::control::station_names(&state);
    let removed = state.fleet.lock().unwrap_or_else(|e| e.into_inner()).remove(&req.id, a.user.as_deref(), now_ms(), &|k| names.get(k).cloned());
    if removed {
        axum::Json(serde_json::json!({"ok": true})).into_response()
    } else {
        (axum::http::StatusCode::NOT_FOUND, "No such station in the Fleet Manager").into_response()
    }
}

/// `POST /api/fleet/ping` `{"id": "<control login>"}` (Fleet Control management):
/// sends `FleetPing` and returns the station's `CommandResult`.
pub async fn api_ping(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<crate::state::AppState>>,
    headers: axum::http::HeaderMap,
    axum::Json(req): axum::Json<IdRequest>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if !crate::dashboard::access(&state, &headers).manages_fleet() {
        return refuse();
    }
    let started = std::time::Instant::now();
    match crate::control::send_command(&state, &req.id, crate::control::ControlCommand::FleetPing { handle: 0 }).await {
        Ok(Some(crate::control::ControlResponse::CommandResult { ok, code, message, .. })) => {
            axum::Json(serde_json::json!({"ok": ok, "code": code, "message": message, "ms": started.elapsed().as_millis() as u64})).into_response()
        }
        Ok(_) => (axum::http::StatusCode::BAD_GATEWAY, "unexpected answer from the station").into_response(),
        Err(crate::control::SendError::NotConnected) => (axum::http::StatusCode::NOT_FOUND, "The station is not connected").into_response(),
        Err(crate::control::SendError::Timeout) => (axum::http::StatusCode::GATEWAY_TIMEOUT, "No answer from the station within 5 s").into_response(),
    }
}

#[derive(Deserialize, Default)]
pub struct IdsRequest {
    /// Control logins; empty: every station that can do it.
    #[serde(default)]
    pub ids: Vec<String>,
}

/// `POST /api/fleet/check` `{"ids": [...]}` (Fleet Control management): every
/// station (online, `update-v1`; empty list: all of them) runs its update check
/// against its own OTA source. Waits for all answers (up to 60 s).
pub async fn api_check(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<crate::state::AppState>>,
    headers: axum::http::HeaderMap,
    axum::Json(req): axum::Json<IdsRequest>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if !crate::dashboard::access(&state, &headers).manages_fleet() {
        return refuse();
    }
    let ids: Vec<String> = {
        let fleet = state.fleet.lock().unwrap_or_else(|e| e.into_inner());
        fleet
            .stations()
            .into_iter()
            .filter(|s| s.online && s.capabilities.iter().any(|c| c == UPDATE_CAPABILITY))
            .filter(|s| req.ids.is_empty() || req.ids.contains(&s.id))
            .map(|s| s.id)
            .collect()
    };
    let asks = ids.into_iter().map(|id| {
        let state = state.clone();
        async move {
            let r = crate::control::send_command(&state, &id, crate::control::ControlCommand::CheckUpdate { handle: 0 }).await;
            (id, r)
        }
    });
    let mut out = Vec::new();
    for (id, r) in futures_util::future::join_all(asks).await {
        let check = match r {
            Ok(Some(crate::control::ControlResponse::UpdateInfo { ok, current, latest, update_available, channel, repo, branch, message, .. })) => {
                CheckInfo { at_ms: now_ms(), ok, current, latest, update_available, channel, repo, branch, message }
            }
            Ok(Some(crate::control::ControlResponse::CommandResult { message, .. })) => {
                CheckInfo { at_ms: now_ms(), ok: false, current: String::new(), latest: None, update_available: false, channel: String::new(), repo: String::new(), branch: String::new(), message }
            }
            Ok(_) | Err(_) => CheckInfo {
                at_ms: now_ms(),
                ok: false,
                current: String::new(),
                latest: None,
                update_available: false,
                channel: String::new(),
                repo: String::new(),
                branch: String::new(),
                message: "no answer from the station".into(),
            },
        };
        let mut fleet = state.fleet.lock().unwrap_or_else(|e| e.into_inner());
        // Keep the running software: a failed check has no `current`.
        let current = fleet.stations().into_iter().find(|s| s.id == id).map(|s| s.version).unwrap_or_default();
        let check = CheckInfo { current: if check.current.is_empty() { current } else { check.current.clone() }, ..check };
        fleet.set_check(&id, check.clone());
        out.push(serde_json::json!({"id": id, "check": check}));
    }
    axum::Json(serde_json::json!({"results": out})).into_response()
}

#[derive(Deserialize)]
pub struct UpdateRequest {
    /// Control logins; empty: every station whose check found a newer version.
    #[serde(default)]
    pub ids: Vec<String>,
    pub mode: crate::fleet_update::Mode,
    #[serde(default)]
    pub during_call: bool,
    #[serde(default = "default_interrupt")]
    pub interrupt_min: u32,
}

fn default_interrupt() -> u32 {
    10
}

/// `POST /api/fleet/update` (Fleet Control management): start an update run.
pub async fn api_update(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<crate::state::AppState>>,
    headers: axum::http::HeaderMap,
    axum::Json(req): axum::Json<UpdateRequest>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let a = crate::dashboard::access(&state, &headers);
    if !a.manages_fleet() {
        return refuse();
    }
    if req.interrupt_min > crate::fleet_update::MAX_INTERRUPT_MIN {
        return (axum::http::StatusCode::BAD_REQUEST, "Interrupt time: at most 120 min").into_response();
    }
    let names = crate::control::station_names(&state);
    let mut fleet = state.fleet.lock().unwrap_or_else(|e| e.into_inner());
    let ids: Vec<String> = if req.ids.is_empty() {
        fleet.stations().into_iter().filter(|s| fleet.updatable(&s.id)).map(|s| s.id).collect()
    } else {
        req.ids.clone()
    };
    let options = crate::fleet_update::Options { mode: req.mode, during_call: req.during_call, interrupt_min: req.interrupt_min };
    match fleet.start_job(&ids, options, a.user.as_deref(), now_ms(), &|k| names.get(k).cloned()) {
        Ok(job) => {
            drop(fleet);
            state.monitor.emit("fleet_changed", serde_json::json!({"job": job}));
            axum::Json(serde_json::json!({"ok": true, "job": job})).into_response()
        }
        Err(e) => (axum::http::StatusCode::CONFLICT, e).into_response(),
    }
}

#[derive(Deserialize, Default)]
pub struct CancelRequest {
    /// One station; absent: the whole run ("Cancel all").
    #[serde(default)]
    pub id: Option<String>,
}

/// `POST /api/fleet/update/cancel` (Fleet Control management): no further
/// starts (the whole run, or one station that has not started yet).
pub async fn api_cancel(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<crate::state::AppState>>,
    headers: axum::http::HeaderMap,
    axum::Json(req): axum::Json<CancelRequest>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let a = crate::dashboard::access(&state, &headers);
    if !a.manages_fleet() {
        return refuse();
    }
    if state.fleet.lock().unwrap_or_else(|e| e.into_inner()).cancel(req.id.as_deref(), a.user.as_deref(), now_ms()) {
        axum::Json(serde_json::json!({"ok": true})).into_response()
    } else {
        (axum::http::StatusCode::NOT_FOUND, "Nothing to cancel").into_response()
    }
}

/// `POST /api/fleet/settings` `{"repo": "...", "branch": "..."}` (admins):
/// the server's "Updates repo".
pub async fn api_settings(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<crate::state::AppState>>,
    headers: axum::http::HeaderMap,
    axum::Json(req): axum::Json<UpdateSettings>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let a = crate::dashboard::access(&state, &headers);
    if !a.admin {
        return (axum::http::StatusCode::FORBIDDEN, "Admins only").into_response();
    }
    let repo = req.repo.trim().to_owned();
    let branch = req.branch.trim().to_owned();
    if !repo.is_empty() && !(repo.starts_with("https://") || repo.starts_with("git@") || repo.starts_with("ssh://")) {
        return (axum::http::StatusCode::BAD_REQUEST, "Repository: an https:// or ssh git URL").into_response();
    }
    if branch.chars().any(|c| c.is_whitespace() || matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\')) || branch.contains("..") {
        return (axum::http::StatusCode::BAD_REQUEST, "Branch: not a valid git branch name").into_response();
    }
    state.fleet.lock().unwrap_or_else(|e| e.into_inner()).set_update_settings(UpdateSettings { repo, branch }, a.user.as_deref(), now_ms());
    axum::Json(serde_json::json!({"ok": true})).into_response()
}

/// Background driver of update runs: every 2 s advance the run and send the
/// `StartUpdate` commands it asks for. Runs for the whole server lifetime, so
/// a run goes on with no browser open.
pub async fn run_updates(state: std::sync::Arc<crate::state::AppState>) {
    let mut every = tokio::time::interval(std::time::Duration::from_secs(2));
    loop {
        every.tick().await;
        let starts = state.fleet.lock().unwrap_or_else(|e| e.into_inner()).tick_job(now_ms());
        for s in starts {
            let state = state.clone();
            tokio::spawn(async move {
                let cmd = crate::control::ControlCommand::StartUpdate { handle: 0, during_call: s.during_call };
                let outcome = match crate::control::send_command(&state, &s.id, cmd).await {
                    Ok(Some(crate::control::ControlResponse::CommandResult { ok: true, .. })) => crate::fleet_update::StartOutcome::Started,
                    Ok(Some(crate::control::ControlResponse::CommandResult { code, message, .. })) => match code.as_str() {
                        "busy" => crate::fleet_update::StartOutcome::Started,
                        "call_active" => crate::fleet_update::StartOutcome::CallActive,
                        _ => crate::fleet_update::StartOutcome::Refused(code, message),
                    },
                    Ok(_) => crate::fleet_update::StartOutcome::Refused("unexpected".into(), "unexpected answer".into()),
                    Err(_) => crate::fleet_update::StartOutcome::Unreachable,
                };
                state.fleet.lock().unwrap_or_else(|e| e.into_inner()).start_result(&s.id, outcome, now_ms());
                state.monitor.emit("fleet_changed", serde_json::json!({"id": s.id}));
            });
        }
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello(on: bool, version: &str) -> Hello {
        Hello { protocol: 1, version: version.into(), brew_login: Some("1000001".into()), mcc: 232, mnc: 20, fleet_control: on, capabilities: if on { vec!["fleet-v1".into()] } else { vec![] } }
    }
    fn no_names(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn a_station_without_fleet_control_is_never_listed() {
        let mut f = Fleet::load(None, 0);
        assert!(!f.hello("bs1", &hello(false, "v1"), 1, &no_names));
        assert!(f.stations().is_empty() && f.log().is_empty());
    }

    #[test]
    fn switched_on_listed_offline_kept_switched_off_removed_with_info() {
        let mut f = Fleet::load(None, 0);
        assert!(f.hello("bs1", &hello(true, "v1"), 10, &no_names));
        assert_eq!(f.stations().len(), 1);
        f.set_online("bs1", false, 20);
        let s = f.get("bs1").unwrap();
        assert!(!s.online && s.last_seen_ms == 20, "offline stations stay listed");
        // Next login with the switch off: removed, INFO in the log.
        assert!(f.hello("bs1", &hello(false, "v1"), 30, &no_names));
        assert!(f.stations().is_empty());
        let log = f.log();
        assert_eq!(log[0].kind, "fleet_off");
        assert_eq!(log[0].text, "Basestation 1000001 switched off Fleet Control");
        assert_eq!(log[1].kind, "fleet_on");
    }

    #[test]
    fn name_from_the_server_and_version_change_logged() {
        let names = |k: &str| (k == "1000001").then(|| "Wien Nord".to_string());
        let mut f = Fleet::load(None, 0);
        f.hello("bs1", &hello(true, "v0.5.9-aaa"), 1, &names);
        f.hello("bs1", &hello(true, "v0.5.9-aaa"), 2, &names);
        f.hello("bs1", &hello(true, "v0.6.0-bbb"), 3, &names);
        let log = f.log();
        assert_eq!(log.len(), 2, "a reconnect with the same version is not a change");
        assert_eq!(log[0].text, "Basestation Wien Nord: software v0.5.9-aaa → v0.6.0-bbb");
        assert_eq!(f.get("bs1").unwrap().first_seen_ms, 1);
    }

    #[test]
    fn removed_by_a_user() {
        let mut f = Fleet::load(None, 0);
        f.hello("bs1", &hello(true, "v1"), 1, &no_names);
        assert!(f.remove("bs1", Some("alice"), 2, &no_names));
        assert!(!f.remove("bs1", Some("alice"), 3, &no_names));
        let e = &f.log()[0];
        assert_eq!((e.kind.as_str(), e.user.as_deref()), ("removed", Some("alice")));
    }

    #[test]
    fn log_keeps_ninety_days_and_survives_a_restart() {
        let dir = std::env::temp_dir().join(format!("fleet-test-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = Fleet::path_for(&dir.join("history.bin"));
        let day = 24 * 3600 * 1000;
        let mut f = Fleet::load(Some(path.clone()), 0);
        f.hello("old", &hello(true, "v1"), day, &no_names);
        f.hello("bs1", &hello(true, "v1"), 50 * day, &no_names);
        f.set_online("bs1", false, 50 * day);
        let g = Fleet::load(Some(path.clone()), 92 * day);
        assert_eq!(g.log().len(), 1, "the entry from day 1 is older than 90 days");
        assert_eq!(g.stations().len(), 2, "stations stay until they switch off or are removed");
        assert!(g.stations().iter().all(|s| !s.online), "after a restart nobody is online yet");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn repositories_compared_loosely() {
        assert!(same_repo("https://github.com/Aitorrio/bost-flowstation", "https://github.com/aitorrio/bost-flowstation.git/"));
        assert!(same_repo("https://user:token@github.com/a/b.git", "https://github.com/a/b"));
        assert!(!same_repo("https://github.com/sebastianmadl/bost-flowstation", DEFAULT_UPDATE_REPO));
        let empty = UpdateSettings::default();
        assert_eq!(empty.effective(), (DEFAULT_UPDATE_REPO.to_string(), "main".to_string()));
        let fork = UpdateSettings { repo: " https://github.com/sebastianmadl/bost-flowstation ".into(), branch: "handover".into() };
        assert_eq!(fork.effective().1, "handover");
    }

    fn check(current: &str, newer: bool) -> CheckInfo {
        CheckInfo { at_ms: 1, ok: true, current: current.into(), latest: Some("v2".into()), update_available: newer, channel: "installed".into(), repo: DEFAULT_UPDATE_REPO.into(), branch: "main".into(), message: String::new() }
    }

    #[test]
    fn updatable_only_after_a_check_for_the_running_software() {
        let mut f = Fleet::load(None, 0);
        let mut h = hello(true, "v1");
        h.capabilities.push(UPDATE_CAPABILITY.into());
        f.hello("bs1", &h, 1, &no_names);
        assert!(!f.updatable("bs1"), "no check yet");
        f.set_check("bs1", check("v1", false));
        assert!(!f.updatable("bs1"), "up to date");
        f.set_check("bs1", check("v1", true));
        assert!(f.updatable("bs1"));
        f.set_online("bs1", false, 2);
        assert!(!f.updatable("bs1"), "offline");
        let mut h2 = hello(true, "v2");
        h2.capabilities.push(UPDATE_CAPABILITY.into());
        f.hello("bs1", &h2, 3, &no_names);
        assert!(f.get("bs1").unwrap().check.is_none(), "a check for other software is dropped");
        // Without update-v1 (older station) never.
        f.hello("bs2", &hello(true, "v1"), 4, &no_names);
        f.set_check("bs2", check("v1", true));
        assert!(!f.updatable("bs2"));
    }

    #[test]
    fn a_run_cannot_start_twice_and_the_job_survives_a_restart() {
        let dir = std::env::temp_dir().join(format!("fleet-job-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = Fleet::path_for(&dir.join("history.bin"));
        let mut f = Fleet::load(Some(path.clone()), 0);
        let mut h = hello(true, "v1");
        h.capabilities.push(UPDATE_CAPABILITY.into());
        f.hello("bs1", &h, 1, &no_names);
        f.set_check("bs1", check("v1", true));
        let o = crate::fleet_update::Options { mode: crate::fleet_update::Mode::Rolling, during_call: false, interrupt_min: 10 };
        assert!(f.start_job(&["bs1".into()], o.clone(), Some("alice"), 2, &no_names).is_ok());
        assert!(f.start_job(&["bs1".into()], o, Some("alice"), 3, &no_names).is_err());
        let starts = f.tick_job(4);
        assert_eq!(starts.len(), 1);
        let g = Fleet::load(Some(path), 5);
        let job = g.job().unwrap();
        assert!(!job.is_finished());
        assert_eq!(job.items[0].state, crate::fleet_update::ItemState::Pending, "the unanswered start is tried again after the restart");
        assert!(g.log().iter().any(|e| e.text.starts_with("Update run started (rolling")));
        std::fs::remove_dir_all(&dir).ok();
    }

}
