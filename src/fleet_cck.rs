//! Fleet Control key management, phase 8c: the common cipher keys (CCK) of security class 3
//! networks, one per location area (EN 300 392-7 clause 4.2.3), kept by the server.
//!
//! Per network the server holds, per location area or for every LA at once ("all", the same
//! CCK-id in each LA, clause 4.2.3), the current CCK and, before a change, the next one. The
//! stations get them in clear over the TLS control channel (`CckKeys`): a CCK is sealed for a
//! radio with that radio's DCK (TA31, clause 4.5.1.1), which only the station knows, so the
//! station hands the next key to its class 3 radios itself (D-OTAR CCK PROVIDE, key type
//! "future") and reports each answer (`CckResult`). The change runs for a whole LA at one time
//! (clause 4.5.5.2, `CckActivate`); during the overlap the station still recognises radios that
//! run the old CCK on the uplink.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::control::ControlCommand;
use crate::fleet_keys::{hex, random, KeyStore};
use crate::fleet_sck::{check_value, net_name, parse_sck};
use crate::state::AppState;

/// Station capability: understands `CckKeys` / `CckActivate`.
pub const CAP_CCK: &str = "cck-v1";

/// After the activation time the next key becomes the current one (ms).
const PROMOTE_AFTER_MS: u64 = 120_000;
const MAX_LEAD_SECS: u64 = 7 * 86_400;
/// Scope key for a CCK that applies to every LA of the network.
pub const ALL: &str = "all";

// ───────────────────────────── wire ─────────────────────────────

/// One CCK as it goes to a station: location area (`None`: every LA), CCK-id, value in hex.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CckWire {
    #[serde(default)]
    pub la: Option<u16>,
    pub cck_id: u16,
    pub key: String,
    #[serde(default)]
    pub next: bool,
}

/// A CCK named by id and check value, as the stations report it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CckRef {
    pub cck_id: u16,
    pub check: String,
}

// ───────────────────────────── store ─────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CckEntry {
    pub cck_id: u16,
    /// Hex, only inside the encrypted store.
    key: String,
    pub check: String,
    /// `generated` or `entered`.
    pub source: String,
    pub by: Option<String>,
    pub set_ms: u64,
}

impl CckEntry {
    fn wire(&self, la: Option<u16>, next: bool) -> CckWire {
        CckWire { la, cck_id: self.cck_id, key: self.key.clone(), next }
    }

    fn same_as(&self, r: &CckRef) -> bool {
        self.cck_id == r.cck_id && self.check == r.check
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CckActivation {
    pub cck_id: u16,
    pub at_ms: u64,
    pub overlap_s: u32,
    pub by: Option<String>,
}

/// The CCKs of one scope (one LA, or every LA).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CckScope {
    pub current: Option<CckEntry>,
    pub next: Option<CckEntry>,
    pub activation: Option<CckActivation>,
    /// CCK-ids each radio accepted (U-OTAR CCK RESULT reported by its station).
    #[serde(default)]
    pub confirmed: BTreeMap<u32, Vec<u16>>,
}

fn default_overlap() -> u32 {
    30
}

/// Everything of phase 8c that goes into the encrypted key store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CckData {
    /// Network (MCC-MNC) → scope (`all` or the LA number) → keys.
    #[serde(default)]
    pub nets: BTreeMap<String, BTreeMap<String, CckScope>>,
    /// Minutes the stations still recognise the old CCK on the uplink after a change.
    #[serde(default = "default_overlap")]
    pub overlap_min: u32,
}

impl Default for CckData {
    fn default() -> Self {
        Self { nets: BTreeMap::new(), overlap_min: default_overlap() }
    }
}

/// What a station reports (not stored).
#[derive(Debug, Clone, Default, Serialize)]
pub struct StationCck {
    pub net: String,
    pub la: u16,
    pub active: Option<CckRef>,
    pub from_config: bool,
    pub keys: Vec<CckRef>,
    pub switch_at_ms: Option<u64>,
}

#[derive(Debug, Default)]
pub struct CckRuntime {
    pub stations: HashMap<String, StationCck>,
}

fn scope_la(scope: &str) -> Option<u16> {
    scope.parse().ok()
}

fn check_scope(scope: &str) -> Result<(), String> {
    if scope == ALL || scope.parse::<u16>().is_ok_and(|la| la <= 0x3fff) {
        Ok(())
    } else {
        Err(format!("location area {scope:?}: use 0-16383 or \"all\""))
    }
}

/// The scope that holds the keys of location area `la` in `net`: its own entry, else `all`.
pub fn scope_of(ks: &KeyStore, net: &str, la: u16) -> Option<String> {
    let n = ks.cck().nets.get(net)?;
    let own = la.to_string();
    if n.contains_key(&own) {
        Some(own)
    } else if n.contains_key(ALL) {
        Some(ALL.into())
    } else {
        None
    }
}

