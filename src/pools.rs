//! Talkgroup pools: groups switched together inside this server.
//!
//! A call on a group of a pool that is switched on is played 1:1 on the
//! pool's other groups. Each copy (a *leg*) is an ordinary group call on its
//! own GSSI with its own call identifier, so a Basestation sets it up for its
//! radios like any other group call from the network and the radios see
//! nothing unusual. The voice frames of the call are copied to every leg, and
//! the legs end with the call.
//!
//! A group that has a call of its own when the pool call starts is not cut
//! off: its leg waits until that call has ended and then joins the running
//! pool call (late entry, the same way a radio joining mid-over does).
//!
//! Pools are edited live from the dashboard and written to the config file
//! without a restart (as the blacklist is). Only this server's own
//! connections get the legs; federation peers do not.

use crate::config::TalkgroupPoolConfig;
use crate::protocol;
use crate::state::{ActiveCall, AppState, CallKind, ClientId, ClientMode, Inner};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::info;
use uuid::Uuid;

/// Owner of every leg: not a connection, so no connection can send voice or
/// end a leg as if it were the talker. Legs end with their pool call.
pub const POOL_OWNER: ClientId = Uuid::from_u128(0x706f_6f6c_0000_4000_8000_0000_0000_0001);

/// Longest pool name accepted from the dashboard.
const MAX_NAME: usize = 40;

/// The legs of one running pool call, keyed by the call that started them.
#[derive(Debug, Clone)]
pub struct PoolRun {
    /// Pool (name) that put the first leg up.
    pub pool: String,
    /// Group the call is on.
    pub origin_gssi: u32,
    /// Connection the call came in on (its talker's station).
    pub owner: ClientId,
    pub source_issi: u32,
    pub priority: u8,
    /// Running legs: target group -> leg call identifier.
    pub legs: BTreeMap<u32, Uuid>,
    /// Target groups that had a call of their own; they join when it ends.
    pub waiting: BTreeSet<u32>,
}

/// Pool state kept with the calls (under `AppState::inner`).
#[derive(Debug, Default)]
pub struct PoolCalls {
    pub runs: std::collections::HashMap<Uuid, PoolRun>,
    /// Leg call identifier -> the call it copies.
    pub legs: std::collections::HashMap<Uuid, Uuid>,
}

/// Groups a call on `gssi` is copied to by the pools switched on in `pools`,
/// each with the name of the (first) pool that copies it there.
pub fn targets_for(pools: &[TalkgroupPoolConfig], gssi: u32) -> BTreeMap<u32, String> {
    let mut out = BTreeMap::new();
    for p in pools.iter().filter(|p| p.enabled) {
        let copies = if p.both_ways { p.groups.contains(&gssi) } else { p.groups.first() == Some(&gssi) };
        if !copies { continue; }
        for &t in p.groups.iter().filter(|&&t| t != gssi) {
            out.entry(t).or_insert_with(|| p.name.clone());
        }
    }
    out
}

/// Checks a pool list from the dashboard; the error says what is wrong.
pub fn validate(pools: &[TalkgroupPoolConfig]) -> Result<(), String> {
    let mut names = HashSet::new();
    for p in pools {
        let name = p.name.trim();
        if name.is_empty() { return Err("every pool needs a name".into()); }
        if name.chars().count() > MAX_NAME { return Err(format!("pool name '{name}' is longer than {MAX_NAME} characters")); }
        if !names.insert(name.to_lowercase()) { return Err(format!("two pools are named '{name}'")); }
        if p.groups.len() < 2 { return Err(format!("pool '{name}' needs at least two groups")); }
        let mut seen = HashSet::new();
        for &g in &p.groups {
            if g == 0 || g > 0xFF_FFFF { return Err(format!("pool '{name}': {g} is not a valid GSSI (1 to 16777215)")); }
            if !seen.insert(g) { return Err(format!("pool '{name}' lists group {g} twice")); }
        }
    }
    Ok(())
}

