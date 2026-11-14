//! Fleet Control key management, phase 8b: the static cipher keys (SCK) of security class 2
//! networks, kept by the server and delivered to the radios through the stations.
//!
//! Per network (MCC-MNC) the server holds the current SCK and, before a change, the next one
//! (number 1..=32 and version, EN 300 392-7 clause 4.2.4.0 and A.8.75). The stations get both
//! keys in clear over the TLS control channel, because they encrypt the air interface with
//! them. A radio gets them sealed under its own K (clause 4.5.2.2): the server picks RSO,
//! KSO = TA41(K, RSO), SSCK = TA51(SCK, SCK-VN, KSO, SCKN), and the station only forwards
//! D-OTAR SCK PROVIDE. Every radio's U-OTAR SCK RESULT comes back, so the server knows how many
//! radios hold the next key before it is switched on. The change runs network wide at one time
//! (clause 4.5.5.4): each station announces it with D-CK CHANGE DEMAND and switches on that slot.
//!
//! Device profiles say how many trunked mode SCKs a radio model holds; a radio with room for one
//! only gets the current key.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::control::ControlCommand;
use crate::fleet_keys::{hex, random, unhex_n, KeyStore};
use crate::state::AppState;

/// Station capability: understands `SckKeys` / `SckSealed` / `SckActivate`.
pub const CAP_SCK: &str = "sck-v1";

/// A radio that misses a key is sent it again at most this often (ms).
const RETRY_MS: u64 = 600_000;
/// After the activation time the next key becomes the current one (ms).
const PROMOTE_AFTER_MS: u64 = 120_000;
/// Activation lead time limits (s).
const MAX_LEAD_SECS: u64 = 7 * 86_400;

// ───────────────────────────── wire ─────────────────────────────

/// One SCK as it goes to a station: value in hex, `next` while it is not in use yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SckWire {
    pub sckn: u8,
    pub vn: u16,
    pub key: String,
    #[serde(default)]
    pub next: bool,
}

/// One SCK sealed for one radio (SSCK, 120 bits, hex).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedSckWire {
    pub sckn: u8,
    pub vn: u16,
    pub ssck: String,
}

/// An SCK named by a station without its value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SckRef {
    pub sckn: u8,
    pub vn: u16,
    pub check: String,
}

// ───────────────────────────── data ─────────────────────────────

/// SHA-256(SCK), first 8 bytes, upper case hex (as for K).
pub fn check_value(key: &[u8; 10]) -> String {
    let d = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, key);
    hex(&d.as_ref()[..8]).to_uppercase()
}

/// 20 hex digits; spaces, `:` and `-` are dropped.
pub fn parse_sck(s: &str) -> Result<[u8; 10], String> {
    let digits: String = s.chars().filter(|c| !matches!(c, ' ' | ':' | '-' | '\t')).collect();
    if digits.len() != 20 {
        return Err(format!("an SCK has 20 hex digits (80 bit), got {}", digits.len()));
    }
    unhex_n::<10>(&digits.to_ascii_lowercase()).ok_or_else(|| "the SCK may only contain hex digits 0-9 and a-f".to_string())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SckEntry {
    pub sckn: u8,
    pub vn: u16,
    /// Hex, only inside the encrypted store.
    key: String,
    pub check: String,
    /// `generated` or `entered`.
    pub source: String,
    pub by: Option<String>,
    pub set_ms: u64,
}

impl SckEntry {
    fn key(&self) -> Option<[u8; 10]> {
        unhex_n::<10>(&self.key)
    }

    fn id(&self) -> (u8, u16) {
        (self.sckn, self.vn)
    }

    fn wire(&self, next: bool) -> SckWire {
        SckWire { sckn: self.sckn, vn: self.vn, key: self.key.clone(), next }
    }

    fn same_as(&self, r: &SckRef) -> bool {
        self.sckn == r.sckn && self.vn == r.vn && self.check == r.check
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Activation {
    pub sckn: u8,
    pub vn: u16,
    pub at_ms: u64,
    pub by: Option<String>,
}

/// The SCKs of one network.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SckNet {
    pub current: Option<SckEntry>,
    pub next: Option<SckEntry>,
    pub activation: Option<Activation>,
    /// Phase 8d: the fallback SCK of a class 3 network (SCKN 31 or 32, EN 300 392-7 clause
    /// 4.2.4.0 and Table A.96), run as class 2 by a station whose key server is unreachable.
    #[serde(default)]
    pub fallback: Option<SckEntry>,
    /// Keys each radio confirmed with U-OTAR SCK RESULT, as (SCKN, SCK-VN).
    #[serde(default)]
    pub confirmed: BTreeMap<u32, Vec<(u8, u16)>>,
}

/// How many keys a radio model holds (Server settings, editable).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    /// Trunked mode SCKs.
    pub tm_sck: u8,
    /// Direct mode SCKs (0: not known).
    #[serde(default)]
    pub dm_sck: u8,
    #[serde(default)]
    pub gck: u8,
    #[serde(default)]
    pub kag: u8,
}

/// The built-in profiles. MTP6000 series: "Motorola MTP6000 Series Security Specifications"
/// (2 TM-SCK, 16 GCK, 10 KAG), MTP6650 read from the radio; the others from the device overview,
/// not checked against manufacturer documents.
/// Built-in profiles: values from "Motorola MTP6000 Series Security Specifications"
/// (MTP6550/6650/6750: 2 trunked mode SCKs, 16 GCKs, 10 KAGs with 3 DM-SCKs each). The other
/// models of the list take the same values (Sebastian 2026-11-13: the Austrian BOS fleet swapped
/// them 1:1, same key storage). Admins change or add models in the server settings.
pub fn default_profiles() -> Vec<Profile> {
    ["MTP3550", "MTP6550", "MTP6650", "MTP6750", "MTP850 S", "MTH800"]
        .into_iter()
        .map(|name| Profile { name: name.into(), tm_sck: 2, dm_sck: 30, gck: 16, kag: 10 })
        .collect()
}

fn default_warn() -> u8 {
    90
}

fn default_fallback_min() -> u8 {
    5
}

fn default_reauth_per_s() -> u8 {
    2
}

/// Everything of phase 8b that goes into the encrypted key store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SckData {
    #[serde(default)]
    pub nets: BTreeMap<String, SckNet>,
    /// `None`: the built-in list.
    #[serde(default)]
    pub profiles: Option<Vec<Profile>>,
    /// Warn before a change when fewer radios than this hold the key (percent).
    #[serde(default = "default_warn")]
    pub warn_pct: u8,
    /// Phase 8d: minutes without the server before a station goes to fallback (1-60).
    #[serde(default = "default_fallback_min")]
    pub fallback_min: u8,
    /// Phase 8d: radios a station authenticates per second once the server is back (1-20).
    #[serde(default = "default_reauth_per_s")]
    pub reauth_per_s: u8,
}

impl Default for SckData {
    fn default() -> Self {
        Self { nets: BTreeMap::new(), profiles: None, warn_pct: default_warn(), fallback_min: default_fallback_min(), reauth_per_s: default_reauth_per_s() }
    }
}

impl SckData {
    pub fn profiles(&self) -> Vec<Profile> {
        self.profiles.clone().unwrap_or_else(default_profiles)
    }

    fn profile(&self, model: Option<&str>) -> Option<Profile> {
        let model = model?;
        self.profiles().into_iter().find(|p| p.name.eq_ignore_ascii_case(model))
    }
}