/// Set the current or next CCK of a scope; `key` `None`: a random one (returned once).
/// `cck_id` `None`: the next key gets the current id + 1 (another least significant bit, so
/// the radios see the change in the encryption mode element, EN 300 392-7 clause 6.5.1 note 2).
#[allow(clippy::too_many_arguments)]
pub fn set_key(ks: &mut KeyStore, net: &str, scope: &str, next: bool, cck_id: Option<u16>, key: Option<[u8; 10]>, by: Option<&str>, now_ms: u64) -> Result<(String, u16, Option<[u8; 10]>), String> {
    ks.ready()?;
    crate::fleet_sck::check_net(net)?;
    check_scope(scope)?;
    {
        let n = ks.cck().nets.get(net);
        let other = if scope == ALL { n.is_some_and(|n| n.keys().any(|k| k != ALL)) } else { n.is_some_and(|n| n.contains_key(ALL)) };
        if other {
            return Err("use either one CCK for all location areas or one per location area, not both".into());
        }
    }
    let generated = key.is_none();
    let key = key.unwrap_or_else(random::<10>);
    let s = ks.cck_mut().nets.entry(net.to_owned()).or_default().entry(scope.to_owned()).or_default();
    if s.activation.is_some() {
        return Err("a key change is planned; cancel it first".into());
    }
    let id = if next {
        let cur = s.current.as_ref().ok_or("set the current CCK first")?;
        let id = cck_id.unwrap_or(cur.cck_id.wrapping_add(1));
        if (id ^ cur.cck_id) & 1 == 0 {
            // The radios find the active CCK by this bit (clause 6.5.1).
            return Err(format!("the next CCK-id needs another last bit than the current one ({})", cur.cck_id));
        }
        id
    } else {
        cck_id.ok_or("the current CCK needs its CCK-id")?
    };
    let entry = CckEntry {
        cck_id: id,
        key: hex(&key),
        check: check_value(&key),
        source: if generated { "generated" } else { "entered" }.into(),
        by: by.map(str::to_owned),
        set_ms: now_ms,
    };
    if let Some(old) = if next { s.next.replace(entry.clone()) } else { s.current.replace(entry.clone()) } {
        s.confirmed.values_mut().for_each(|v| v.retain(|k| *k != old.cck_id));
    }
    s.confirmed.values_mut().for_each(|v| v.retain(|k| *k != id));
    s.confirmed.retain(|_, v| !v.is_empty());
    ks.save();
    Ok((entry.check, id, generated.then_some(key)))
}

/// Remove the next key, or the current one while no station runs it and there is no next one.
pub fn delete_key(ks: &mut KeyStore, net: &str, scope: &str, current: bool) -> Result<bool, String> {
    ks.ready()?;
    let in_use = current_in_use(ks, net, scope);
    let Some(n) = ks.cck_mut().nets.get_mut(net) else { return Ok(false) };
    let Some(s) = n.get_mut(scope) else { return Ok(false) };
    if s.activation.is_some() {
        return Err("a key change is planned; cancel it first".into());
    }
    let removed = if current {
        if s.next.is_some() {
            return Err("remove the next key first".into());
        }
        if in_use {
            return Err("stations run this key; it can only be replaced by a key change".into());
        }
        s.current.take()
    } else {
        s.next.take()
    };
    let Some(old) = removed else { return Ok(false) };
    s.confirmed.values_mut().for_each(|v| v.retain(|k| *k != old.cck_id));
    s.confirmed.retain(|_, v| !v.is_empty());
    if s.current.is_none() && s.next.is_none() {
        n.remove(scope);
    }
    if n.is_empty() {
        ks.cck_mut().nets.remove(net);
    }
    ks.save();
    Ok(true)
}

/// Stations in `scope` of `net` (as they last reported).
fn scope_stations<'a>(ks: &'a KeyStore, net: &'a str, scope: &'a str) -> impl Iterator<Item = (&'a String, &'a StationCck)> + 'a {
    ks.cck_rt.stations.iter().filter(move |(_, st)| st.net == net && (scope == ALL || scope_la(scope) == Some(st.la)))
}

/// A station in the scope runs its current key.
pub fn current_in_use(ks: &KeyStore, net: &str, scope: &str) -> bool {
    let Some(cur) = ks.cck().nets.get(net).and_then(|n| n.get(scope)).and_then(|s| s.current.as_ref()) else { return false };
    scope_stations(ks, net, scope).any(|(_, st)| st.active.as_ref().is_some_and(|a| cur.same_as(a)))
}

