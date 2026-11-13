//! Fleet Control key management, phase 8e: security class 3 radios moving between stations.
//!
//! A class 3 radio encrypts everything it sends with its DCK, which comes out of the
//! authentication at the station it registered with (EN 300 392-7 clause 4.2.1). "Within an LA of
//! security class 3 all cells shall have knowledge of the DCK in use for each ITSI operating in
//! that LA" (clause 6.6.1.1), and on an announced reselection to a class 3 cell "the SwMI shall
//! ensure that the new serving cell, and the LA to which it belongs, has DCK of the roaming ITSI"
//! (clause 4.5.1.3). How the SwMI moves the DCK inside itself is not specified.
//!
//! Each station reports the DCK of a radio that authenticated there (`DckReport`); the server
//! passes it on to the other stations of the same network that share a location area with that
//! station or are its neighbours (`DckKeys`). The DCKs live in memory only: a DCK is valid for one
//! authentication session, and the next authentication anywhere replaces it.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::control::ControlCommand;
use crate::fleet_bulk::DirCell;
use crate::fleet_sck::net_name;
use crate::state::AppState;

/// Station capability: reports its radios' DCKs and takes those of other stations.
pub const CAP_DCK: &str = "dck-v1";

/// One DCK on the wire: radio and 20 hex digits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DckWire {
    pub issi: u32,
    pub dck: String,
}

/// The DCK in use per radio and the station that reported it. Never written to disk.
#[derive(Default)]
pub struct DckRuntime {
    pub dcks: HashMap<u32, (String, DckWire)>,
}

impl std::fmt::Debug for DckRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DckRuntime({} radios)", self.dcks.len())
    }
}

/// A station that takes part: online, keys from the server, `dck-v1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DckStation {
    pub id: String,
    pub login: Option<String>,
    pub net: String,
}

fn valid(w: &DckWire) -> bool {
    w.dck.len() == 20 && w.dck.chars().all(|c| c.is_ascii_hexdigit())
}

/// Location areas of the cells of Brew login `login` in network `net`.
fn las(dir: &[DirCell], login: &str, net: &str) -> HashSet<u16> {
    dir.iter().filter(|d| d.station == login && net_name(d.mcc, d.mnc) == net).map(|d| d.location_area).collect()
}

/// Stations that get the DCKs reported by `from`: the other stations of its network that share
/// a location area with it (clause 6.6.1.1) or are its neighbours (clause 4.5.1.3).
pub fn targets(from: &DckStation, stations: &[DckStation], dir: &[DirCell]) -> Vec<String> {
    let Some(login) = from.login.as_deref() else { return Vec::new() };
    let own = las(dir, login, &from.net);
    let neighbours = crate::fleet_keys::neighbour_stations(dir, login);
    stations
        .iter()
        .filter(|s| s.id != from.id && s.net == from.net)
        .filter(|s| {
            s.login.as_deref().is_some_and(|l| neighbours.contains(l) || !las(dir, l, &s.net).is_disjoint(&own))
        })
        .map(|s| s.id.clone())
        .collect()
}

fn dck_stations(state: &AppState) -> Vec<DckStation> {
    let fleet = state.fleet.lock().unwrap_or_else(|e| e.into_inner());
    fleet
        .stations()
        .into_iter()
        .filter(|s| s.online && s.key_source.as_deref() == Some("server") && s.capabilities.iter().any(|c| c == CAP_DCK))
        .map(|s| DckStation { id: s.id.clone(), login: s.brew_login.clone(), net: net_name(s.mcc, s.mnc) })
        .collect()
}

fn runtime(state: &AppState) -> std::sync::MutexGuard<'_, crate::fleet_keys::KeyStore> {
    state.fleet_keys.lock().unwrap_or_else(|e| e.into_inner())
}

/// A station reports the new DCK of a radio that authenticated there: kept, and sent to the
/// stations of the same LA and next door.
pub async fn on_report(state: &Arc<AppState>, id: &str, key: DckWire) {
    let stations = dck_stations(state);
    let Some(from) = stations.iter().find(|s| s.id == id).cloned() else { return };
    if !valid(&key) {
        tracing::warn!(station = id, issi = key.issi, "DCK report with a malformed key ignored");
        return;
    }
    runtime(state).dck_rt.dcks.insert(key.issi, (id.to_owned(), key.clone()));
    let dir = crate::fleet_bulk::directory(&*state.inner.read().await);
    let to = targets(&from, &stations, &dir);
    tracing::debug!(station = id, issi = key.issi, stations = to.len(), "DCK passed on");
    for t in to {
        crate::fleet_keys::send(state, &t, ControlCommand::DckKeys { keys: vec![key.clone()], drop: Vec::new() });
    }
}

/// After a hello: a station that takes part gets the DCKs of its LA and its neighbours (also
/// when there are none: then it knows that the server forwards DCKs).
pub fn after_hello(state: &Arc<AppState>, id: &str) {
    let (state, id) = (state.clone(), id.to_owned());
    tokio::spawn(async move {
        let stations = dck_stations(&state);
        if !stations.iter().any(|s| s.id == id) {
            return;
        }
        let dir = crate::fleet_bulk::directory(&*state.inner.read().await);
        let known: Vec<(String, DckWire)> = runtime(&state).dck_rt.dcks.values().cloned().collect();
        let keys: Vec<DckWire> = known
            .into_iter()
            .filter(|(from, _)| {
                stations.iter().find(|s| &s.id == from).is_some_and(|f| targets(f, &stations, &dir).contains(&id))
            })
            .map(|(_, k)| k)
            .collect();
        let _ = crate::control::send_command(&state, &id, ControlCommand::DckKeys { keys, drop: Vec::new() }).await;
    });
}