/// What the stations report (not stored).
#[derive(Debug, Clone, Default, Serialize)]
pub struct StationSck {
    pub active: Option<SckRef>,
    pub from_config: bool,
    pub keys: Vec<SckRef>,
    pub switch_at_ms: Option<u64>,
    pub at_ms: u64,
    /// The station's network (MCC-MNC) when it reported.
    pub net: String,
}

#[derive(Debug, Default)]
pub struct SckRuntime {
    pub stations: HashMap<String, StationSck>,
    /// Last delivery per radio (ms).
    attempts: HashMap<u32, u64>,
}

pub fn net_name(mcc: u16, mnc: u16) -> String {
    format!("{mcc}-{mnc}")
}

pub(crate) fn check_net(net: &str) -> Result<(), String> {
    let ok = net.split_once('-').is_some_and(|(a, b)| a.parse::<u16>().is_ok_and(|m| m <= 999) && b.parse::<u16>().is_ok_and(|m| m <= 16383));
    if ok { Ok(()) } else { Err(format!("network {net:?}: use MCC-MNC, e.g. 232-1")) }
}

// ───────────────────────────── store operations ─────────────────────────────

/// Set the current or next SCK of `net`; `key` `None`: a random one (returned once).
#[allow(clippy::too_many_arguments)]
pub fn set_key(ks: &mut KeyStore, net: &str, next: bool, sckn: u8, vn: u16, key: Option<[u8; 10]>, by: Option<&str>, now_ms: u64) -> Result<(String, Option<[u8; 10]>), String> {
    ks.ready()?;
    check_net(net)?;
    if !(1..=32).contains(&sckn) {
        return Err("SCK number must be 1-32".into());
    }
    let generated = key.is_none();
    let key = key.unwrap_or_else(random::<10>);
    let entry = SckEntry {
        sckn,
        vn,
        key: hex(&key),
        check: check_value(&key),
        source: if generated { "generated" } else { "entered" }.into(),
        by: by.map(str::to_owned),
        set_ms: now_ms,
    };
    let n = ks.sck_mut().nets.entry(net.to_owned()).or_default();
    if next {
        let cur = n.current.as_ref().ok_or("set the current SCK first")?;
        if cur.sckn == sckn {
            // The radios keep the keys by number: the same number would overwrite the key in use.
            return Err(format!("the next SCK needs another number than the current one ({})", cur.sckn));
        }
        if n.activation.is_some() {
            return Err("a key change is planned; cancel it first".into());
        }
        if let Some(old) = &n.next {
            let id = old.id();
            n.confirmed.values_mut().for_each(|v| v.retain(|k| *k != id));
        }
        n.next = Some(entry.clone());
    } else {
        if n.activation.is_some() {
            return Err("a key change is planned; cancel it first".into());
        }
        if n.next.as_ref().is_some_and(|x| x.sckn == sckn) {
            return Err("the next SCK already has this number".into());
        }
        if let Some(old) = &n.current {
            let id = old.id();
            n.confirmed.values_mut().for_each(|v| v.retain(|k| *k != id));
        }
        n.current = Some(entry.clone());
    }
    // A key with this number and version that radios confirmed before was another value.
    let id = entry.id();
    n.confirmed.values_mut().for_each(|v| v.retain(|k| *k != id));
    n.confirmed.retain(|_, v| !v.is_empty());
    ks.save();
    ks.sck_rt.attempts.clear();
    Ok((entry.check, generated.then_some(key)))
}

/// Remove the current SCK of `net` while no station runs it and there is no next key.
pub fn delete_current(ks: &mut KeyStore, net: &str) -> Result<bool, String> {
    ks.ready()?;
    let in_use = current_in_use(ks, net);
    let Some(n) = ks.sck_mut().nets.get_mut(net) else { return Ok(false) };
    if n.current.is_none() {
        return Ok(false);
    }
    if n.activation.is_some() {
        return Err("a key change is planned; cancel it first".into());
    }
    if n.next.is_some() {
        return Err("remove the next key first".into());
    }
    if in_use {
        return Err("stations run this key; it can only be replaced by a key change".into());
    }
    if n.fallback.is_some() {
        // The fallback key stays.
        let old = n.current.take().map(|e| e.id());
        if let Some(id) = old {
            n.confirmed.values_mut().for_each(|v| v.retain(|k| *k != id));
            n.confirmed.retain(|_, v| !v.is_empty());
        }
        ks.save();
        return Ok(true);
    }
    ks.sck_mut().nets.remove(net);
    ks.save();
    Ok(true)
}

pub fn delete_next(ks: &mut KeyStore, net: &str) -> Result<bool, String> {
    ks.ready()?;
    let Some(n) = ks.sck_mut().nets.get_mut(net) else { return Ok(false) };
    if n.activation.is_some() {
        return Err("a key change is planned; cancel it first".into());
    }
    let Some(old) = n.next.take() else { return Ok(false) };
    let id = old.id();
    n.confirmed.values_mut().for_each(|v| v.retain(|k| *k != id));
    n.confirmed.retain(|_, v| !v.is_empty());
    ks.save();
    Ok(true)
}

/// Phase 8d: set the fallback SCK of `net` (SCKN 31 or 32); `key` `None`: a random one
/// (returned once). Its number may not be the current or next SCK's: radios keep keys by number.
pub fn set_fallback(ks: &mut KeyStore, net: &str, sckn: u8, vn: u16, key: Option<[u8; 10]>, by: Option<&str>, now_ms: u64) -> Result<(String, Option<[u8; 10]>), String> {
    ks.ready()?;
    check_net(net)?;
    if !matches!(sckn, 31 | 32) {
        return Err("the fallback SCK has number 31 or 32 (EN 300 392-7 Table A.96)".into());
    }
    let generated = key.is_none();
    let key = key.unwrap_or_else(random::<10>);
    let entry = SckEntry {
        sckn,
        vn,
        key: hex(&key),
        check: check_value(&key),
        source: if generated { "generated" } else { "entered" }.into(),
        by: by.map(str::to_owned),
        set_ms: now_ms,
    };
    let n = ks.sck_mut().nets.entry(net.to_owned()).or_default();
    if [&n.current, &n.next].into_iter().flatten().any(|e| e.sckn == sckn) {
        return Err(format!("SCK {sckn} is already the current or next key of this network"));
    }
    if let Some(old) = &n.fallback {
        let id = old.id();
        n.confirmed.values_mut().for_each(|v| v.retain(|k| *k != id));
    }
    let id = entry.id();
    n.confirmed.values_mut().for_each(|v| v.retain(|k| *k != id));
    n.confirmed.retain(|_, v| !v.is_empty());
    n.fallback = Some(entry.clone());
    ks.save();
    ks.sck_rt.attempts.clear();
    Ok((entry.check, generated.then_some(key)))
}

pub fn delete_fallback(ks: &mut KeyStore, net: &str) -> Result<bool, String> {
    ks.ready()?;
    let Some(n) = ks.sck_mut().nets.get_mut(net) else { return Ok(false) };
    let Some(old) = n.fallback.take() else { return Ok(false) };
    let id = old.id();
    n.confirmed.values_mut().for_each(|v| v.retain(|k| *k != id));
    n.confirmed.retain(|_, v| !v.is_empty());
    let empty = n.current.is_none() && n.next.is_none() && n.confirmed.is_empty();
    if empty {
        ks.sck_mut().nets.remove(net);
    }
    ks.save();
    Ok(true)
}

