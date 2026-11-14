//! Fleet Control key management, phase 8d: what a station does while this server (its key server,
//! the authentication centre) is unreachable, and how it comes back. Design worked out with
//! Patrick, OE1LHP.
//!
//! The server only hands out the settings and the fallback SCK; the station decides on its own,
//! because it is cut off when it matters:
//!
//! - It authenticates from its stock of data sets as long as it has some.
//! - After `fallback_min` minutes without the server, once a radio could not be authenticated, it
//!   goes to fallback: "system wide services temporarily not supported" (TS 100 392-2 clause
//!   18.5.2) and "temporary registration" for radios without data sets (clause 16.4.8, NOTE 2).
//!   A class 3 cell holding the fallback SCK (SCKN 31 or 32, EN 300 392-7 clause 4.2.4.0 and
//!   Table A.96) runs class 2 on it (clause 4.5.6.2).
//! - When the server is back it fetches fresh data sets first, authenticates the radios that
//!   registered without authentication again at no more than `reauth_per_s` per second (clause
//!   4.1.2), and only then returns to class 3 (clause 4.5.6.3) and normal mode.
//!
//! The fallback SCK reaches the radios like the class 2 SCKs (phase 8b, sealed per radio,
//! clause 4.5.2.2), so they hold it before anything fails. Stations report their fallback
//! periods (`FallbackState`); the server writes each one into the change log once.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::control::ControlCommand;
use crate::fleet_sck::{net_name, SckRef};
use crate::state::AppState;

/// Station capability: understands `FallbackKeys`, reports `FallbackState`.
pub const CAP_FALLBACK: &str = "fallback-v1";

/// A fallback period of a station, station time (Unix ms).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FallbackEpisode {
    pub from_ms: u64,
    #[serde(default)]
    pub to_ms: Option<u64>,
    #[serde(default)]
    pub class2: bool,
    #[serde(default)]
    pub temporary: u32,
}

/// What a station last reported (not stored).
#[derive(Debug, Clone, Default, Serialize)]
pub struct StationFallback {
    pub phase: String,
    pub key: Option<SckRef>,
    pub episodes: Vec<FallbackEpisode>,
    pub at_ms: u64,
}

#[derive(Debug, Default)]
pub struct FallbackRuntime {
    pub stations: HashMap<String, StationFallback>,
    /// Periods already in the change log: (station, start, ended).
    logged: HashSet<(String, u64, bool)>,
}

fn keystore(state: &AppState) -> std::sync::MutexGuard<'_, crate::fleet_keys::KeyStore> {
    state.fleet_keys.lock().unwrap_or_else(|e| e.into_inner())
}

/// Stations that take part (online, keys from the server, `fallback-v1`) with their network.
fn stations(state: &AppState) -> Vec<(String, String)> {
    let fleet = state.fleet.lock().unwrap_or_else(|e| e.into_inner());
    fleet
        .stations()
        .into_iter()
        .filter(|s| s.online && s.key_source.as_deref() == Some("server") && s.capabilities.iter().any(|c| c == CAP_FALLBACK))
        .map(|s| (s.id.clone(), net_name(s.mcc, s.mnc)))
        .collect()
}

/// `FallbackKeys` for a station of `net`.
pub fn command(ks: &crate::fleet_keys::KeyStore, net: &str) -> ControlCommand {
    let d = ks.sck();
    ControlCommand::FallbackKeys {
        key: crate::fleet_sck::fallback_of(ks, net),
        after_s: d.fallback_min.clamp(1, 60) as u32 * 60,
        reauth_per_s: d.reauth_per_s.clamp(1, 20),
    }
}

/// After a hello: a station that takes part gets the settings and the fallback SCK of its network.
pub fn after_hello(state: &Arc<AppState>, id: &str) {
    let Some((_, net)) = stations(state).into_iter().find(|(s, _)| s == id) else {
        keystore(state).fallback_rt.stations.remove(id);
        return;
    };
    let cmd = command(&keystore(state), &net);
    crate::fleet_keys::send(state, id, cmd);
}

/// The fallback SCK of `net` changed: its stations get it.
pub fn push_net(state: &Arc<AppState>, net: &str) {
    let cmd = command(&keystore(state), net);
    for (id, _) in stations(state).into_iter().filter(|(_, n)| n == net) {
        crate::fleet_keys::send(state, &id, cmd.clone());
    }
}

/// The settings changed: every station gets them.
pub fn push_all(state: &Arc<AppState>) {
    for (id, net) in stations(state) {
        let cmd = command(&keystore(state), &net);
        crate::fleet_keys::send(state, &id, cmd);
    }
}

