//! Cell change log ("Cell Change Log" on the dashboard): the last cell
//! changes of the radios. Not "roaming" in the ETSI sense (a change of
//! location area, TS 100 392-2), although the module keeps its name.
//!
//! A change is logged when an ISSI registers on another Basestation than the
//! one holding it (announced reselection prepared here, or any other), and when a
//! station reports the radio on another of its own cells (`SERVICE_MS_CELL`,
//! multi-cell stations). The target cell of a move between stations is only
//! known once the new station reports it, so the entry is completed by that
//! report. Kept in memory only.

use serde::Serialize;
use std::collections::VecDeque;

/// Entries kept.
pub const MAX_ENTRIES: usize = 200;

/// How long after a move the new station's cell report still completes it.
const CELL_REPORT_MS: u64 = 10_000;

/// How the radio changed cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Announced cell reselection (TS 100 392-2 clause 18.3.4.7) prepared
    /// through this server's handover relay. Shown as "announced"; the wire
    /// value stays `handover` for API compatibility.
    Handover,
    /// The radio registered on another Basestation without a preparation
    /// here: unannounced or undeclared reselection, or announced to a cell
    /// this server did not prepare. Shown as "not announced".
    Reselection,
    /// Another cell of the same station (multi-cell FlowStation).
    WithinStation,
}

/// Where a radio was: the station (its Brew login, see
/// `carriers::station_name`) and, when reported, the cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Place {
    pub station: String,
    /// Cell name `<station>/<cell>`, when known.
    pub cell: Option<String>,
    pub carrier: Option<u16>,
}

impl Place {
    pub fn station(station: String) -> Self {
        Self { station, cell: None, carrier: None }
    }