/// The fallback SCK of `net`.
pub fn fallback_of(ks: &KeyStore, net: &str) -> Option<SckWire> {
    ks.sck().nets.get(net)?.fallback.as_ref().map(|e| e.wire(false))
}

/// True when a station of `net` reported that it runs the server's current key (from the server
/// or, with the same value, from its own config).
pub fn current_in_use(ks: &KeyStore, net: &str) -> bool {
    let Some(cur) = ks.sck().nets.get(net).and_then(|n| n.current.as_ref()) else { return false };
    ks.sck_rt.stations.values().any(|st| st.net == net && st.active.as_ref().is_some_and(|a| cur.same_as(a)))
}

/// The keys `issi` should hold: the current one, and the next one when its model has room.
/// Nothing while no station runs the current key: a wrong current key would otherwise overwrite
/// the key the radios use today (they keep keys by number).
///
/// Phase 8d: the fallback SCK (31/32) of a class 3 network goes too, when the model has room
/// for it besides the others; a network without a current SCK (class 3) needs no station
/// running one first.
fn deliverable(ks: &KeyStore, net: &str, issi: u32) -> Vec<SckEntry> {
    let Some(n) = ks.sck().nets.get(net) else { return Vec::new() };
    let in_use = current_in_use(ks, net);
    let room = ks.sck().profile(ks.model_of(issi)).map_or(u8::MAX, |p| p.tm_sck) as usize;
    let mut out: Vec<SckEntry> = Vec::new();
    if in_use {
        out.extend(n.current.iter().cloned());
        if let Some(next) = n.next.as_ref().filter(|_| room >= 2 || n.current.is_none()) {
            out.push(next.clone());
        }
    }
    if let Some(fb) = n.fallback.as_ref().filter(|_| (in_use || n.current.is_none()) && out.len() < room) {
        out.push(fb.clone());
    }
    out
}

/// Deliverable keys the radio has not confirmed.
pub fn missing(ks: &KeyStore, net: &str, issi: u32) -> Vec<SckEntry> {
    let confirmed = ks.sck().nets.get(net).and_then(|n| n.confirmed.get(&issi)).cloned().unwrap_or_default();
    deliverable(ks, net, issi).into_iter().filter(|e| !confirmed.contains(&e.id())).collect()
}

/// Seal `keys` for `issi` under a fresh session key (clause 4.5.2.2). `None` without K.
pub fn seal_for(ks: &KeyStore, issi: u32, keys: &[SckEntry]) -> Option<(String, Vec<SealedSckWire>)> {
    let k = ks.k_of(issi)?;
    let rso = random::<10>();
    let kso = crate::taa1::ta41(&k, &rso);
    let sealed = keys
        .iter()
        .filter_map(|e| {
            let key = e.key()?;
            // SCKN is zero-based inside the sealed block (A.8.70).
            let ssck = crate::taa1::otar::ta51(&key, e.vn, &kso, e.sckn - 1);
            Some(SealedSckWire { sckn: e.sckn, vn: e.vn, ssck: hex(&ssck) })
        })
        .collect();
    Some((hex(&rso), sealed))
}

/// A radio's answer for one key.
pub fn note_result(ks: &mut KeyStore, net: &str, issi: u32, sckn: u8, vn: u16, ok: bool) {
    let Some(n) = ks.sck_mut().nets.get_mut(net) else { return };
    let known = [&n.current, &n.next, &n.fallback].into_iter().flatten().any(|e| e.id() == (sckn, vn));
    if !known {
        return;
    }
    let list = n.confirmed.entry(issi).or_default();
    let had = list.contains(&(sckn, vn));
    if ok && !had {
        list.push((sckn, vn));
    } else if !ok && had {
        list.retain(|k| *k != (sckn, vn));
    } else {
        return;
    }
    n.confirmed.retain(|_, v| !v.is_empty());
    ks.save();
}

/// Radios of `net` with K: seen at one of its stations (Brew logins).
fn members(ks: &KeyStore, logins: &[String]) -> Vec<u32> {
    ks.issis().into_iter().filter(|i| ks.seen_of(*i).is_some_and(|(st, _)| logins.iter().any(|l| l == st))).collect()
}

/// (radios that hold the key, radios of the net).
pub fn coverage(ks: &KeyStore, net: &str, id: (u8, u16), logins: &[String]) -> (usize, usize) {
    let m = members(ks, logins);
    let confirmed = ks.sck().nets.get(net).map(|n| &n.confirmed);
    let have = m.iter().filter(|i| confirmed.and_then(|c| c.get(i)).is_some_and(|v| v.contains(&id))).count();
    (have, m.len())
}

/// Plan the change to the next key (`next`) or to the current one (stations still on their own
/// key) at `at_ms`. Refused below the warning level unless `force`.
#[allow(clippy::too_many_arguments)]
pub fn activate(ks: &mut KeyStore, net: &str, next: bool, at_ms: u64, force: bool, logins: &[String], by: Option<&str>) -> Result<Activation, String> {
    ks.ready()?;
    let warn = ks.sck().warn_pct as usize;
    let n = ks.sck().nets.get(net).ok_or("no SCK for this network")?;
    let e = if next { n.next.as_ref().ok_or("no next SCK")? } else { n.current.as_ref().ok_or("no current SCK")? };
    if n.activation.is_some() {
        return Err("a key change is already planned".into());
    }
    let id = e.id();
    let (have, total) = coverage(ks, net, id, logins);
    if !force && total > 0 && have * 100 < warn * total {
        return Err(format!("only {have} of {total} radios hold SCK {} version {} (warning below {warn} %)", id.0, id.1));
    }
    let a = Activation { sckn: id.0, vn: id.1, at_ms, by: by.map(str::to_owned) };
    ks.sck_mut().nets.get_mut(net).expect("checked").activation = Some(a.clone());
    ks.save();
    Ok(a)
}

/// Drop a planned change. Refused once a station planned it: it has told its radios, and EN
/// 300 392-7 has no PDU that takes a D-CK CHANGE DEMAND back.
pub fn cancel(ks: &mut KeyStore, net: &str) -> Result<bool, String> {
    ks.ready()?;
    if ks.sck_rt.stations.values().any(|st| st.net == net && st.switch_at_ms.is_some()) {
        return Err("stations already announced the change to the radios (D-CK CHANGE DEMAND); it cannot be taken back".into());
    }
    let Some(n) = ks.sck_mut().nets.get_mut(net) else { return Ok(false) };
    let had = n.activation.take().is_some();
    if had {
        ks.save();
    }
    Ok(had)
}

/// After the activation time: the next key is the current one. Returns true when it changed.
pub fn promote(ks: &mut KeyStore, net: &str, now_ms: u64) -> bool {
    let Some(n) = ks.sck_mut().nets.get_mut(net) else { return false };
    let Some(a) = &n.activation else { return false };
    if now_ms < a.at_ms + PROMOTE_AFTER_MS {
        return false;
    }
    let id = (a.sckn, a.vn);
    if n.next.as_ref().is_some_and(|e| e.id() == id) {
        let old = n.current.take().map(|e| e.id());
        n.current = n.next.take();
        if let Some(old) = old {
            n.confirmed.values_mut().for_each(|v| v.retain(|k| *k != old));
            n.confirmed.retain(|_, v| !v.is_empty());
        }
    }
    n.activation = None;
    ks.save();
    true
}