/// A radio's answer for one CCK, reported by the station of location area `la`.
pub fn note_result(ks: &mut KeyStore, net: &str, la: u16, issi: u32, cck_id: u16, ok: bool) {
    let Some(scope) = scope_of(ks, net, la) else { return };
    let Some(s) = ks.cck_mut().nets.get_mut(net).and_then(|n| n.get_mut(&scope)) else { return };
    if ![&s.current, &s.next].into_iter().flatten().any(|e| e.cck_id == cck_id) {
        return;
    }
    let list = s.confirmed.entry(issi).or_default();
    let had = list.contains(&cck_id);
    if ok == had {
        return;
    }
    if ok {
        list.push(cck_id);
    } else {
        list.retain(|k| *k != cck_id);
    }
    s.confirmed.retain(|_, v| !v.is_empty());
    ks.save();
}

/// Radios with K seen at a station of the scope.
fn members(ks: &KeyStore, net: &str, scope: &str, logins: &HashMap<String, String>) -> Vec<u32> {
    let stations: Vec<&String> = scope_stations(ks, net, scope).filter_map(|(id, _)| logins.get(id)).collect();
    ks.issis(net).into_iter().filter(|i| ks.seen_of(*i).is_some_and(|(st, _)| stations.iter().any(|l| *l == st))).collect()
}

/// (radios that hold `cck_id`, radios of the scope).
pub fn coverage(ks: &KeyStore, net: &str, scope: &str, cck_id: u16, logins: &HashMap<String, String>) -> (usize, usize) {
    let m = members(ks, net, scope, logins);
    let confirmed = ks.cck().nets.get(net).and_then(|n| n.get(scope)).map(|s| &s.confirmed);
    let have = m.iter().filter(|i| confirmed.and_then(|c| c.get(i)).is_some_and(|v| v.contains(&cck_id))).count();
    (have, m.len())
}

/// Plan the change of a scope to its next key (`next`) or to its current one (stations still
/// on their own key). Refused below the warning level unless `force`.
#[allow(clippy::too_many_arguments)]
pub fn activate(ks: &mut KeyStore, net: &str, scope: &str, next: bool, at_ms: u64, force: bool, logins: &HashMap<String, String>, by: Option<&str>) -> Result<CckActivation, String> {
    ks.ready()?;
    let warn = ks.sck().warn_pct as usize;
    let overlap_s = ks.cck().overlap_min.min(1440) * 60;
    let s = ks.cck().nets.get(net).and_then(|n| n.get(scope)).ok_or("no CCK for this location area")?;
    let e = if next { s.next.as_ref().ok_or("no next CCK")? } else { s.current.as_ref().ok_or("no current CCK")? };
    if s.activation.is_some() {
        return Err("a key change is already planned".into());
    }
    let id = e.cck_id;
    if next {
        let (have, total) = coverage(ks, net, scope, id, logins);
        if !force && total > 0 && have * 100 < warn * total {
            return Err(format!("only {have} of {total} radios hold CCK {id} (warning below {warn} %)"));
        }
    }
    let a = CckActivation { cck_id: id, at_ms, overlap_s, by: by.map(str::to_owned) };
    ks.cck_mut().nets.get_mut(net).and_then(|n| n.get_mut(scope)).expect("checked").activation = Some(a.clone());
    ks.save();
    Ok(a)
}

/// Drop a planned change. Refused once a station planned it: it has told its radios, and EN
/// 300 392-7 has no PDU that takes a D-CK CHANGE DEMAND back.
pub fn cancel(ks: &mut KeyStore, net: &str, scope: &str) -> Result<bool, String> {
    ks.ready()?;
    if scope_stations(ks, net, scope).any(|(_, st)| st.switch_at_ms.is_some()) {
        return Err("stations already announced the change to the radios (D-CK CHANGE DEMAND); it cannot be taken back".into());
    }
    let Some(s) = ks.cck_mut().nets.get_mut(net).and_then(|n| n.get_mut(scope)) else { return Ok(false) };
    let had = s.activation.take().is_some();
    if had {
        ks.save();
    }
    Ok(had)
}

/// After the activation time the next key becomes the current one.
pub fn promote(ks: &mut KeyStore, net: &str, scope: &str, now_ms: u64) -> bool {
    let Some(s) = ks.cck_mut().nets.get_mut(net).and_then(|n| n.get_mut(scope)) else { return false };
    let Some(a) = s.activation.clone() else { return false };
    if now_ms < a.at_ms + PROMOTE_AFTER_MS {
        return false;
    }
    if s.next.as_ref().is_some_and(|e| e.cck_id == a.cck_id) {
        let old = s.current.take().map(|e| e.cck_id);
        s.current = s.next.take();
        if let Some(old) = old {
            s.confirmed.values_mut().for_each(|v| v.retain(|k| *k != old));
            s.confirmed.retain(|_, v| !v.is_empty());
        }
    }
    s.activation = None;
    ks.save();
    true
}