    pub fn cell(station: String, cell: u8, carrier: u16) -> Self {
        let name = crate::carriers::cell_name(&station, cell);
        Self { station, cell: Some(name), carrier: Some(carrier) }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entry {
    pub at_ms: u64,
    pub issi: u32,
    pub from: Place,
    pub to: Place,
    pub kind: Kind,
}

#[derive(Default)]
pub struct RoamingLog {
    entries: VecDeque<Entry>,
}

impl RoamingLog {
    pub fn push(&mut self, entry: Entry) {
        if self.entries.len() == MAX_ENTRIES { self.entries.pop_front(); }
        self.entries.push_back(entry);
    }

    /// `station` reported `issi` on `to` at `now_ms`, coming from cell
    /// `previous` of the same station (if it had reported one). Completes the
    /// ISSI's latest move to this station when that still lacks its cell,
    /// otherwise logs a change within the station when the cell is another.
    pub fn cell_reported(&mut self, now_ms: u64, issi: u32, to: Place, previous: Option<Place>) {
        let open = self.entries.iter_mut().rev().find(|e| e.issi == issi)
            .filter(|e| e.to.station == to.station && e.to.cell.is_none() && now_ms.saturating_sub(e.at_ms) <= CELL_REPORT_MS);
        if let Some(entry) = open {
            entry.to = to;
            return;
        }
        if let Some(from) = previous.filter(|p| p.cell != to.cell) {
            self.push(Entry { at_ms: now_ms, issi, from, to, kind: Kind::WithinStation });
        }
    }

    /// Newest first.
    pub fn newest_first(&self) -> Vec<Entry> {
        self.entries.iter().rev().cloned().collect()
    }
}

/// Where `client` has `issi`: its station and, when reported, the cell.
fn place(inner: &crate::state::Inner, issi: u32, client: crate::state::ClientId) -> Place {
    let station = crate::carriers::station_name(client, inner.clients.get(&client).and_then(|c| c.username.as_deref()));
    match inner.carriers.ms_cell(issi, client) {
        Some(cell) => Place::cell(station, cell.cell, cell.key.carrier),
        None => Place::station(station),
    }
}

fn is_basestation(inner: &crate::state::Inner, id: crate::state::ClientId) -> bool {
    inner.clients.get(&id).is_some_and(|c| c.mode == crate::state::ClientMode::Basestation)
}

/// `issi`, held by Basestation `from`, registered on Basestation `to`
/// (`router::handle_subscriber`, before the registration moves it).
/// `handed_over`: it came with a handover (`handover::Handovers`).
pub fn registration_moved(inner: &mut crate::state::Inner, issi: u32, from: crate::state::ClientId, to: crate::state::ClientId, handed_over: bool) {
    if from == to || !is_basestation(inner, from) || !is_basestation(inner, to) { return; }
    let entry = Entry {
        at_ms: crate::telemetry::now_ms(),
        issi,
        from: place(inner, issi, from),
        // Its cell there follows with the station's report (`cell_reported`).
        to: Place::station(crate::carriers::station_name(to, inner.clients.get(&to).and_then(|c| c.username.as_deref()))),
        kind: if handed_over { Kind::Handover } else { Kind::Reselection },
    };
    tracing::info!(issi, from = ?entry.from.cell.as_ref().unwrap_or(&entry.from.station), to = %entry.to.station, kind = ?entry.kind, "subscriber changed cells");
    inner.roaming_log.push(entry);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn moved(at_ms: u64, issi: u32, kind: Kind) -> Entry {
        Entry { at_ms, issi, from: Place::cell("bs1".into(), 0, 1528), to: Place::station("bs2".into()), kind }
    }

    use crate::{protocol::{self, ConnVersion, SUB_REGISTER}, state::{AppState, Client, ClientId, ClientMode}};
    use std::sync::Arc;

    async fn station(state: &Arc<AppState>, login: &str, mode: ClientMode, carriers: &[u16]) -> ClientId {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        std::mem::forget(rx);
        let id = uuid::Uuid::new_v4();
        state.inner.write().await.clients.insert(id, Client { tx, mode, version: ConnVersion::V0, version_announced: false, remote_addr: None, connected_at_ms: 0, username: Some(login.into()) });
        let cells: Vec<String> = carriers.iter().enumerate()
            .map(|(i, c)| format!(r#"{{"cell":{i},"carrier":{c},"carrier_ext":268,"location_area":{c}}}"#)).collect();
        let info = format!(r#"{{"mcc":232,"mnc":5,"cells":[{}],"capabilities":["handover-v1"]}}"#, cells.join(","));
        crate::router::handle_packet(state.clone(), id, protocol::build_service(protocol::SERVICE_BTS_INFO, &info)).await;
        id
    }

    async fn register(state: &Arc<AppState>, from: ClientId, issi: u32, carrier: Option<u16>) {
        crate::router::handle_packet(state.clone(), from, protocol::build_subscriber_message(SUB_REGISTER, issi, &[])).await;
        if let Some(c) = carrier {
            crate::router::handle_packet(state.clone(), from, protocol::build_service(protocol::SERVICE_MS_CELL,
                &format!(r#"{{"issi":{issi},"carrier":{c},"carrier_ext":268}}"#))).await;
        }
    }

    async fn log(state: &Arc<AppState>) -> Vec<(u32, Option<String>, String, Option<String>, Kind)> {
        state.inner.read().await.roaming_log.newest_first().into_iter()
            .map(|e| (e.issi, e.from.cell, e.to.station, e.to.cell, e.kind)).collect()
    }

    #[tokio::test]
    async fn moves_between_and_within_stations_are_logged() {
        let mut config = crate::config::Config::default();
        config.storage.enabled = false;
        config.sms_center.enabled = false;
        config.handover.timeout_ms = 20;
        let state = Arc::new(AppState::new(config, "test.toml".into()).0);
        let a = station(&state, "bs1", ClientMode::Basestation, &[1528]).await;
        let b = station(&state, "bs2", ClientMode::Basestation, &[1530]).await;
        let site = station(&state, "site", ClientMode::Basestation, &[1532, 1534]).await;
        register(&state, a, 7, Some(1528)).await;
        assert!(log(&state).await.is_empty(), "a first registration is no cell change");

        // Announced handover A -> B: B registers while the request is open.
        crate::router::handle_packet(state.clone(), a, protocol::build_service(protocol::SERVICE_HO_FORWARD_REG,
            r#"{"transaction_id":1,"issi":7,"target_carrier":1530,"target_carrier_ext":268,"sdu":"00","sdu_len_bits":8}"#)).await;
        register(&state, b, 7, Some(1530)).await;
        // B never answers: the request times out and later moves to B are no handover.
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        // Unannounced back to A, then to the multi-cell site and across its cells.
        register(&state, a, 7, Some(1528)).await;
        register(&state, site, 7, Some(1532)).await;
        register(&state, site, 7, Some(1534)).await;
        // A station that does not report cells: only the station is known.
        register(&state, b, 7, None).await;

        assert_eq!(log(&state).await, vec![
            (7, Some("site/1".into()), "bs2".into(), None, Kind::Reselection),
            (7, Some("site/0".into()), "site".into(), Some("site/1".into()), Kind::WithinStation),
            (7, Some("bs1/0".into()), "site".into(), Some("site/0".into()), Kind::Reselection),
            (7, Some("bs2/0".into()), "bs1".into(), Some("bs1/0".into()), Kind::Reselection),
            (7, Some("bs1/0".into()), "bs2".into(), Some("bs2/0".into()), Kind::Handover),
        ]);
    }

    #[tokio::test]
    async fn reregistering_on_the_same_station_and_peers_are_not_moves() {
        let state = AppState::for_test();
        let a = station(&state, "bs1", ClientMode::Basestation, &[1528]).await;
        let peer = station(&state, "peer", ClientMode::Peer, &[]).await;
        let terminal = station(&state, "term", ClientMode::Terminal, &[]).await;
        register(&state, a, 7, Some(1528)).await;
        register(&state, a, 7, Some(1528)).await;
        register(&state, peer, 7, None).await;
        register(&state, a, 8, None).await;
        register(&state, terminal, 8, None).await;
        assert!(log(&state).await.is_empty());
    }

    #[test]
    fn keeps_the_newest_entries() {
        let mut log = RoamingLog::default();
        for i in 0..(MAX_ENTRIES as u32 + 5) { log.push(moved(u64::from(i), i, Kind::Reselection)); }
        let all = log.newest_first();
        assert_eq!(all.len(), MAX_ENTRIES);
        assert_eq!(all[0].issi, MAX_ENTRIES as u32 + 4);
        assert_eq!(all.last().unwrap().issi, 5);
    }

    #[test]
    fn cell_report_completes_the_move() {
        let mut log = RoamingLog::default();
        log.push(moved(1000, 7, Kind::Handover));
        log.cell_reported(1500, 7, Place::cell("bs2".into(), 0, 1530), None);
        let all = log.newest_first();
        assert_eq!(all.len(), 1);
        assert_eq!((all[0].to.cell.as_deref(), all[0].to.carrier), (Some("bs2/0"), Some(1530)));
        // A second report of the same cell adds nothing.
        log.cell_reported(2000, 7, Place::cell("bs2".into(), 0, 1530), Some(Place::cell("bs2".into(), 0, 1530)));
        assert_eq!(log.newest_first().len(), 1);
    }

    #[test]
    fn late_or_foreign_reports_do_not_complete_a_move() {
        let mut log = RoamingLog::default();
        log.push(moved(1000, 7, Kind::Reselection));
        log.cell_reported(1000 + CELL_REPORT_MS + 1, 7, Place::cell("bs2".into(), 0, 1530), None);
        log.cell_reported(1500, 7, Place::cell("bs3".into(), 0, 1532), None);
        assert!(log.newest_first()[0].to.cell.is_none());
    }

    #[test]
    fn another_cell_of_the_station_is_a_move_within_it() {
        let mut log = RoamingLog::default();
        log.cell_reported(1000, 7, Place::cell("site".into(), 3, 1530), Some(Place::cell("site".into(), 0, 1528)));
        let all = log.newest_first();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].kind, Kind::WithinStation);
        assert_eq!((all[0].from.cell.as_deref(), all[0].to.cell.as_deref()), (Some("site/0"), Some("site/3")));
        // The first report of a radio is no move.
        log.cell_reported(1000, 8, Place::cell("site".into(), 0, 1528), None);
        assert_eq!(log.newest_first().len(), 1);
    }
}