/// `SckKeys` for the stations of `net`.
fn keys_command(ks: &KeyStore, net: &str) -> Option<ControlCommand> {
    let n = ks.sck().nets.get(net)?;
    let keys: Vec<SckWire> = n.current.iter().map(|e| e.wire(false)).chain(n.next.iter().map(|e| e.wire(true))).collect();
    (!keys.is_empty()).then_some(ControlCommand::SckKeys { keys })
}

// ───────────────────────────── stations ─────────────────────────────

/// A listed station as far as phase 8b needs it.
#[derive(Debug, Clone)]
pub struct StationNet {
    pub net: String,
    pub brew_login: Option<String>,
    /// Online, keys from the server, `sck-v1`.
    pub sck: bool,
}

pub fn station_nets(state: &AppState) -> HashMap<String, StationNet> {
    let fleet = state.fleet.lock().unwrap_or_else(|e| e.into_inner());
    fleet
        .stations()
        .into_iter()
        .map(|s| {
            let sck = s.online && s.key_source.as_deref() == Some("server") && s.capabilities.iter().any(|c| c == CAP_SCK);
            (s.id.clone(), StationNet { net: net_name(s.mcc, s.mnc), brew_login: s.brew_login.clone(), sck })
        })
        .collect()
}

fn logins_of(nets: &HashMap<String, StationNet>, net: &str) -> Vec<String> {
    nets.values().filter(|s| s.net == net).filter_map(|s| s.brew_login.clone()).collect()
}

fn keystore(state: &AppState) -> std::sync::MutexGuard<'_, KeyStore> {
    state.fleet_keys.lock().unwrap_or_else(|e| e.into_inner())
}

/// Send the key set (and a planned change) to every station of `net` that uses it.
pub fn push_net(state: &Arc<AppState>, net: &str) {
    let nets = station_nets(state);
    let (cmd, act) = {
        let ks = keystore(state);
        (keys_command(&ks, net), ks.sck().nets.get(net).and_then(|n| n.activation.clone()))
    };
    for (id, _) in nets.iter().filter(|(_, s)| s.sck && s.net == net) {
        if let Some(c) = &cmd {
            crate::fleet_keys::send(state, id, c.clone());
        }
        if let Some(a) = &act {
            crate::fleet_keys::send(state, id, ControlCommand::SckActivate { sckn: a.sckn, vn: a.vn, at_ms: a.at_ms });
        }
    }
}

/// After a hello: a station with server keys gets the set of its network and a planned change.
pub fn after_hello(state: &Arc<AppState>, id: &str) {
    let nets = station_nets(state);
    let Some(s) = nets.get(id).filter(|s| s.sck) else {
        keystore(state).sck_rt.stations.remove(id);
        return;
    };
    let (cmd, act) = {
        let ks = keystore(state);
        (keys_command(&ks, &s.net), ks.sck().nets.get(&s.net).and_then(|n| n.activation.clone()))
    };
    if let Some(c) = cmd {
        crate::fleet_keys::send(state, id, c);
    }
    if let Some(a) = act {
        crate::fleet_keys::send(state, id, ControlCommand::SckActivate { sckn: a.sckn, vn: a.vn, at_ms: a.at_ms });
    }
}

/// What the station runs with. A station that ran a server key and missed a change (offline at
/// the time) is moved to the current key at once; a station on its own config key stays until
/// someone activates the current key for it.
pub fn on_state(state: &Arc<AppState>, id: &str, active: Option<SckRef>, from_config: bool, keys: Vec<SckRef>, switch_at_ms: Option<u64>) {
    let nets = station_nets(state);
    let Some(s) = nets.get(id).filter(|s| s.sck) else { return };
    let catch_up = {
        let mut ks = keystore(state);
        ks.sck_rt.stations.insert(id.to_owned(), StationSck { active: active.clone(), from_config, keys, switch_at_ms, at_ms: crate::fleet::now_ms(), net: s.net.clone() });
        let n = ks.sck().nets.get(&s.net);
        match (n.and_then(|n| n.current.as_ref()), &active) {
            (Some(cur), Some(a)) if !from_config && switch_at_ms.is_none() && n.is_some_and(|n| n.activation.is_none()) && !cur.same_as(a) => Some(cur.id()),
            _ => None,
        }
    };
    if let Some((sckn, vn)) = catch_up {
        tracing::info!(station = %id, sckn, vn, "station missed an SCK change; switching it now");
        crate::fleet_keys::send(state, id, ControlCommand::SckActivate { sckn, vn, at_ms: crate::fleet::now_ms() });
    }
    state.monitor.emit("fleet_changed", serde_json::json!({"keys": true}));
}

/// A radio asked for SCKs (or the station's dashboard did): seal what it may have.
pub fn on_demand(state: &Arc<AppState>, id: &str, issi: u32, sckns: &[u8]) {
    let nets = station_nets(state);
    let Some(s) = nets.get(id).filter(|s| s.sck) else { return };
    let (rso, sealed) = {
        let mut ks = keystore(state);
        let wanted: Vec<SckEntry> = deliverable(&ks, &s.net, issi).into_iter().filter(|e| sckns.is_empty() || sckns.contains(&e.sckn)).collect();
        ks.sck_rt.attempts.insert(issi, crate::fleet::now_ms());
        match seal_for(&ks, issi, &wanted) {
            Some((rso, sealed)) if !wanted.is_empty() => (rso, sealed),
            _ => (hex(&[0u8; 10]), Vec::new()),
        }
    };
    if sealed.is_empty() {
        tracing::info!(station = %id, issi, "SCK request rejected (no K or no such key)");
    }
    crate::fleet_keys::send(state, id, ControlCommand::SckSealed { issi, rso, keys: sealed });
}

pub fn on_result(state: &Arc<AppState>, id: &str, issi: u32, sckn: u8, vn: u16, ok: bool, detail: Option<String>) {
    let nets = station_nets(state);
    let Some(s) = nets.get(id) else { return };
    keystore(state).sck_mut_note(&s.net, issi, sckn, vn, ok);
    if !ok {
        let names = crate::control::station_names(state);
        let why = detail.unwrap_or_default();
        state.fleet.lock().unwrap_or_else(|e| e.into_inner()).log_station(
            id,
            None,
            "keys_sck_failed",
            |label| format!("Basestation {label}: ISSI {issi} did not take SCK {sckn} version {vn}: {why}"),
            crate::fleet::now_ms(),
            &|k| names.get(k).cloned(),
        );
    }
    state.monitor.emit("fleet_changed", serde_json::json!({"keys": true}));
}

impl KeyStore {
    fn sck_mut_note(&mut self, net: &str, issi: u32, sckn: u8, vn: u16, ok: bool) {
        note_result(self, net, issi, sckn, vn, ok);
    }
}