/// Unix ms as `YYYY-MM-DD HH:MM:SS UTC` (civil date from days, proleptic Gregorian).
fn utc(ms: u64) -> String {
    let secs = ms / 1000;
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + (m <= 2) as i64;
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// The change log lines for periods not logged yet (start, and end with its numbers).
fn new_log_lines(rt: &mut FallbackRuntime, id: &str, episodes: &[FallbackEpisode]) -> Vec<String> {
    let mut out = Vec::new();
    let when = utc;
    for e in episodes {
        let mode = if e.class2 { "class 2 on the fallback SCK" } else { "same security class" };
        if rt.logged.insert((id.to_owned(), e.from_ms, false)) && e.to_ms.is_none() {
            out.push(format!("in key fallback since {} ({mode}): this server was unreachable", when(e.from_ms)));
        }
        if let Some(to) = e.to_ms.filter(|_| rt.logged.insert((id.to_owned(), e.from_ms, true))) {
            out.push(format!(
                "key fallback from {} to {} ({mode}, {} temporary registration(s)), back to normal mode",
                when(e.from_ms),
                when(to),
                e.temporary
            ));
        }
    }
    out
}

/// A station reports its fallback state (after `FallbackKeys` and on every change).
pub fn on_state(state: &Arc<AppState>, id: &str, phase: String, key: Option<SckRef>, episodes: Vec<FallbackEpisode>) {
    let lines = {
        let mut ks = keystore(state);
        let rt = &mut ks.fallback_rt;
        let lines = new_log_lines(rt, id, &episodes);
        rt.stations.insert(id.to_owned(), StationFallback { phase, key, episodes, at_ms: crate::fleet::now_ms() });
        lines
    };
    if !lines.is_empty() {
        let names = crate::control::station_names(state);
        let mut fleet = state.fleet.lock().unwrap_or_else(|e| e.into_inner());
        for line in lines {
            fleet.log_station(id, None, "keys_fallback", |label| format!("Basestation {label}: {line}"), crate::fleet::now_ms(), &|k| names.get(k).cloned());
        }
    }
    state.monitor.emit("fleet_changed", serde_json::json!({"keys": true}));
}

/// For `GET /api/fleet/keys`: per station phase, fallback SCK held, last period.
pub fn view(ks: &crate::fleet_keys::KeyStore) -> serde_json::Value {
    let d = ks.sck();
    serde_json::json!({
        "fallback_min": d.fallback_min,
        "reauth_per_s": d.reauth_per_s,
        "stations": ks.fallback_rt.stations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet_keys::KeyStore;

    #[test]
    fn the_command_carries_the_networks_fallback_sck_and_the_settings() {
        let mut ks = KeyStore::memory();
        let ControlCommand::FallbackKeys { key, after_s, reauth_per_s } = command(&ks, "232-1") else { panic!() };
        assert!(key.is_none());
        assert_eq!((after_s, reauth_per_s), (300, 2), "defaults: 5 min, 2 radios per second");
        crate::fleet_sck::set_fallback(&mut ks, "232-1", 31, 4, Some([0x5A; 10]), None, 0).unwrap();
        ks.sck_mut().fallback_min = 10;
        ks.sck_mut().reauth_per_s = 5;
        let ControlCommand::FallbackKeys { key, after_s, reauth_per_s } = command(&ks, "232-1") else { panic!() };
        assert_eq!(key.map(|k| (k.sckn, k.vn, k.key)), Some((31, 4, "5a".repeat(10))));
        assert_eq!((after_s, reauth_per_s), (600, 5));
        assert!(matches!(command(&ks, "232-2"), ControlCommand::FallbackKeys { key: None, .. }), "per network");
    }

    #[test]
    fn utc_times_read_like_a_clock() {
        assert_eq!(utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(utc(1_794_610_800_000), "2026-11-13 23:00:00 UTC");
        assert_eq!(utc(951_782_400_000), "2000-02-29 00:00:00 UTC");
    }

    #[test]
    fn each_period_is_logged_once_at_start_and_once_at_the_end() {
        let mut rt = FallbackRuntime::default();
        let open = FallbackEpisode { from_ms: 1_000, to_ms: None, class2: true, temporary: 0 };
        assert_eq!(new_log_lines(&mut rt, "bs1", &[open.clone()]).len(), 1);
        assert!(new_log_lines(&mut rt, "bs1", &[open]).is_empty(), "reported again: not logged twice");
        let closed = FallbackEpisode { from_ms: 1_000, to_ms: Some(61_000), class2: true, temporary: 3 };
        let lines = new_log_lines(&mut rt, "bs1", &[closed.clone()]);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("3 temporary"), "{lines:?}");
        assert!(new_log_lines(&mut rt, "bs1", &[closed.clone()]).is_empty());
        // A period seen only after it ended (the station was cut off all along): the end line.
        let lines = new_log_lines(&mut rt, "bs2", &[closed]);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("back to normal mode"));
    }

    #[test]
    fn wire_shapes_match_the_station() {
        let cmd = ControlCommand::FallbackKeys { key: None, after_s: 300, reauth_per_s: 2 };
        assert_eq!(cmd.to_wire_json(), serde_json::json!({"FallbackKeys": {"key": null, "after_s": 300, "reauth_per_s": 2}}));
        let r: crate::control::ControlResponse = serde_json::from_value(serde_json::json!({"FallbackState": {
            "phase": "fallback", "key": {"sckn": 31, "vn": 4, "check": "AB"},
            "episodes": [{"from_ms": 5, "to_ms": null, "class2": true, "temporary": 0}]
        }}))
        .unwrap();
        assert!(matches!(r, crate::control::ControlResponse::FallbackState { ref phase, key: Some(_), ref episodes } if phase == "fallback" && episodes.len() == 1));
    }
}