/// `CckKeys` for the stations of `net`: every scope (the station takes its own LA's keys).
/// Also sent empty: the station drops old keys and answers with `CckState`, which tells the
/// server its location area and the CCK of its own config.
fn keys_command(ks: &KeyStore, net: &str) -> Option<ControlCommand> {
    let empty = std::collections::BTreeMap::new();
    let n = ks.cck().nets.get(net).unwrap_or(&empty);
    let keys: Vec<CckWire> = n
        .iter()
        .flat_map(|(scope, s)| {
            let la = scope_la(scope);
            s.current.iter().map(move |e| e.wire(la, false)).chain(s.next.iter().map(move |e| e.wire(la, true)))
        })
        .collect();
    Some(ControlCommand::CckKeys { keys })
}

fn activate_commands(ks: &KeyStore, net: &str) -> Vec<ControlCommand> {
    let Some(n) = ks.cck().nets.get(net) else { return Vec::new() };
    n.iter()
        .filter_map(|(scope, s)| s.activation.as_ref().map(|a| ControlCommand::CckActivate { la: scope_la(scope), cck_id: a.cck_id, at_ms: a.at_ms, overlap_s: a.overlap_s }))
        .collect()
}

// ───────────────────────────── stations ─────────────────────────────

/// Online stations with server keys and `cck-v1`: id → network.
fn cck_stations(state: &AppState) -> HashMap<String, String> {
    let fleet = state.fleet.lock().unwrap_or_else(|e| e.into_inner());
    fleet
        .stations()
        .into_iter()
        .filter(|s| s.online && s.key_source.as_deref() == Some("server") && s.capabilities.iter().any(|c| c == CAP_CCK))
        .map(|s| (s.id.clone(), net_name(s.mcc, s.mnc)))
        .collect()
}

/// Station id → Brew login.
fn logins(state: &AppState) -> HashMap<String, String> {
    let fleet = state.fleet.lock().unwrap_or_else(|e| e.into_inner());
    fleet.stations().into_iter().filter_map(|s| s.brew_login.clone().map(|l| (s.id.clone(), l))).collect()
}

fn keystore(state: &AppState) -> std::sync::MutexGuard<'_, KeyStore> {
    state.fleet_keys.lock().unwrap_or_else(|e| e.into_inner())
}

/// Send the key set and planned changes to every station of `net` that uses them.
pub fn push_net(state: &Arc<AppState>, net: &str) {
    let stations = cck_stations(state);
    let (cmd, acts) = {
        let ks = keystore(state);
        (keys_command(&ks, net), activate_commands(&ks, net))
    };
    for (id, _) in stations.iter().filter(|(_, n)| *n == net) {
        if let Some(c) = &cmd {
            crate::fleet_keys::send(state, id, c.clone());
        }
        for a in &acts {
            crate::fleet_keys::send(state, id, a.clone());
        }
    }
}

/// After a station's hello: its network's CCKs, and a planned change.
pub fn after_hello(state: &Arc<AppState>, id: &str) {
    let stations = cck_stations(state);
    let Some(net) = stations.get(id) else {
        keystore(state).cck_rt.stations.remove(id);
        return;
    };
    let (cmd, acts) = {
        let ks = keystore(state);
        (keys_command(&ks, net), activate_commands(&ks, net))
    };
    if let Some(c) = cmd {
        crate::fleet_keys::send(state, id, c);
    }
    for a in acts {
        crate::fleet_keys::send(state, id, a);
    }
}

/// A station reports its CCK. A station that ran a server key and missed a change (no change
/// planned on either side) is moved to the current key at once; a station on its own config key
/// stays until someone activates the current key for it.
#[allow(clippy::too_many_arguments)]
pub fn on_state(state: &Arc<AppState>, id: &str, la: u16, active: Option<CckRef>, from_config: bool, keys: Vec<CckRef>, switch_at_ms: Option<u64>) {
    let stations = cck_stations(state);
    let Some(net) = stations.get(id).cloned() else { return };
    let catch_up = {
        let mut ks = keystore(state);
        ks.cck_rt.stations.insert(id.to_owned(), StationCck { net: net.clone(), la, active: active.clone(), from_config, keys, switch_at_ms });
        let scope = scope_of(&ks, &net, la);
        let s = scope.as_ref().and_then(|sc| ks.cck().nets.get(&net).and_then(|n| n.get(sc)));
        match (s.and_then(|s| s.current.as_ref()), &active) {
            (Some(cur), Some(a)) if !from_config && switch_at_ms.is_none() && s.is_some_and(|s| s.activation.is_none()) && !cur.same_as(a) => {
                Some((scope.and_then(|sc| scope_la(&sc)), cur.cck_id, ks.cck().overlap_min.min(1440) * 60))
            }
            _ => None,
        }
    };
    if let Some((la, cck_id, overlap_s)) = catch_up {
        tracing::info!(station = %id, cck_id, "station missed a CCK change; switching it now");
        crate::fleet_keys::send(state, id, ControlCommand::CckActivate { la, cck_id, at_ms: crate::fleet::now_ms(), overlap_s });
    }
    state.monitor.emit("fleet_changed", serde_json::json!({"keys": true}));
}