/// Every 15 s (from `fleet_keys::tick`, with the radios registered now and their station's Brew
/// login): finish changes whose time has passed, and send missing keys to registered radios.
pub fn tick(state: &Arc<AppState>, registered: &[(u32, String)]) {
    let now = crate::fleet::now_ms();
    let nets = station_nets(state);
    let promoted: Vec<String> = {
        let mut ks = keystore(state);
        let names: Vec<String> = ks.sck().nets.keys().cloned().collect();
        names.into_iter().filter(|n| promote(&mut ks, n, now)).collect()
    };
    for net in promoted {
        crate::fleet_keys::log(state, None, format!("network {net}: SCK change done, the next key is the current one"));
        push_net(state, &net);
    }
    for (id, s) in nets.iter().filter(|(_, s)| s.sck) {
        let Some(login) = &s.brew_login else { continue };
        let mine: Vec<u32> = registered.iter().filter(|(_, st)| st == login).map(|(i, _)| *i).collect();
        for issi in mine {
            let job = {
                let mut ks = keystore(state);
                let due = ks.sck_rt.attempts.get(&issi).is_none_or(|t| now.saturating_sub(*t) >= RETRY_MS);
                let miss = if due && ks.has_k(issi) { missing(&ks, &s.net, issi) } else { Vec::new() };
                if miss.is_empty() {
                    None
                } else {
                    ks.sck_rt.attempts.insert(issi, now);
                    // Send every deliverable key: a radio with two slots gets both in one PDU.
                    let all = deliverable(&ks, &s.net, issi);
                    seal_for(&ks, issi, &all)
                }
            };
            if let Some((rso, keys)) = job {
                crate::fleet_keys::send(state, id, ControlCommand::SckSealed { issi, rso, keys });
            }
        }
    }
}

/// For `GET /api/fleet/keys`.
pub fn view(ks: &KeyStore, nets: &HashMap<String, StationNet>, now_ms: u64) -> serde_json::Value {
    let entry = |e: &SckEntry| serde_json::json!({"sckn": e.sckn, "vn": e.vn, "check": e.check, "source": e.source, "by": e.by, "set_ms": e.set_ms});
    let mut names: Vec<String> = ks.sck().nets.keys().cloned().collect();
    for s in nets.values() {
        if !names.contains(&s.net) {
            names.push(s.net.clone());
        }
    }
    names.sort();
    let out: serde_json::Map<String, serde_json::Value> = names
        .iter()
        .map(|net| {
            let logins = logins_of(nets, net);
            let n = ks.sck().nets.get(net).cloned().unwrap_or_default();
            let cov = |e: &Option<SckEntry>| e.as_ref().map(|e| coverage(ks, net, e.id(), &logins));
            let stations: Vec<serde_json::Value> = nets
                .iter()
                .filter(|(_, s)| s.net == *net)
                .map(|(id, s)| {
                    let rep = ks.sck_rt.stations.get(id);
                    serde_json::json!({
                        "id": id, "sck": s.sck,
                        "active": rep.and_then(|r| r.active.clone()),
                        "from_config": rep.is_some_and(|r| r.from_config),
                        "switch_at_ms": rep.and_then(|r| r.switch_at_ms),
                        "matches_current": rep.and_then(|r| r.active.as_ref()).zip(n.current.as_ref()).is_some_and(|(a, c)| c.same_as(a)),
                    })
                })
                .collect();
            let radios: serde_json::Map<String, serde_json::Value> = ks
                .issis()
                .into_iter()
                .filter(|i| n.confirmed.contains_key(i) || ks.seen_of(*i).is_some_and(|(st, _)| logins.iter().any(|l| l == st)))
                .map(|i| {
                    let room = ks.sck().profile(ks.model_of(i)).map(|p| p.tm_sck);
                    (i.to_string(), serde_json::json!({"confirmed": n.confirmed.get(&i).cloned().unwrap_or_default(), "room": room, "missing": missing(ks, net, i).iter().map(|e| e.sckn).collect::<Vec<_>>()}))
                })
                .collect();
            (
                net.clone(),
                serde_json::json!({
                    "current": n.current.as_ref().map(entry),
                    "next": n.next.as_ref().map(entry),
                    "activation": n.activation,
                    "coverage_current": cov(&n.current),
                    "coverage_next": cov(&n.next),
                    "fallback": n.fallback.as_ref().map(entry),
                    "coverage_fallback": cov(&n.fallback),
                    "current_in_use": current_in_use(ks, net),
                    "stations": stations,
                    "radios": radios,
                }),
            )
        })
        .collect();
    serde_json::json!({"warn_pct": ks.sck().warn_pct, "fallback_min": ks.sck().fallback_min, "reauth_per_s": ks.sck().reauth_per_s, "profiles": ks.sck().profiles(), "nets": out, "now_ms": now_ms})
}

// ───────────────────────────── dashboard API ─────────────────────────────

use axum::{extract::State, http::{HeaderMap, StatusCode}, response::{IntoResponse, Response}, Json};

#[derive(Deserialize)]
pub struct SetReq {
    net: String,
    /// `current` or `next`.
    role: String,
    sckn: u8,
    #[serde(default)]
    vn: u16,
    /// 20 hex digits; empty: generate.
    #[serde(default)]
    key: String,
}

/// `POST /api/fleet/keys/sck/set` (Fleet Control keys): enter or generate the current or next
/// SCK of a network. A generated key is shown once (for loading radios by cable).
pub async fn api_set(State(state): State<Arc<AppState>>, headers: HeaderMap, Json(req): Json<SetReq>) -> Response {
    let a = crate::dashboard::access(&state, &headers);
    if !a.manages_keys() {
        return crate::fleet_keys::forbidden();
    }
    let next = match req.role.as_str() {
        "next" => true,
        "current" | "fallback" => false,
        _ => return crate::fleet_keys::bad("role: current, next or fallback"),
    };
    let key = if req.key.trim().is_empty() {
        None
    } else {
        match parse_sck(&req.key) {
            Ok(k) => Some(k),
            Err(e) => return crate::fleet_keys::bad(e),
        }
    };
    let res = if req.role == "fallback" {
        set_fallback(&mut keystore(&state), &req.net, req.sckn, req.vn, key, a.user.as_deref(), crate::fleet::now_ms())
    } else {
        set_key(&mut keystore(&state), &req.net, next, req.sckn, req.vn, key, a.user.as_deref(), crate::fleet::now_ms())
    };
    match res {
        Ok((check, generated)) => {
            crate::fleet_keys::log(
                &state,
                a.user.as_deref(),
                format!("network {}: {} SCK {} version {} {} (check value {check})", req.net, req.role, req.sckn, req.vn, if generated.is_some() { "generated" } else { "entered" }),
            );
            push_net(&state, &req.net);
            if req.role == "fallback" {
                crate::fleet_fallback::push_net(&state, &req.net);
            }
            let grouped = generated.map(|k| {
                let h = hex(&k).to_uppercase();
                h.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).unwrap_or("")).collect::<Vec<_>>().join(" ")
            });
            Json(serde_json::json!({"ok": true, "check": check, "key_once": grouped})).into_response()
        }
        Err(e) => crate::fleet_keys::bad(e),
    }
}

#[derive(Deserialize)]
pub struct NetReq {
    net: String,
    /// For `delete`: `next` (default) or `current`.
    #[serde(default = "role_next")]
    role: String,
}

/// `POST /api/fleet/keys/sck/delete`: drop the next SCK (no change planned), or the current one
/// while no station runs it.
pub async fn api_delete(State(state): State<Arc<AppState>>, headers: HeaderMap, Json(req): Json<NetReq>) -> Response {
    let a = crate::dashboard::access(&state, &headers);
    if !a.manages_keys() {
        return crate::fleet_keys::forbidden();
    }
    let current = req.role == "current";
    let fallback = req.role == "fallback";
    let res = if fallback {
        delete_fallback(&mut keystore(&state), &req.net)
    } else if current {
        delete_current(&mut keystore(&state), &req.net)
    } else {
        delete_next(&mut keystore(&state), &req.net)
    };
    match res {
        Ok(had) => {
            if had {
                crate::fleet_keys::log(&state, a.user.as_deref(), format!("network {}: {} SCK removed", req.net, req.role));
                push_net(&state, &req.net);
                if fallback {
                    crate::fleet_fallback::push_net(&state, &req.net);
                }
            }
            Json(serde_json::json!({"ok": true, "removed": had})).into_response()
        }
        Err(e) => crate::fleet_keys::bad(e),
    }
}