/// Replaces the pools on the running server and writes the config file
/// without a restart. Running pool calls keep their legs until they end.
pub async fn apply(state: &Arc<AppState>, mut pools: Vec<TalkgroupPoolConfig>, by: &str) -> Result<(), String> {
    for p in &mut pools { p.name = p.name.trim().to_owned(); }
    validate(&pools)?;
    if let Ok(mut live) = state.pools.write() { *live = pools.clone(); }
    tracing::warn!(count = pools.len(), by, "talkgroup pools changed");
    let cfg = state.config_snapshot();
    crate::CONFIG_WRITE_APPLIED_LIVE.store(true, std::sync::atomic::Ordering::SeqCst);
    let saved = crate::dashboard::save_config(state, &cfg).await;
    if saved.is_err() {
        crate::CONFIG_WRITE_APPLIED_LIVE.store(false, std::sync::atomic::Ordering::SeqCst);
    }
    saved.map_err(|e| format!("applied, but writing the config file failed: {e}"))
}

/// Whether `gssi` has a group call other than `origin` and the legs of
/// `origin`'s own run.
fn busy(inner: &Inner, gssi: u32, origin: Uuid) -> bool {
    let own = inner.pools.runs.get(&origin);
    inner.calls.iter().any(|(id, c)| {
        c.kind == CallKind::Group && c.destination == gssi && *id != origin
            && !own.is_some_and(|r| r.legs.values().any(|l| l == id))
    })
}

/// Connections a leg on `gssi` goes to: the ones routing the group, as for
/// any group call, but no federation peer.
fn leg_targets(state: &AppState, inner: &Inner, gssi: u32) -> HashSet<ClientId> {
    let mut targets = if state.config.route_without_affiliations {
        inner.clients.keys().copied().collect::<HashSet<_>>()
    } else {
        inner.group_clients.get(&gssi).cloned().unwrap_or_default()
    };
    if targets.is_empty() && !state.config.route_without_affiliations && state.config.fallback_broadcast_when_no_affiliations {
        targets = inner.clients.keys().copied().collect();
    }
    targets.retain(|c| inner.clients.get(c).is_some_and(|c| c.mode != ClientMode::Peer));
    targets
}

type Out = Vec<(mpsc::UnboundedSender<Vec<u8>>, Vec<u8>)>;

/// Sets up the leg of `origin`'s run on `gssi`: a new group call there, its
/// GROUP_TX queued for each target. Returns the leg's identifier.
fn start_leg(state: &AppState, inner: &mut Inner, origin: Uuid, gssi: u32, out: &mut Out) -> Option<Uuid> {
    let run = inner.pools.runs.get(&origin)?.clone();
    let leg = Uuid::new_v4();
    let peers = leg_targets(state, inner, gssi);
    let header = protocol::build_group_tx(&leg, run.source_issi, gssi, run.priority);
    for cid in &peers {
        if let Some(c) = inner.clients.get(cid) {
            out.push((c.tx.clone(), protocol::adapt_to_version(&header, c.forward_version()).into_owned()));
        }
    }
    inner.calls.insert(leg, ActiveCall {
        kind: CallKind::Group,
        owner: POOL_OWNER,
        source_issi: run.source_issi,
        destination: gssi,
        priority: run.priority,
        peers: peers.clone(),
        started_at: std::time::Instant::now(),
        last_activity_ms: ActiveCall::new_activity(),
    });
    inner.group_floor.entry(gssi).or_insert(leg);
    inner.group_tx_raw.insert(leg, header);
    inner.pools.legs.insert(leg, origin);
    if let Some(r) = inner.pools.runs.get_mut(&origin) {
        r.legs.insert(gssi, leg);
        r.waiting.remove(&gssi);
    }
    info!(origin=%origin, leg=%leg, from_gssi=run.origin_gssi, gssi, pool=%run.pool, targets=peers.len(), "talkgroup pool: leg set up");
    Some(leg)
}