pub fn on_result(state: &Arc<AppState>, id: &str, issi: u32, cck_id: u16, ok: bool, detail: Option<String>) {
    let mut ks = keystore(state);
    let Some(st) = ks.cck_rt.stations.get(id).cloned() else { return };
    note_result(&mut ks, &st.net, st.la, issi, cck_id, ok);
    drop(ks);
    if !ok {
        crate::fleet_keys::log(state, None, format!("ISSI {issi} did not take CCK {cck_id}: {}", detail.unwrap_or_default()));
    }
    state.monitor.emit("fleet_changed", serde_json::json!({"keys": true}));
}

/// Every few seconds: finished changes.
pub fn tick(state: &Arc<AppState>) {
    let now = crate::fleet::now_ms();
    let done: Vec<(String, String)> = {
        let mut ks = keystore(state);
        let all: Vec<(String, String)> = ks.cck().nets.iter().flat_map(|(n, m)| m.keys().map(move |s| (n.clone(), s.clone()))).collect();
        all.into_iter().filter(|(n, s)| promote(&mut ks, n, s, now)).collect()
    };
    let mut nets: Vec<String> = Vec::new();
    for (net, scope) in done {
        crate::fleet_keys::log(state, None, format!("network {net}, LA {scope}: CCK change done, the next key is the current one"));
        if !nets.contains(&net) {
            nets.push(net);
        }
    }
    for net in nets {
        push_net(state, &net);
    }
}

/// For `GET /api/fleet/keys`.
pub fn view(ks: &KeyStore, logins: &HashMap<String, String>) -> serde_json::Value {
    let entry = |e: &CckEntry| serde_json::json!({"cck_id": e.cck_id, "check": e.check, "source": e.source, "by": e.by, "set_ms": e.set_ms});
    let nets: serde_json::Map<String, serde_json::Value> = ks
        .cck()
        .nets
        .iter()
        .map(|(net, scopes)| {
            let v: serde_json::Map<String, serde_json::Value> = scopes
                .iter()
                .map(|(scope, s)| {
                    let cov = |e: &Option<CckEntry>| e.as_ref().map(|e| coverage(ks, net, scope, e.cck_id, logins));
                    let stations: Vec<serde_json::Value> = scope_stations(ks, net, scope)
                        .map(|(id, st)| serde_json::json!({"id": id, "la": st.la, "active": st.active, "from_config": st.from_config, "switch_at_ms": st.switch_at_ms,
                            "matches_current": st.active.as_ref().zip(s.current.as_ref()).is_some_and(|(a, c)| c.same_as(a))}))
                        .collect();
                    (
                        scope.clone(),
                        serde_json::json!({
                            "current": s.current.as_ref().map(entry), "next": s.next.as_ref().map(entry), "activation": s.activation,
                            "coverage_current": cov(&s.current), "coverage_next": cov(&s.next), "current_in_use": current_in_use(ks, net, scope),
                            "stations": stations, "confirmed": s.confirmed,
                        }),
                    )
                })
                .collect();
            (net.clone(), serde_json::Value::Object(v))
        })
        .collect();
    let unassigned: Vec<serde_json::Value> = ks
        .cck_rt
        .stations
        .iter()
        .filter(|(_, st)| st.active.is_some() && scope_of(ks, &st.net, st.la).is_none())
        .map(|(id, st)| serde_json::json!({"id": id, "net": st.net, "la": st.la, "active": st.active, "from_config": st.from_config}))
        .collect();
    serde_json::json!({"overlap_min": ks.cck().overlap_min, "nets": nets, "unassigned": unassigned})
}

// ───────────────────────────── dashboard API ─────────────────────────────

use axum::{extract::State, http::{HeaderMap, StatusCode}, response::{IntoResponse, Response}, Json};

#[derive(Deserialize)]
pub struct SetReq {
    net: String,
    /// LA number or `all`.
    scope: String,
    /// `current` or `next`.
    role: String,
    #[serde(default)]
    cck_id: Option<u16>,
    /// 20 hex digits; empty: generate.
    #[serde(default)]
    key: String,
}