#[derive(Deserialize)]
pub struct ActivateReq {
    net: String,
    /// `next` (the change) or `current` (stations still on their own key).
    #[serde(default = "role_next")]
    role: String,
    /// Seconds from now (0: at once, the next multiframe on each station).
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

/// `POST /api/fleet/keys/sck/activate` (Fleet Control keys and management): plan the network
/// wide change. Refused below the warning level without `force`.
pub async fn api_activate(State(state): State<Arc<AppState>>, headers: HeaderMap, Json(req): Json<ActivateReq>) -> Response {
    let a = crate::dashboard::access(&state, &headers);
    if !a.manages_keys() || !a.manages_fleet() {
        return (StatusCode::FORBIDDEN, "This needs the rights Fleet Control keys and Fleet Control management").into_response();
    }
    if req.in_secs > MAX_LEAD_SECS {
        return crate::fleet_keys::bad("at most 7 days ahead");
    }
    let nets = station_nets(&state);
    let logins = logins_of(&nets, &req.net);
    let at_ms = crate::fleet::now_ms() + req.in_secs * 1000;
    let res = activate(&mut keystore(&state), &req.net, req.role != "current", at_ms, req.force, &logins, a.user.as_deref());
    match res {
        Ok(act) => {
            crate::fleet_keys::log(
                &state,
                a.user.as_deref(),
                format!("network {}: change to SCK {} version {} planned in {} s{}", req.net, act.sckn, act.vn, req.in_secs, if req.force { " (below the warning level)" } else { "" }),
            );
            push_net(&state, &req.net);
            Json(serde_json::json!({"ok": true, "at_ms": act.at_ms})).into_response()
        }
        Err(e) => (StatusCode::CONFLICT, e).into_response(),
    }
}

/// `POST /api/fleet/keys/sck/cancel`: drop a planned change. Stations that already switched keep
/// the new key; the others stay on the old one.
pub async fn api_cancel(State(state): State<Arc<AppState>>, headers: HeaderMap, Json(req): Json<NetReq>) -> Response {
    let a = crate::dashboard::access(&state, &headers);
    if !a.manages_keys() || !a.manages_fleet() {
        return (StatusCode::FORBIDDEN, "This needs the rights Fleet Control keys and Fleet Control management").into_response();
    }
    let res = cancel(&mut keystore(&state), &req.net);
    match res {
        Ok(had) => {
            if had {
                crate::fleet_keys::log(&state, a.user.as_deref(), format!("network {}: planned SCK change cancelled", req.net));
                push_net(&state, &req.net);
            }
            Json(serde_json::json!({"ok": true, "cancelled": had})).into_response()
        }
        Err(e) => crate::fleet_keys::bad(e),
    }
}

#[derive(Deserialize)]
pub struct SendReq {
    net: String,
    /// One radio; none: every registered radio that misses a key.
    #[serde(default)]
    issi: Option<u32>,
}

/// `POST /api/fleet/keys/sck/send`: "send now" — the next tick delivers to these radios without
/// waiting for the retry interval.
pub async fn api_send(State(state): State<Arc<AppState>>, headers: HeaderMap, Json(req): Json<SendReq>) -> Response {
    let a = crate::dashboard::access(&state, &headers);
    if !a.manages_keys() {
        return crate::fleet_keys::forbidden();
    }
    {
        let mut ks = keystore(&state);
        match req.issi {
            Some(i) => {
                ks.sck_rt.attempts.remove(&i);
            }
            None => ks.sck_rt.attempts.clear(),
        }
    }
    crate::fleet_keys::log(&state, a.user.as_deref(), format!("network {}: SCK delivery started{}", req.net, req.issi.map(|i| format!(" for ISSI {i}")).unwrap_or_default()));
    crate::fleet_keys::tick(&state).await;
    Json(serde_json::json!({"ok": true})).into_response()
}

#[derive(Deserialize)]
pub struct SettingsReq {
    warn_pct: u8,
    /// Phase 8d; `None`: keep.
    #[serde(default)]
    fallback_min: Option<u8>,
    #[serde(default)]
    reauth_per_s: Option<u8>,
    /// `None`: keep; empty list: back to the built-in profiles.
    #[serde(default)]
    profiles: Option<Vec<Profile>>,
}

/// `POST /api/fleet/keys/sck/settings` (admins): warning level and device profiles.
pub async fn api_settings(State(state): State<Arc<AppState>>, headers: HeaderMap, Json(req): Json<SettingsReq>) -> Response {
    let a = crate::dashboard::access(&state, &headers);
    if !a.admin {
        return (StatusCode::FORBIDDEN, "Admins only").into_response();
    }
    if req.warn_pct > 100 {
        return crate::fleet_keys::bad("warning level 0-100 %");
    }
    if req.fallback_min.is_some_and(|m| !(1..=60).contains(&m)) {
        return crate::fleet_keys::bad("fallback after 1-60 min");
    }
    if req.reauth_per_s.is_some_and(|r| !(1..=20).contains(&r)) {
        return crate::fleet_keys::bad("re-authentication 1-20 radios per second");
    }
    if let Some(list) = &req.profiles {
        let mut seen = std::collections::HashSet::new();
        for p in list {
            let name = p.name.trim();
            if name.is_empty() || name.len() > 40 || !seen.insert(name.to_ascii_lowercase()) {
                return crate::fleet_keys::bad(format!("profile name {:?}: 1-40 characters, each name once", p.name));
            }
            if !(1..=32).contains(&p.tm_sck) {
                return crate::fleet_keys::bad(format!("profile {}: trunked mode SCKs 1-32", p.name));
            }
        }
    }
    let res = {
        let mut ks = keystore(&state);
        ks.ready().map(|_| {
            let d = ks.sck_mut();
            d.warn_pct = req.warn_pct;
            if let Some(m) = req.fallback_min {
                d.fallback_min = m;
            }
            if let Some(r) = req.reauth_per_s {
                d.reauth_per_s = r;
            }
            if let Some(list) = req.profiles {
                d.profiles = if list.is_empty() { None } else { Some(list.into_iter().map(|p| Profile { name: p.name.trim().to_owned(), ..p }).collect()) };
            }
            ks.save();
        })
    };
    match res {
        Ok(()) => {
            crate::fleet_keys::log(&state, a.user.as_deref(), format!("SCK settings changed (warning below {} %)", req.warn_pct));
            crate::fleet_fallback::push_all(&state);
            Json(serde_json::json!({"ok": true})).into_response()
        }
        Err(e) => crate::fleet_keys::bad(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const K: [u8; 16] = [0x57; 16];

    /// A station of `net` reports that it runs the current key with this value.
    fn station_runs(ks: &mut KeyStore, net: &str, key: [u8; 10]) {
        let cur = ks.sck().nets[net].current.clone().unwrap();
        let active = Some(SckRef { sckn: cur.sckn, vn: cur.vn, check: check_value(&key) });
        ks.sck_rt.stations.insert("bs1".into(), StationSck { active, from_config: true, keys: vec![], switch_at_ms: None, at_ms: 0, net: net.into() });
    }

    #[test]
    fn nothing_goes_to_radios_until_a_station_runs_the_current_key_and_an_unused_one_can_go() {
        let mut ks = store_with_radios(&[100]);
        set_key(&mut ks, "232-1", false, 1, 0, Some([0x11; 10]), None, 0).unwrap();
        // The station runs another value under the same number: the server's key is wrong.
        station_runs(&mut ks, "232-1", [0x99; 10]);
        assert!(missing(&ks, "232-1", 100).is_empty(), "a wrong current key must not reach the radios");
        assert!(delete_current(&mut ks, "232-1").unwrap());
        assert!(!ks.sck().nets.contains_key("232-1"));
        set_key(&mut ks, "232-1", false, 1, 0, Some([0x99; 10]), None, 0).unwrap();
        assert_eq!(missing(&ks, "232-1", 100).len(), 1, "the station runs it: radios may get it");
        set_key(&mut ks, "232-1", true, 2, 1, None, None, 0).unwrap();
        assert!(delete_current(&mut ks, "232-1").unwrap_err().contains("next key first"));
        delete_next(&mut ks, "232-1").unwrap();
        assert!(delete_current(&mut ks, "232-1").unwrap_err().contains("stations run"));
    }

    fn store_with_radios(issis: &[u32]) -> KeyStore {
        let mut ks = KeyStore::memory();
        for &i in issis {
            ks.set_k(i, &K, "manual", None, 0).unwrap();
            ks.note_seen(i, "1000001", 1);
        }
        ks
    }

    #[test]
    fn the_fallback_sck_is_31_or_32_and_reaches_class3_radios_without_a_current_key() {
        let mut ks = store_with_radios(&[100]);
        assert!(set_fallback(&mut ks, "232-1", 1, 0, None, None, 0).unwrap_err().contains("31 or 32"));
        let (check, shown) = set_fallback(&mut ks, "232-1", 31, 4, Some([0x5A; 10]), None, 0).unwrap();
        assert_eq!(check, check_value(&[0x5A; 10]));
        assert!(shown.is_none());
        // Class 3 network: no current SCK, the fallback key goes to the radios at once.
        assert_eq!(missing(&ks, "232-1", 100).iter().map(|e| e.id()).collect::<Vec<_>>(), vec![(31, 4)]);
        note_result(&mut ks, "232-1", 100, 31, 4, true);
        assert!(missing(&ks, "232-1", 100).is_empty());
        assert_eq!(coverage(&ks, "232-1", (31, 4), &["1000001".to_string()]), (1, 1));
        // A replaced fallback key must go again.
        set_fallback(&mut ks, "232-1", 32, 5, None, None, 0).unwrap();
        assert_eq!(missing(&ks, "232-1", 100).iter().map(|e| e.sckn).collect::<Vec<_>>(), vec![32]);
        assert_eq!(fallback_of(&ks, "232-1").map(|w| (w.sckn, w.vn)), Some((32, 5)));
        assert!(delete_fallback(&mut ks, "232-1").unwrap());
        assert!(fallback_of(&ks, "232-1").is_none());
        assert!(!ks.sck().nets.contains_key("232-1"), "nothing left of the network");
    }

    #[test]
    fn the_fallback_sck_takes_a_free_slot_and_its_own_number() {
        let mut ks = store_with_radios(&[100, 101]);
        set_key(&mut ks, "232-1", false, 31, 1, Some([0x11; 10]), None, 0).unwrap();
        assert!(set_fallback(&mut ks, "232-1", 31, 2, None, None, 0).unwrap_err().contains("already"));
        set_fallback(&mut ks, "232-1", 32, 2, Some([0x5A; 10]), None, 0).unwrap();
        // While no station runs the current key nothing goes, the fallback key neither.
        assert!(missing(&ks, "232-1", 100).is_empty());
        station_runs(&mut ks, "232-1", [0x11; 10]);
        assert_eq!(missing(&ks, "232-1", 100).iter().map(|e| e.sckn).collect::<Vec<_>>(), vec![31, 32]);
        // A radio with one slot keeps the key in use.
        ks.sck_mut().profiles = Some(vec![Profile { name: "One".into(), tm_sck: 1, dm_sck: 0, gck: 0, kag: 0 }]);
        ks.set_model(101, "one").unwrap();
        assert_eq!(missing(&ks, "232-1", 101).iter().map(|e| e.sckn).collect::<Vec<_>>(), vec![31]);
        // Removing the current key keeps the fallback key.
        ks.sck_rt.stations.clear();
        assert!(delete_current(&mut ks, "232-1").unwrap());
        assert_eq!(fallback_of(&ks, "232-1").map(|w| w.sckn), Some(32));
    }

    #[test]
    fn typed_sck_loses_separators_and_needs_20_hex_digits() {
        assert_eq!(parse_sck("0123 4567:89AB-CDEF 0123").unwrap(), [0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF, 0x01, 0x23]);
        assert!(parse_sck("0123456789ABCDEF012").unwrap_err().contains("got 19"));
        assert!(parse_sck("0123456789ABCDEF012G").is_err());
    }

    #[test]
    fn the_next_key_needs_its_own_number_and_a_current_key() {
        let mut ks = store_with_radios(&[]);
        assert!(set_key(&mut ks, "232-1", true, 2, 1, None, None, 0).unwrap_err().contains("current SCK first"));
        let (check, shown) = set_key(&mut ks, "232-1", false, 1, 5, Some([0x11; 10]), None, 0).unwrap();
        assert_eq!(check, check_value(&[0x11; 10]));
        assert!(shown.is_none(), "an entered key is never shown again");
        assert!(set_key(&mut ks, "232-1", true, 1, 6, None, None, 0).unwrap_err().contains("another number"));
        let (_, shown) = set_key(&mut ks, "232-1", true, 2, 6, None, None, 0).unwrap();
        assert!(shown.is_some(), "a generated key is shown once");
        assert!(set_key(&mut ks, "bad", false, 1, 1, None, None, 0).is_err());
        assert!(set_key(&mut ks, "232-1", false, 33, 1, None, None, 0).is_err());
    }

    #[test]
    fn sealed_keys_open_with_the_radios_k_only() {
        let mut ks = store_with_radios(&[100]);
        set_key(&mut ks, "232-1", false, 1, 5, Some([0x11; 10]), None, 0).unwrap();
        station_runs(&mut ks, "232-1", [0x11; 10]);
        set_key(&mut ks, "232-1", true, 2, 6, Some([0x22; 10]), None, 0).unwrap();
        let keys = missing(&ks, "232-1", 100);
        assert_eq!(keys.iter().map(|e| e.id()).collect::<Vec<_>>(), vec![(1, 5), (2, 6)]);
        let (rso, sealed) = seal_for(&ks, 100, &keys).unwrap();
        let kso = crate::taa1::ta41(&K, &unhex_n::<10>(&rso).unwrap());
        for (s, want) in sealed.iter().zip([[0x11; 10], [0x22; 10]]) {
            let ssck = unhex_n::<15>(&s.ssck).unwrap();
            assert_eq!(crate::taa1::otar::ta52(&ssck, &kso, s.vn), Ok((want, s.sckn - 1)));
            let wrong = crate::taa1::ta41(&[0x58; 16], &unhex_n::<10>(&rso).unwrap());
            assert!(crate::taa1::otar::ta52(&ssck, &wrong, s.vn).is_err(), "another radio's K cannot open it");
        }
        assert!(seal_for(&ks, 200, &keys).is_none(), "no K: nothing to seal");
    }

    #[test]
    fn a_radio_with_one_slot_gets_only_the_current_key() {
        let mut ks = store_with_radios(&[100, 101]);
        set_key(&mut ks, "232-1", false, 1, 5, Some([0x11; 10]), None, 0).unwrap();
        station_runs(&mut ks, "232-1", [0x11; 10]);
        set_key(&mut ks, "232-1", true, 2, 6, Some([0x22; 10]), None, 0).unwrap();
        ks.sck_mut().profiles = Some(vec![Profile { name: "One".into(), tm_sck: 1, dm_sck: 0, gck: 0, kag: 0 }]);
        ks.set_model(101, "one").unwrap();
        assert_eq!(missing(&ks, "232-1", 100).len(), 2);
        assert_eq!(missing(&ks, "232-1", 101).iter().map(|e| e.sckn).collect::<Vec<_>>(), vec![1]);
    }

    #[test]
    fn activation_waits_for_the_warning_level_and_the_change_promotes_next() {
        let radios: Vec<u32> = (100..110).collect();
        let mut ks = store_with_radios(&radios);
        let logins = vec!["1000001".to_string()];
        set_key(&mut ks, "232-1", false, 1, 5, Some([0x11; 10]), None, 0).unwrap();
        set_key(&mut ks, "232-1", true, 2, 6, Some([0x22; 10]), None, 0).unwrap();
        for &i in &radios[..8] {
            note_result(&mut ks, "232-1", i, 2, 6, true);
        }
        note_result(&mut ks, "232-1", 100, 9, 9, true); // a key the network does not have
        assert_eq!(coverage(&ks, "232-1", (2, 6), &logins), (8, 10));
        let err = activate(&mut ks, "232-1", true, 1000, false, &logins, None).unwrap_err();
        assert!(err.contains("only 8 of 10"), "{err}");
        note_result(&mut ks, "232-1", 108, 2, 6, true);
        let a = activate(&mut ks, "232-1", true, 1000, false, &logins, Some("seb")).unwrap();
        assert_eq!((a.sckn, a.vn, a.at_ms), (2, 6, 1000));
        assert!(set_key(&mut ks, "232-1", true, 3, 7, None, None, 0).is_err(), "no new key while a change is planned");
        assert!(!promote(&mut ks, "232-1", 1000 + PROMOTE_AFTER_MS - 1));
        assert!(promote(&mut ks, "232-1", 1000 + PROMOTE_AFTER_MS));
        let n = &ks.sck().nets["232-1"];
        assert_eq!(n.current.as_ref().map(|e| e.id()), Some((2, 6)));
        assert!(n.next.is_none() && n.activation.is_none());
        assert_eq!(n.confirmed[&100], vec![(2, 6)], "confirmations of the old key go");
    }

    #[test]
    fn a_failed_result_and_a_replaced_key_drop_the_confirmation() {
        let mut ks = store_with_radios(&[100]);
        set_key(&mut ks, "232-1", false, 1, 5, Some([0x11; 10]), None, 0).unwrap();
        station_runs(&mut ks, "232-1", [0x11; 10]);
        set_key(&mut ks, "232-1", true, 2, 6, Some([0x22; 10]), None, 0).unwrap();
        note_result(&mut ks, "232-1", 100, 2, 6, true);
        note_result(&mut ks, "232-1", 100, 1, 5, true);
        assert!(missing(&ks, "232-1", 100).is_empty());
        note_result(&mut ks, "232-1", 100, 1, 5, false);
        assert_eq!(missing(&ks, "232-1", 100).iter().map(|e| e.sckn).collect::<Vec<_>>(), vec![1]);
        // Same number and version, another value: the radio must get it again.
        set_key(&mut ks, "232-1", true, 2, 6, Some([0x33; 10]), None, 0).unwrap();
        assert_eq!(missing(&ks, "232-1", 100).len(), 2);
    }

    #[test]
    fn a_change_a_station_already_announced_cannot_be_cancelled() {
        let mut ks = store_with_radios(&[]);
        set_key(&mut ks, "232-1", false, 1, 5, Some([0x11; 10]), None, 0).unwrap();
        set_key(&mut ks, "232-1", true, 2, 6, Some([0x22; 10]), None, 0).unwrap();
        activate(&mut ks, "232-1", true, 5000, true, &[], None).unwrap();
        ks.sck_rt.stations.insert("bs1".into(), StationSck { active: None, from_config: true, keys: vec![], switch_at_ms: Some(5000), at_ms: 0, net: "232-1".into() });
        assert!(cancel(&mut ks, "232-1").unwrap_err().contains("cannot be taken back"));
        ks.sck_rt.stations.clear();
        assert!(cancel(&mut ks, "232-1").unwrap(), "no station has it yet");
    }

    #[test]
    fn the_keys_survive_a_restart_inside_the_encrypted_store() {
        let dir = std::env::temp_dir().join(format!("brew-sck-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let storage = dir.join("history");
        let mut ks = KeyStore::load(Some(&storage), None);
        set_key(&mut ks, "232-1", false, 1, 5, Some([0xA1; 10]), None, 0).unwrap();
        let raw = std::fs::read_to_string(dir.join("history.fleet-keys")).unwrap();
        assert!(!raw.to_lowercase().contains(&hex(&[0xA1; 10])), "never in clear on disk");
        let again = KeyStore::load(Some(&storage), None);
        assert_eq!(again.sck().nets["232-1"].current.as_ref().and_then(|e| e.key()), Some([0xA1; 10]));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn wire_shapes_match_the_station() {
        let cmd = ControlCommand::SckKeys { keys: vec![SckWire { sckn: 1, vn: 5, key: "00112233445566778899".into(), next: false }] };
        assert_eq!(cmd.to_wire_json(), serde_json::json!({"SckKeys": {"keys": [{"sckn": 1, "vn": 5, "key": "00112233445566778899", "next": false}]}}));
        let cmd = ControlCommand::SckActivate { sckn: 2, vn: 6, at_ms: 7 };
        assert_eq!(cmd.to_wire_json(), serde_json::json!({"SckActivate": {"sckn": 2, "vn": 6, "at_ms": 7}}));
        let cmd = ControlCommand::SckSealed { issi: 9, rso: "00".into(), keys: vec![] };
        assert_eq!(cmd.to_wire_json(), serde_json::json!({"SckSealed": {"issi": 9, "rso": "00", "keys": []}}));
        let r: crate::control::ControlResponse =
            serde_json::from_value(serde_json::json!({"SckState": {"active": {"sckn": 1, "vn": 5, "check": "AB"}, "from_config": true, "keys": []}})).unwrap();
        assert!(matches!(r, crate::control::ControlResponse::SckState { from_config: true, active: Some(_), switch_at_ms: None, .. }));
        let r: crate::control::ControlResponse = serde_json::from_value(serde_json::json!({"SckResult": {"issi": 9, "sckn": 1, "vn": 5, "ok": false, "detail": "x"}})).unwrap();
        assert!(matches!(r, crate::control::ControlResponse::SckResult { ok: false, .. }));
    }
}