/// A group call was routed (`GROUP_TX` on `id` from `source`): puts up its
/// legs on the groups its pools copy it to, or, for a new over of a call
/// that already has legs, announces the new talker on them. Calls the legs
/// started for the monitor.
pub async fn on_group_tx(state: &Arc<AppState>, source: ClientId, id: Uuid, source_issi: u32, gssi: u32, priority: u8) {
    let pools = state.pools.read().map(|p| p.clone()).unwrap_or_default();
    let mut started = Vec::new();
    {
        let mut inner = state.inner.write().await;
        let mut out: Out = Vec::new();
        // A station sending a GROUP_TX on a leg's identifier: one of its
        // radios talks in the leg's call, which is now an ordinary call of
        // that station; it is no longer a copy.
        if let Some(origin) = inner.pools.legs.remove(&id) {
            if let Some(r) = inner.pools.runs.get_mut(&origin) { r.legs.retain(|_, l| *l != id); }
            return;
        }
        let mut wanted = targets_for(&pools, gssi);
        // A group that does not admit the talker gets no copy (`group_access`).
        wanted.retain(|t, _| state.group_permits(*t, source_issi));
        if wanted.is_empty() && !inner.pools.runs.contains_key(&id) { return; }
        let pool = wanted.values().next().cloned().unwrap_or_default();
        let run = inner.pools.runs.entry(id).or_insert_with(|| PoolRun {
            pool, origin_gssi: gssi, owner: source, source_issi, priority,
            legs: BTreeMap::new(), waiting: BTreeSet::new(),
        });
        run.owner = source;
        run.source_issi = source_issi;
        run.priority = priority;
        let existing = run.legs.clone();
        // New over of a call with legs: the legs get the new talker.
        for (&t, &leg) in &existing {
            let header = protocol::build_group_tx(&leg, source_issi, t, priority);
            if let Some(call) = inner.calls.get(&leg) {
                for cid in &call.peers {
                    if let Some(c) = inner.clients.get(cid) {
                        out.push((c.tx.clone(), protocol::adapt_to_version(&header, c.forward_version()).into_owned()));
                    }
                }
                call.touch();
            }
            inner.group_tx_raw.insert(leg, header);
        }
        for &t in wanted.keys().filter(|t| !existing.contains_key(t)) {
            if busy(&inner, t, id) {
                if let Some(r) = inner.pools.runs.get_mut(&id) { r.waiting.insert(t); }
                info!(origin=%id, gssi=t, "talkgroup pool: group has its own call, joins when it ends");
            } else if let Some(leg) = start_leg(state, &mut inner, id, t, &mut out) {
                started.push((leg, t));
            }
        }
        for (tx, msg) in out { let _ = tx.send(msg); }
    }
    for (leg, t) in started {
        state.monitor.call_started(leg, "group", source_issi, t, priority).await;
    }
}

/// A voice or DTMF frame of call `id` was routed: copies it to the call's
/// legs (same frame, the leg's call identifier).
pub async fn on_frame(state: &Arc<AppState>, id: Uuid, raw: &[u8]) {
    let inner = state.inner.read().await;
    let Some(run) = inner.pools.runs.get(&id) else { return };
    if raw.len() < 18 { return; }
    for leg in run.legs.values() {
        let Some(call) = inner.calls.get(leg) else { continue };
        call.touch();
        let mut frame = raw.to_vec();
        frame[2..18].copy_from_slice(leg.as_bytes());
        for cid in &call.peers {
            if let Some(c) = inner.clients.get(cid) { let _ = c.tx.send(frame.clone()); }
        }
    }
}