/// `POST /api/fleet/keys/cck/set` (Fleet Control keys): enter or generate a CCK.
pub async fn api_set(State(state): State<Arc<AppState>>, headers: HeaderMap, Json(req): Json<SetReq>) -> Response {
    let a = crate::dashboard::access(&state, &headers);
    if !a.manages_keys() {
        return crate::fleet_keys::forbidden();
    }
    if let Some(r) = crate::fleet_keys::net_refused(&state, &a, &req.net) {
        return r;
    }
    let next = match req.role.as_str() {
        "next" => true,
        "current" => false,
        _ => return crate::fleet_keys::bad("role: current or next"),
    };
    let key = if req.key.trim().is_empty() {
        None
    } else {
        match parse_sck(&req.key) {
            Ok(k) => Some(k),
            Err(e) => return crate::fleet_keys::bad(e),
        }
    };
    let res = set_key(&mut keystore(&state), &req.net, &req.scope, next, req.cck_id, key, a.user.as_deref(), crate::fleet::now_ms());
    match res {
        Ok((check, id, generated)) => {
            crate::fleet_keys::log(
                &state,
                a.user.as_deref(),
                format!("network {}, LA {}: {} CCK {id} {} (check value {check})", req.net, req.scope, req.role, if generated.is_some() { "generated" } else { "entered" }),
            );
            push_net(&state, &req.net);
            let grouped = generated.map(|k| {
                let h = hex(&k).to_uppercase();
                h.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).unwrap_or("")).collect::<Vec<_>>().join(" ")
            });
            let mut r = Json(serde_json::json!({"ok": true, "check": check, "cck_id": id, "key_once": grouped})).into_response();
            r.headers_mut().insert(axum::http::header::CACHE_CONTROL, axum::http::HeaderValue::from_static("no-store"));
            r
        }
        Err(e) => crate::fleet_keys::bad(e),
    }
}

#[derive(Deserialize)]
pub struct ScopeReq {
    net: String,
    scope: String,
    /// For `delete`: `next` (default) or `current`.
    #[serde(default)]
    role: Option<String>,
}

/// `POST /api/fleet/keys/cck/delete`: remove the next CCK, or the current one no station runs.
pub async fn api_delete(State(state): State<Arc<AppState>>, headers: HeaderMap, Json(req): Json<ScopeReq>) -> Response {
    let a = crate::dashboard::access(&state, &headers);
    if !a.manages_keys() {
        return crate::fleet_keys::forbidden();
    }
    if let Some(r) = crate::fleet_keys::net_refused(&state, &a, &req.net) {
        return r;
    }
    let current = req.role.as_deref() == Some("current");
    match delete_key(&mut keystore(&state), &req.net, &req.scope, current) {
        Ok(had) => {
            if had {
                crate::fleet_keys::log(&state, a.user.as_deref(), format!("network {}, LA {}: {} CCK removed", req.net, req.scope, if current { "current" } else { "next" }));
                push_net(&state, &req.net);
            }
            Json(serde_json::json!({"ok": true, "removed": had})).into_response()
        }
        Err(e) => crate::fleet_keys::bad(e),
    }
}

#[derive(Deserialize)]
pub struct ActivateReq {
    net: String,
    scope: String,
    #[serde(default = "role_next")]
    role: String,
    #[serde(default = "lead_default")]
    in_secs: u64,
    #[serde(default)]
    force: bool,
}

fn role_next() -> String {
    "next".into()
}

fn lead_default() -> u64 {
    300
}

/// `POST /api/fleet/keys/cck/activate` (Fleet Control keys and management).
pub async fn api_activate(State(state): State<Arc<AppState>>, headers: HeaderMap, Json(req): Json<ActivateReq>) -> Response {
    let a = crate::dashboard::access(&state, &headers);
    if !a.manages_keys() || !a.manages_fleet() {
        return (StatusCode::FORBIDDEN, "This needs the rights Fleet Control keys and Fleet Control management").into_response();
    }
    if let Some(r) = crate::fleet_keys::net_refused(&state, &a, &req.net) {
        return r;
    }
    if req.in_secs > MAX_LEAD_SECS {
        return crate::fleet_keys::bad("at most 7 days ahead");
    }
    let logins = logins(&state);
    let at_ms = crate::fleet::now_ms() + req.in_secs * 1000;
    let res = activate(&mut keystore(&state), &req.net, &req.scope, req.role != "current", at_ms, req.force, &logins, a.user.as_deref());
    match res {
        Ok(act) => {
            crate::fleet_keys::log(
                &state,
                a.user.as_deref(),
                format!("network {}, LA {}: change to CCK {} planned in {} s, overlap {} min{}", req.net, req.scope, act.cck_id, req.in_secs, act.overlap_s / 60, if req.force { " (below the warning level)" } else { "" }),
            );
            push_net(&state, &req.net);
            Json(serde_json::json!({"ok": true, "at_ms": act.at_ms})).into_response()
        }
        Err(e) => (StatusCode::CONFLICT, e).into_response(),
    }
}