/// K of these radios changed or was removed: their DCKs are void, every station drops them.
pub fn drop_radios(state: &Arc<AppState>, issis: &[u32]) {
    if issis.is_empty() {
        return;
    }
    {
        let mut ks = runtime(state);
        for issi in issis {
            ks.dck_rt.dcks.remove(issi);
        }
    }
    for s in dck_stations(state) {
        crate::fleet_keys::send(state, &s.id, ControlCommand::DckKeys { keys: Vec::new(), drop: issis.to_vec() });
    }
}

/// Count for the Keys page.
pub fn view(ks: &crate::fleet_keys::KeyStore) -> serde_json::Value {
    serde_json::json!({ "radios": ks.dck_rt.dcks.len() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(station: &str, carrier: u16, la: u16, neighbours: &[u16]) -> DirCell {
        DirCell {
            name: format!("{station}/0"),
            station: station.into(),
            cell: 0,
            online: true,
            mcc: 232,
            mnc: 20,
            carrier,
            carrier_ext: 0,
            location_area: la,
            rxlev_access_min_dbm: None,
            ms_txpwr_max_dbm: None,
            service_details: None,
            neighbours: Some(neighbours.iter().map(|&n| (232, 20, n, 0)).collect()),
        }
    }

    fn st(id: &str, login: &str) -> DckStation {
        DckStation { id: id.into(), login: Some(login.into()), net: "232-20".into() }
    }

    #[test]
    fn dcks_go_to_the_same_la_and_the_neighbours_only() {
        // a and b share LA 1; c is a's neighbour in LA 2; d is far away; e is another network.
        let dir = vec![cell("a", 100, 1, &[300]), cell("b", 200, 1, &[]), cell("c", 300, 2, &[]), cell("d", 400, 3, &[])];
        let mut stations = vec![st("A", "a"), st("B", "b"), st("C", "c"), st("D", "d")];
        stations.push(DckStation { id: "E".into(), login: Some("b".into()), net: "232-1".into() });
        let mut t = targets(&stations[0], &stations, &dir);
        t.sort();
        assert_eq!(t, vec!["B", "C"]);
        // The neighbour relation counts both ways: c's DCKs go to a, not to b.
        assert_eq!(targets(&stations[2], &stations, &dir), vec!["A"]);
        assert!(targets(&stations[3], &stations, &dir).is_empty());
    }

    #[test]
    fn a_station_without_a_brew_login_gets_and_gives_nothing() {
        let dir = vec![cell("a", 100, 1, &[]), cell("b", 200, 1, &[])];
        let lone = DckStation { id: "X".into(), login: None, net: "232-20".into() };
        let stations = vec![st("A", "a"), lone.clone()];
        assert!(targets(&lone, &stations, &dir).is_empty());
        assert!(targets(&stations[0], &stations, &dir).is_empty());
    }

    #[test]
    fn only_20_hex_digits_are_a_dck() {
        assert!(valid(&DckWire { issi: 1, dck: "0123456789abcdefABCD".into() }));
        assert!(!valid(&DckWire { issi: 1, dck: "0123".into() }));
        assert!(!valid(&DckWire { issi: 1, dck: "0123456789abcdefABCX".into() }));
    }

    #[test]
    fn debug_never_shows_a_key() {
        let mut rt = DckRuntime::default();
        rt.dcks.insert(5, ("A".into(), DckWire { issi: 5, dck: "11223344556677889900".into() }));
        assert_eq!(format!("{rt:?}"), "DckRuntime(1 radios)");
    }

    /// Ten stations in five LAs along a line (each the neighbour of the next), 25 radios: every
    /// report reaches exactly the other station of its LA and the stations next door.
    #[test]
    fn simulation_10_stations_25_radios() {
        let dir: Vec<DirCell> = (0..10u16)
            .map(|i| {
                let mut n = Vec::new();
                if i < 9 {
                    n.push(1000 + i + 1);
                }
                cell(&format!("s{i}"), 1000 + i, i / 2 + 1, &n)
            })
            .collect();
        let stations: Vec<DckStation> = (0..10).map(|i| st(&format!("S{i}"), &format!("s{i}"))).collect();
        let mut rt = DckRuntime::default();
        for r in 0..25u32 {
            let from = (r % 10) as usize;
            rt.dcks.insert(2_360_000 + r, (stations[from].id.clone(), DckWire { issi: 2_360_000 + r, dck: format!("{:020X}", r) }));
            let mut t = targets(&stations[from], &stations, &dir);
            t.sort();
            let mut want: Vec<String> = (0..10usize)
                .filter(|&i| i != from && (i / 2 == from / 2 || i + 1 == from || from + 1 == i))
                .map(|i| format!("S{i}"))
                .collect();
            want.sort();
            assert_eq!(t, want, "radio {r} at S{from}");
        }
        assert_eq!(rt.dcks.len(), 25);
    }
}