/// Call `id` on `gssi` has ended. Returns the legs to end with it (the
/// caller ends them like any call), and starts the legs that waited for a
/// call on `gssi` to end.
pub async fn on_call_end(state: &Arc<AppState>, id: Uuid, gssi: u32) -> Vec<Uuid> {
    let mut to_end = Vec::new();
    let mut started = Vec::new();
    {
        let mut inner = state.inner.write().await;
        if let Some(origin) = inner.pools.legs.remove(&id) {
            if let Some(r) = inner.pools.runs.get_mut(&origin) { r.legs.retain(|_, l| *l != id); }
        }
        if let Some(run) = inner.pools.runs.remove(&id) {
            to_end.extend(run.legs.values().copied());
            info!(origin=%id, legs=run.legs.len(), "talkgroup pool: call ended, ending its legs");
        }
        // Runs whose call went without an end (pre-empted): drop them too.
        let gone: Vec<Uuid> = inner.pools.runs.keys().filter(|o| !inner.calls.contains_key(o)).copied().collect();
        for o in gone {
            if let Some(run) = inner.pools.runs.remove(&o) { to_end.extend(run.legs.values().copied()); }
        }
        for leg in &to_end { inner.pools.legs.remove(leg); }
        // Legs waiting for this group to become free join now.
        let waiting: Vec<Uuid> = inner.pools.runs.iter()
            .filter(|(_, r)| r.waiting.contains(&gssi))
            .map(|(o, _)| *o).collect();
        let mut out: Out = Vec::new();
        for origin in waiting {
            if busy(&inner, gssi, origin) { continue; }
            if let Some(leg) = start_leg(state, &mut inner, origin, gssi, &mut out) {
                let r = &inner.pools.runs[&origin];
                started.push((leg, r.source_issi, r.priority));
                info!(origin=%origin, leg=%leg, gssi, "talkgroup pool: waiting group joined the running call");
            }
        }
        for (tx, msg) in out { let _ = tx.send(msg); }
    }
    for (leg, src, prio) in started {
        state.monitor.call_started(leg, "group", src, gssi, prio).await;
    }
    to_end
}

/// What the dashboard shows of a pool.
#[derive(Debug, Serialize)]
pub struct PoolView {
    #[serde(flatten)]
    pub pool: TalkgroupPoolConfig,
    /// Running call on the pool, if any.
    pub live: Option<LiveView>,
}

#[derive(Debug, Serialize)]
pub struct LiveView {
    pub gssi: u32,
    pub talker: u32,
    pub legs: Vec<u32>,
    pub waiting: Vec<u32>,
}

/// The pools with their running calls, for `/api/pools`.
pub async fn snapshot(state: &Arc<AppState>) -> Vec<PoolView> {
    let pools = state.pools.read().map(|p| p.clone()).unwrap_or_default();
    let inner = state.inner.read().await;
    pools.into_iter().map(|p| {
        let live = inner.pools.runs.values()
            .find(|r| r.pool == p.name)
            .map(|r| LiveView { gssi: r.origin_gssi, talker: r.source_issi, legs: r.legs.keys().copied().collect(), waiting: r.waiting.iter().copied().collect() });
        PoolView { pool: p, live }
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(name: &str, both: bool, groups: &[u32]) -> TalkgroupPoolConfig {
        TalkgroupPoolConfig { name: name.into(), enabled: true, both_ways: both, groups: groups.to_vec() }
    }

    #[test]
    fn both_ways_pool_copies_from_every_group() {
        let p = [pool("Nord", true, &[2321, 2323, 264])];
        assert_eq!(targets_for(&p, 2321).keys().copied().collect::<Vec<_>>(), vec![264, 2323]);
        assert_eq!(targets_for(&p, 264).keys().copied().collect::<Vec<_>>(), vec![2321, 2323]);
        assert!(targets_for(&p, 9).is_empty());
    }

    #[test]
    fn one_way_pool_copies_only_from_its_first_group() {
        let p = [pool("Info", false, &[9, 2321, 2322])];
        assert_eq!(targets_for(&p, 9).keys().copied().collect::<Vec<_>>(), vec![2321, 2322]);
        assert!(targets_for(&p, 2321).is_empty());
    }

    #[test]
    fn switched_off_pool_copies_nothing() {
        let mut p = pool("Off", true, &[1, 2]);
        p.enabled = false;
        assert!(targets_for(&[p], 1).is_empty());
    }

    #[test]
    fn validation_rejects_bad_pools() {
        assert!(validate(&[pool("A", true, &[1, 2])]).is_ok());
        assert!(validate(&[pool("", true, &[1, 2])]).is_err());
        assert!(validate(&[pool("A", true, &[1])]).is_err());
        assert!(validate(&[pool("A", true, &[1, 1])]).is_err());
        assert!(validate(&[pool("A", true, &[1, 0x100_0000])]).is_err());
        assert!(validate(&[pool("A", true, &[1, 2]), pool("a", true, &[3, 4])]).is_err());
    }
}
