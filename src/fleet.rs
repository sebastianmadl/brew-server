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
    /// `fleet_on`, `fleet_off`, `version`, `removed`.
    pub kind: String,
    pub text: String,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    stations: BTreeMap<String, Station>,
    log: VecDeque<LogEntry>,
    next_id: u64,
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
        };
        match &entry {
            None => self.push(now_ms, id, label.clone(), None, "fleet_on", format!("Basestation {label} switched on Fleet Control")),
            Some(old) if old.version != h.version => {
                self.push(now_ms, id, label.clone(), None, "version", format!("Basestation {label}: software {} → {}", old.version, h.version))
            }
            Some(_) => {}
        }
        self.data.stations.insert(id.to_owned(), station);
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

/// Station as the Fleet Manager shows it.
#[derive(Serialize)]
struct StationView {
    #[serde(flatten)]
    station: Station,
    online: bool,
    label: String,
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
    let stations: Vec<StationView> = fleet
        .stations()
        .into_iter()
        .map(|s| StationView { online: s.online, label: label(&lookup, &s.id, s.brew_login.as_deref()), station: s })
        .collect();
    axum::Json(serde_json::json!({
        "offered": state.config.control.enabled,
        "protocol": PROTOCOL,
        "can_manage": a.manages_fleet(),
        "stations": stations,
        "log": fleet.log(),
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
}