/// `POST /api/fleet/keys/cck/cancel`.
pub async fn api_cancel(State(state): State<Arc<AppState>>, headers: HeaderMap, Json(req): Json<ScopeReq>) -> Response {
    let a = crate::dashboard::access(&state, &headers);
    if !a.manages_keys() || !a.manages_fleet() {
        return (StatusCode::FORBIDDEN, "This needs the rights Fleet Control keys and Fleet Control management").into_response();
    }
    if let Some(r) = crate::fleet_keys::net_refused(&state, &a, &req.net) {
        return r;
    }
    match cancel(&mut keystore(&state), &req.net, &req.scope) {
        Ok(had) => {
            if had {
                crate::fleet_keys::log(&state, a.user.as_deref(), format!("network {}, LA {}: planned CCK change cancelled", req.net, req.scope));
                push_net(&state, &req.net);
            }
            Json(serde_json::json!({"ok": true, "cancelled": had})).into_response()
        }
        Err(e) => crate::fleet_keys::bad(e),
    }
}

#[derive(Deserialize)]
pub struct SettingsReq {
    overlap_min: u32,
}

/// `POST /api/fleet/keys/cck/settings` (admins): overlap.
pub async fn api_settings(State(state): State<Arc<AppState>>, headers: HeaderMap, Json(req): Json<SettingsReq>) -> Response {
    let a = crate::dashboard::access(&state, &headers);
    if !a.admin {
        return (StatusCode::FORBIDDEN, "Admins only").into_response();
    }
    if req.overlap_min > 1440 {
        return crate::fleet_keys::bad("overlap 0-1440 min");
    }
    {
        let mut ks = keystore(&state);
        if let Err(e) = ks.ready() {
            return crate::fleet_keys::bad(e);
        }
        ks.cck_mut().overlap_min = req.overlap_min;
        ks.save();
    }
    crate::fleet_keys::log(&state, a.user.as_deref(), format!("CCK overlap set to {} min", req.overlap_min));
    Json(serde_json::json!({"ok": true})).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    const K: [u8; 16] = [0x57; 16];

    fn store_with_radios(issis: &[u32]) -> KeyStore {
        let mut ks = KeyStore::memory();
        for &i in issis {
            ks.set_k("232-1", i, &K, "manual", None, 0).unwrap();
            ks.note_seen(i, "1000001", 1);
        }
        ks
    }

    fn station(ks: &mut KeyStore, id: &str, la: u16, active: Option<CckRef>, from_config: bool) {
        ks.cck_rt.stations.insert(id.into(), StationCck { net: "232-1".into(), la, active, from_config, keys: vec![], switch_at_ms: None });
    }

    fn logins() -> HashMap<String, String> {
        HashMap::from([("bs1".to_string(), "1000001".to_string())])
    }

    #[test]
    fn the_next_cck_id_flips_the_last_bit_and_scopes_do_not_mix() {
        let mut ks = store_with_radios(&[]);
        assert!(set_key(&mut ks, "232-1", "2", true, None, None, None, 0).unwrap_err().contains("current CCK first"));
        assert!(set_key(&mut ks, "232-1", "2", false, None, None, None, 0).unwrap_err().contains("CCK-id"));
        set_key(&mut ks, "232-1", "2", false, Some(42), Some([0x11; 10]), None, 0).unwrap();
        assert!(set_key(&mut ks, "232-1", "2", true, Some(44), None, None, 0).unwrap_err().contains("last bit"), "6.5.1: the radio must see the change");
        let (_, id, shown) = set_key(&mut ks, "232-1", "2", true, None, None, None, 0).unwrap();
        assert_eq!(id, 43);
        assert!(shown.is_some(), "a generated key is shown once");
        assert!(set_key(&mut ks, "232-1", ALL, false, Some(1), None, None, 0).unwrap_err().contains("either"));
        assert!(set_key(&mut ks, "232-1", "16384", false, Some(1), None, None, 0).is_err());
    }

    #[test]
    fn keys_go_to_the_stations_per_location_area() {
        let mut ks = store_with_radios(&[]);
        set_key(&mut ks, "232-1", "2", false, Some(42), Some([0x11; 10]), None, 0).unwrap();
        set_key(&mut ks, "232-1", "2", true, None, Some([0x22; 10]), None, 0).unwrap();
        set_key(&mut ks, "232-1", "5", false, Some(7), Some([0x55; 10]), None, 0).unwrap();
        let Some(ControlCommand::CckKeys { keys }) = keys_command(&ks, "232-1") else { panic!() };
        assert_eq!(keys.iter().map(|k| (k.la, k.cck_id, k.next)).collect::<Vec<_>>(), vec![(Some(2), 42, false), (Some(2), 43, true), (Some(5), 7, false)]);
        assert_eq!(keys[0].key, hex(&[0x11; 10]));
        assert_eq!(scope_of(&ks, "232-1", 5).as_deref(), Some("5"));
        assert_eq!(scope_of(&ks, "232-1", 9), None);
    }

    #[test]
    fn coverage_and_warning_follow_the_radios_results_and_the_change_promotes_next() {
        let radios: Vec<u32> = (100..110).collect();
        let mut ks = store_with_radios(&radios);
        station(&mut ks, "bs1", 2, None, true);
        set_key(&mut ks, "232-1", "2", false, Some(42), Some([0x11; 10]), None, 0).unwrap();
        set_key(&mut ks, "232-1", "2", true, None, Some([0x22; 10]), None, 0).unwrap();
        for &i in &radios[..8] {
            note_result(&mut ks, "232-1", 2, i, 43, true);
        }
        note_result(&mut ks, "232-1", 2, 100, 99, true); // not a key of this LA
        assert_eq!(coverage(&ks, "232-1", "2", 43, &logins()), (8, 10));
        assert!(activate(&mut ks, "232-1", "2", true, 1000, false, &logins(), None).unwrap_err().contains("only 8 of 10"));
        note_result(&mut ks, "232-1", 2, 108, 43, true);
        let a = activate(&mut ks, "232-1", "2", true, 1000, false, &logins(), Some("seb")).unwrap();
        assert_eq!((a.cck_id, a.overlap_s), (43, 30 * 60));
        assert!(!promote(&mut ks, "232-1", "2", 1000 + PROMOTE_AFTER_MS - 1));
        assert!(promote(&mut ks, "232-1", "2", 1000 + PROMOTE_AFTER_MS));
        let s = &ks.cck().nets["232-1"]["2"];
        assert_eq!(s.current.as_ref().map(|e| e.cck_id), Some(43));
        assert!(s.next.is_none() && s.activation.is_none());
    }

    #[test]
    fn an_unused_current_cck_can_be_removed_one_in_use_cannot() {
        let mut ks = store_with_radios(&[]);
        set_key(&mut ks, "232-1", "2", false, Some(42), Some([0x11; 10]), None, 0).unwrap();
        station(&mut ks, "bs1", 2, Some(CckRef { cck_id: 42, check: check_value(&[0x99; 10]) }), true);
        assert!(!current_in_use(&ks, "232-1", "2"), "same id, another value");
        assert!(delete_key(&mut ks, "232-1", "2", true).unwrap());
        assert!(!ks.cck().nets.contains_key("232-1"));
        set_key(&mut ks, "232-1", "2", false, Some(42), Some([0x99; 10]), None, 0).unwrap();
        assert!(current_in_use(&ks, "232-1", "2"));
        assert!(delete_key(&mut ks, "232-1", "2", true).unwrap_err().contains("stations run"));
    }

    #[test]
    fn a_change_a_station_already_announced_cannot_be_cancelled() {
        let mut ks = store_with_radios(&[]);
        set_key(&mut ks, "232-1", "2", false, Some(42), Some([0x11; 10]), None, 0).unwrap();
        set_key(&mut ks, "232-1", "2", true, None, Some([0x22; 10]), None, 0).unwrap();
        activate(&mut ks, "232-1", "2", true, 5000, true, &logins(), None).unwrap();
        ks.cck_rt.stations.insert("bs1".into(), StationCck { net: "232-1".into(), la: 2, active: None, from_config: true, keys: vec![], switch_at_ms: Some(5000) });
        assert!(cancel(&mut ks, "232-1", "2").unwrap_err().contains("cannot be taken back"));
        ks.cck_rt.stations.clear();
        assert!(cancel(&mut ks, "232-1", "2").unwrap(), "no station has it yet");
    }

    #[test]
    fn the_keys_survive_a_restart_inside_the_encrypted_store() {
        let dir = std::env::temp_dir().join(format!("brew-cck-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let storage = dir.join("history");
        let mut ks = KeyStore::load(Some(&storage), None);
        set_key(&mut ks, "232-1", ALL, false, Some(42), Some([0x11; 10]), None, 0).unwrap();
        ks.cck_mut().overlap_min = 45;
        ks.save();
        let raw = std::fs::read_to_string(dir.join("history.fleet-keys")).unwrap();
        assert!(!raw.contains(&hex(&[0x11; 10])), "the key file is encrypted");
        let ks2 = KeyStore::load(Some(&storage), None);
        assert_eq!(ks2.cck().nets["232-1"][ALL].current.as_ref().map(|e| e.cck_id), Some(42));
        assert_eq!(ks2.cck().overlap_min, 45);
        std::fs::remove_dir_all(&dir).ok();
    }
}
