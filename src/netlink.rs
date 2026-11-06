//! Multi Network: links from this server to other Brew networks.
//!
//! Each `[[multi_network.networks]]` entry is one outbound connection to that
//! network's Brew server in plain Basestation mode, so the far server needs
//! nothing new: to it this server is one more Basestation, logged in with the
//! Basestation login that network gave out and introducing itself with the
//! name the operator chose (`user_agent`). The credentials stay on this
//! server; no station ever sees them.
//!
//! What crosses a link is decided by the talkgroup pools (see `pools`): a
//! pool may list groups of other networks (`remote`) next to this server's
//! own groups, which makes it a bridge across networks. A call on one member
//! is played on the others as an ordinary group call on their own GSSI, with
//! the talker's ISSI passed on as it is. Nothing else crosses: registrations,
//! services and calls that are not on a bridged group are dropped in both
//! directions.
//!
//! Inside this server a group of another network is addressed by an *alias*:
//! `(slot << 24) | gssi`, where the slot (1 to 255) stands for the network.
//! A GSSI has 24 bits (TS 100 392-2 V3.10.1 clause 16.10.28, Table 16.60)
//! and Brew carries it in a 32-bit field, so an alias can never collide with
//! a real group. The link puts the real GSSI back on everything it sends out and
//! the alias on everything it lets in, so the router and the pool machinery
//! work unchanged and a station never sees an alias.
//!
//! How a network is reached is not defined by ETSI: Brew is not an ETSI
//! interface, and the inter-system interface (ISI, EN 300 392-3) is not used
//! here. The links are edited live from the dashboard and written to the
//! config file without a restart.

use crate::config::{MultiNetworkConfig, NetworkConfig, TalkgroupPoolConfig};
use crate::federation::{self, Keepalive, LinkIdentity};
use crate::protocol::{self, ConnVersion};
use crate::state::{AppState, Client, ClientId, ClientMode};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Longest network name accepted from the dashboard.
const MAX_NAME: usize = 40;
/// Longest `user_agent` accepted.
const MAX_USER_AGENT: usize = 80;
/// Bits of a GSSI; the slot sits above them.
const GSSI_BITS: u32 = 24;
const GSSI_MASK: u32 = 0xFF_FFFF;

/// Internal address of group `gssi` of the network in `slot`.
pub fn alias(slot: u8, gssi: u32) -> u32 {
    (u32::from(slot) << GSSI_BITS) | (gssi & GSSI_MASK)
}

/// `(slot, gssi)` of an alias; `None` for a group of this server.
pub fn split_alias(value: u32) -> Option<(u8, u32)> {
    let slot = (value >> GSSI_BITS) as u8;
    (slot != 0).then_some((slot, value & GSSI_MASK))
}

/// The running links and what the dashboard shows of them.
#[derive(Default)]
pub struct NetLinks {
    reg: Mutex<Registry>,
}

#[derive(Default)]
struct Registry {
    /// Set once the services run (an HA standby node starts no links).
    started: bool,
    /// Network name -> slot, kept for the life of the process so an alias
    /// never changes under a running call.
    slots: HashMap<String, u8>,
    tasks: HashMap<String, LinkTask>,
    status: HashMap<String, LinkStatus>,
    /// Numbers the link tasks, so a stopped task cannot overwrite the state
    /// of the one that replaced it.
    next_gen: u64,
}

struct LinkTask {
    cfg: NetworkConfig,
    stop: watch::Sender<bool>,
    gen: u64,
}

/// State of one link.
#[derive(Debug, Clone, Default)]
struct LinkStatus {
    connected: bool,
    since_ms: u64,
    last_error: String,
    /// Raw upstream channel of the connected link (bypasses the filter).
    up: Option<mpsc::UnboundedSender<Vec<u8>>>,
    /// Groups the bridge ISSI is affiliated to there right now.
    affiliated: BTreeSet<u32>,
}

impl NetLinks {
    fn lock(&self) -> std::sync::MutexGuard<'_, Registry> {
        self.reg.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Slot of every network in `cfg`, assigning new ones as needed. A
    /// network that finds no free slot (more than 255 in one process life)
    /// is left out.
    pub fn slots(&self, cfg: &MultiNetworkConfig) -> HashMap<String, u8> {
        let mut reg = self.lock();
        for n in &cfg.networks {
            if reg.slots.contains_key(&n.name) { continue; }
            let used: HashSet<u8> = reg.slots.values().copied().collect();
            if let Some(free) = (1..=u8::MAX).find(|s| !used.contains(s)) {
                reg.slots.insert(n.name.clone(), free);
            }
        }
        cfg.networks.iter().filter_map(|n| reg.slots.get(&n.name).map(|s| (n.name.clone(), *s))).collect()
    }
}

/// Slot of every configured network (see `NetLinks::slots`).
pub fn slots(state: &AppState) -> HashMap<String, u8> {
    let cfg = state.networks.read().map(|c| c.clone()).unwrap_or_default();
    state.netlinks.slots(&cfg)
}

/// Starts the links (called once the services run).
pub async fn run(state: Arc<AppState>) {
    state.netlinks.lock().started = true;
    reconcile(&state);
}

/// Brings the running links in line with `state.networks`: stops the ones
/// removed, switched off or changed, starts the missing ones.
pub fn reconcile(state: &Arc<AppState>) {
    let cfg = state.networks.read().map(|c| c.clone()).unwrap_or_default();
    let slots = state.netlinks.slots(&cfg);
    let mut reg = state.netlinks.lock();
    reg.status.retain(|name, _| cfg.networks.iter().any(|n| &n.name == name));
    if !reg.started { return; }
    let stale: Vec<String> = reg.tasks.iter()
        .filter(|(name, task)| !cfg.enabled || !cfg.networks.iter().any(|n| n.enabled && &n.name == *name && *n == task.cfg))
        .map(|(name, _)| name.clone()).collect();
    for name in stale {
        info!(network = %name, "multi network: stopping link");
        if let Some(task) = reg.tasks.remove(&name) { let _ = task.stop.send(true); }
        reg.status.remove(&name);
    }
    for net in cfg.networks.iter().filter(|n| cfg.enabled && n.enabled) {
        if reg.tasks.contains_key(&net.name) { continue; }
        let Some(&slot) = slots.get(&net.name) else { continue };
        let (stop, stop_rx) = watch::channel(false);
        reg.next_gen += 1;
        let gen = reg.next_gen;
        reg.tasks.insert(net.name.clone(), LinkTask { cfg: net.clone(), stop, gen });
        reg.status.insert(net.name.clone(), LinkStatus::default());
        let (state, net) = (state.clone(), net.clone());
        tokio::spawn(async move { link_loop(state, net, slot, gen, stop_rx).await });
    }
}

/// Updates the state of link task `gen` of network `name`; a task that has
/// been stopped or replaced changes nothing.
fn set_status(state: &AppState, name: &str, gen: u64, f: impl FnOnce(&mut LinkStatus)) {
    let mut reg = state.netlinks.lock();
    if reg.tasks.get(name).is_some_and(|t| t.gen == gen) {
        f(reg.status.entry(name.to_owned()).or_default());
    }
}

fn stopped(stop: &watch::Receiver<bool>) -> bool {
    *stop.borrow() || stop.has_changed().is_err()
}

async fn link_loop(state: Arc<AppState>, net: NetworkConfig, slot: u8, gen: u64, mut stop: watch::Receiver<bool>) {
    let interval = Duration::from_secs(net.reconnect_interval_seconds.max(5));
    loop {
        if stopped(&stop) { break; }
        info!(network = %net.name, remote_host = %net.remote_host, "multi network: connecting");
        match connect_and_run(&state, &net, slot, gen, &mut stop).await {
            Ok(()) => set_status(&state, &net.name, gen, |s| { s.connected = false; s.up = None; }),
            Err(e) => {
                warn!(network = %net.name, error = %format!("{e:#}"), "multi network: link failed");
                set_status(&state, &net.name, gen, |s| { s.connected = false; s.up = None; s.last_error = format!("{e:#}"); });
            }
        }
        if stopped(&stop) { break; }
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = stop.changed() => {}
        }
    }
    info!(network = %net.name, "multi network: link stopped");
}

/// The `User-Agent` a link introduces itself with: `user_agent` when it is a
/// valid header value (printable ASCII), else this server's own.
pub fn user_agent(net: &NetworkConfig) -> &str {
    let ua = net.user_agent.trim();
    if valid_user_agent(ua) && !ua.is_empty() { ua } else { federation::USER_AGENT }
}

fn valid_user_agent(ua: &str) -> bool {
    ua.len() <= MAX_USER_AGENT && ua.bytes().all(|b| (0x20..0x7f).contains(&b))
}

async fn connect_and_run(state: &Arc<AppState>, net: &NetworkConfig, slot: u8, gen: u64, stop: &mut watch::Receiver<bool>) -> anyhow::Result<()> {
    let peer = net.as_peer();
    let tls = if peer.tls { Some(federation::tls_connector(&peer)?) } else { None };
    let who = LinkIdentity { mode: "Basestation", user_agent: user_agent(net) };
    let dial = async {
        // Paths as configured / discovered: a trailing slash may matter to
        // the far server.
        let path = leading_slash(&peer.path);
        let ws_path = federation::discover(&peer, tls.as_ref(), &path, &[], who).await?;
        let remote_addr = tokio::net::lookup_host(&peer.remote_host).await.ok().and_then(|mut it| it.next());
        let scheme = if tls.is_some() { "wss" } else { "ws" };
        let ws_url = format!("{scheme}://{}{}", peer.remote_host, leading_slash(&ws_path));
        let mut request = ws_url.into_client_request()?;
        request.headers_mut().insert("User-Agent", who.user_agent.parse()?);
        request.headers_mut().insert("X-Brew-Mode", "Basestation".parse()?);
        request.headers_mut().insert("X-Brew-Version", protocol::BREW_PROTOCOL_VERSION.to_string().parse()?);
        request.headers_mut().insert("Sec-WebSocket-Protocol", "brew".parse()?);
        let stream = federation::dial(&peer, tls.as_ref()).await?;
        let (ws, _resp) = tokio_tungstenite::client_async(request, stream).await?;
        anyhow::Ok((ws, remote_addr))
    };
    let (ws, remote_addr) = tokio::select! {
        r = tokio::time::timeout(federation::DIAL_TIMEOUT, dial) => r
            .map_err(|_| anyhow::anyhow!("timed out after {}s connecting", federation::DIAL_TIMEOUT.as_secs()))??,
        _ = stop.changed() => return Ok(()),
    };
    info!(network = %net.name, slot, "multi network: link up");
    run_session(state, net, slot, gen, ws, remote_addr, stop).await;
    Ok(())
}

fn leading_slash(path: &str) -> String {
    let p = path.trim();
    if p.starts_with('/') { p.to_string() } else { format!("/{p}") }
}

/// Groups of network `name` that the switched-on pools bridge.
pub fn bridged_groups(pools: &[TalkgroupPoolConfig], name: &str) -> BTreeSet<u32> {
    pools.iter().filter(|p| p.enabled)
        .flat_map(|p| p.remote.iter())
        .filter(|r| r.network == name)
        .map(|r| r.gssi)
        .collect()
}

fn pools_now(state: &AppState) -> Vec<TalkgroupPoolConfig> {
    state.pools.read().map(|p| p.clone()).unwrap_or_default()
}

/// Upstream messages that bring the bridge ISSI's affiliations from `have`
/// to `want` (nothing without a bridge ISSI).
fn affiliation_changes(issi: u32, have: &BTreeSet<u32>, want: &BTreeSet<u32>) -> Vec<Vec<u8>> {
    if issi == 0 { return Vec::new(); }
    let mut out = Vec::new();
    let gone: Vec<u32> = have.difference(want).copied().collect();
    let new: Vec<u32> = want.difference(have).copied().collect();
    if !gone.is_empty() { out.push(protocol::build_subscriber_message(protocol::SUB_DEAFFILIATE, issi, &gone)); }
    if !new.is_empty() { out.push(protocol::build_subscriber_message(protocol::SUB_AFFILIATE, issi, &new)); }
    out
}

/// After a pool change: every connected link affiliates its bridge ISSI to
/// the groups now bridged and leaves the others.
pub fn refresh_affiliations(state: &AppState) {
    let pools = pools_now(state);
    let cfg = state.networks.read().map(|c| c.clone()).unwrap_or_default();
    let mut reg = state.netlinks.lock();
    for net in &cfg.networks {
        let Some(status) = reg.status.get_mut(&net.name) else { continue };
        let Some(up) = status.up.clone() else { continue };
        let want = bridged_groups(&pools, &net.name);
        for msg in affiliation_changes(net.bridge_issi, &status.affiliated, &want) { let _ = up.send(msg); }
        if net.bridge_issi != 0 { status.affiliated = want; }
    }
}

/// Call identifiers of a link: this server's own on one side, the other
/// network's on the other. A call crossing a link gets a new identifier
/// there, so a copy that comes back (a loop through the same server, or
/// over another way) is never taken for the call it came from, and an
/// answer in a call over there finds the call it answers here.
#[derive(Debug, Default)]
pub struct CallIds {
    to_remote: HashMap<Uuid, Uuid>,
    to_local: HashMap<Uuid, Uuid>,
}

impl CallIds {
    fn pair(&mut self, local: Uuid, remote: Uuid) {
        if self.to_remote.len() >= MAX_CALLS { self.to_remote.clear(); self.to_local.clear(); }
        self.to_remote.insert(local, remote);
        self.to_local.insert(remote, local);
    }

    fn forget_local(&mut self, local: &Uuid) {
        if let Some(r) = self.to_remote.remove(local) { self.to_local.remove(&r); }
    }
}

/// Shared by the two directions of one link.
pub type SharedCallIds = Arc<Mutex<CallIds>>;

fn ids_lock(ids: &SharedCallIds) -> std::sync::MutexGuard<'_, CallIds> {
    ids.lock().unwrap_or_else(|e| e.into_inner())
}

/// What a link lets out: group calls on an alias of its own slot (with the
/// real GSSI put back) and the call control and frames of those calls, all
/// under the call's identifier over there. Everything else this server sends
/// its connections stays here.
#[derive(Debug)]
pub struct Outbound {
    slot: u8,
    ids: SharedCallIds,
    /// Talkers registered over there by this link, with their groups
    /// (`None` = talkers are not registered).
    talkers: Option<SharedTalkers>,
    bridge_issi: u32,
}

/// Talkers a link registered in the other network (ISSI -> groups).
pub type SharedTalkers = Arc<std::sync::Mutex<HashMap<u32, BTreeSet<u32>>>>;

/// Bound on the talkers a link keeps registered.
const MAX_TALKERS: usize = 1024;

/// What a link lets in: group calls on the groups it bridges (with the alias
/// put on) and the call control and frames of those calls, under this
/// server's identifier of the call.
#[derive(Debug)]
pub struct Inbound {
    slot: u8,
    ids: SharedCallIds,
}

/// Bound on the calls a link remembers (a call whose end was lost).
const MAX_CALLS: usize = 4096;
const DEST_AT: usize = 2 + 16 + 4;

fn call_id(pkt: &[u8]) -> Option<Uuid> {
    pkt.get(2..18).and_then(|b| Uuid::from_slice(b).ok())
}

fn set_call_id(pkt: &mut [u8], id: Uuid) {
    pkt[2..18].copy_from_slice(id.as_bytes());
}

fn is_call_or_frame(pkt: &[u8]) -> bool {
    matches!(pkt.first(), Some(&protocol::CLASS_CALL_CONTROL) | Some(&protocol::CLASS_FRAME))
}

fn is_group_tx(pkt: &[u8]) -> bool {
    pkt.first() == Some(&protocol::CLASS_CALL_CONTROL) && pkt.get(1) == Some(&protocol::CALL_GROUP_TX) && pkt.len() >= DEST_AT + 4
}

fn is_group_idle(pkt: &[u8]) -> bool {
    pkt.first() == Some(&protocol::CLASS_CALL_CONTROL) && pkt.get(1) == Some(&protocol::CALL_GROUP_IDLE)
}

fn dest(pkt: &[u8]) -> u32 {
    u32::from_le_bytes(pkt[DEST_AT..DEST_AT + 4].try_into().unwrap_or_default())
}

fn src(pkt: &[u8]) -> u32 {
    u32::from_le_bytes(pkt[DEST_AT - 4..DEST_AT].try_into().unwrap_or_default())
}

/// One readable log line for a call crossing the link (GROUP_TX = a new
/// over, GROUP_IDLE = call end); other packets are not logged.
fn log_crossing(network: &str, dir: &str, before: &[u8], after: Option<&[u8]>) {
    if is_group_tx(before) {
        let (talker, gssi) = (src(before), dest(before));
        match (dir, after) {
            ("in", Some(_)) => info!("multi network: call in from {network}:{gssi}, talker {talker}"),
            ("in", None) => debug!("multi network: call on {network}:{gssi} not bridged, dropped"),
            (_, Some(p)) => info!("multi network: call out to {network}:{}, talker {talker}", dest(p)),
            _ => {}
        }
    } else if is_group_idle(before) && after.is_some() {
        info!("multi network: call {dir} ended ({network})");
    }
}

fn set_dest(pkt: &mut [u8], value: u32) {
    pkt[DEST_AT..DEST_AT + 4].copy_from_slice(&value.to_le_bytes());
}

impl Outbound {
    pub fn new(slot: u8, ids: SharedCallIds) -> Self { Self { slot, ids, talkers: None, bridge_issi: 0 } }

    /// Also register talkers over there before their first over.
    pub fn registering(mut self, talkers: SharedTalkers, bridge_issi: u32) -> Self {
        self.talkers = Some(talkers);
        self.bridge_issi = bridge_issi;
        self
    }

    /// Everything the link sends for `pkt`: the talker's registration and
    /// affiliation there when it is new (as a Basestation does for a radio
    /// before it talks), then the packet as it goes over, or nothing if
    /// the packet stays here.
    pub fn packets(&mut self, pkt: Vec<u8>) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        if let (Some(talkers), true) = (&self.talkers, is_group_tx(&pkt)) {
            let talker = src(&pkt);
            if let Some((slot, gssi)) = split_alias(dest(&pkt)) {
                if slot == self.slot && talker != 0 && talker <= GSSI_MASK && talker != self.bridge_issi {
                    let mut t = talkers.lock().unwrap_or_else(|e| e.into_inner());
                    if !t.contains_key(&talker) && t.len() < MAX_TALKERS {
                        out.push(protocol::build_subscriber_message(protocol::SUB_REGISTER, talker, &[]));
                        t.insert(talker, BTreeSet::new());
                    }
                    if let Some(groups) = t.get_mut(&talker) {
                        if groups.insert(gssi) {
                            out.push(protocol::build_subscriber_message(protocol::SUB_AFFILIATE, talker, &[gssi]));
                        }
                    }
                }
            }
        }
        if let Some(p) = self.translate(pkt) { out.push(p); }
        out
    }

    /// The packet as it goes to the other network, or `None` if it stays.
    pub fn translate(&mut self, mut pkt: Vec<u8>) -> Option<Vec<u8>> {
        if !is_call_or_frame(&pkt) { return None; }
        let local = call_id(&pkt)?;
        let mut ids = ids_lock(&self.ids);
        if is_group_tx(&pkt) {
            let (slot, gssi) = split_alias(dest(&pkt))?;
            if slot != self.slot { return None; }
            set_dest(&mut pkt, gssi);
            let remote = match ids.to_remote.get(&local) {
                Some(r) => *r,
                None => { let r = Uuid::new_v4(); ids.pair(local, r); r }
            };
            set_call_id(&mut pkt, remote);
            return Some(pkt);
        }
        let remote = *ids.to_remote.get(&local)?;
        if is_group_idle(&pkt) { ids.forget_local(&local); }
        set_call_id(&mut pkt, remote);
        Some(pkt)
    }
}

impl Inbound {
    pub fn new(slot: u8, ids: SharedCallIds) -> Self { Self { slot, ids } }

    /// The packet as the router gets it, or `None` if it is dropped.
    /// `bridged` says whether a group of that network is in a pool.
    pub fn translate(&mut self, mut pkt: Vec<u8>, bridged: impl Fn(u32) -> bool) -> Option<Vec<u8>> {
        if !is_call_or_frame(&pkt) { return None; }
        let remote = call_id(&pkt)?;
        let mut ids = ids_lock(&self.ids);
        if is_group_tx(&pkt) {
            let gssi = dest(&pkt);
            if gssi == 0 || gssi > GSSI_MASK || !bridged(gssi) { return None; }
            set_dest(&mut pkt, alias(self.slot, gssi));
            // A call of ours over there (an answer in it) keeps our id.
            let local = match ids.to_local.get(&remote) {
                Some(l) => *l,
                None => { let l = Uuid::new_v4(); ids.pair(l, remote); l }
            };
            set_call_id(&mut pkt, local);
            return Some(pkt);
        }
        let local = *ids.to_local.get(&remote)?;
        if is_group_idle(&pkt) { ids.forget_local(&local); }
        set_call_id(&mut pkt, local);
        Some(pkt)
    }
}

async fn run_session(
    state: &Arc<AppState>,
    net: &NetworkConfig,
    slot: u8,
    gen: u64,
    ws: tokio_tungstenite::WebSocketStream<Box<dyn federation::PeerIo>>,
    remote_addr: Option<std::net::SocketAddr>,
    stop: &mut watch::Receiver<bool>,
) {
    let id = Uuid::new_v4();
    let (mut ws_tx, mut ws_rx) = ws.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (up_tx, mut up_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    {
        let mut inner = state.inner.write().await;
        inner.clients.insert(id, Client {
            tx, mode: ClientMode::Network, version: ConnVersion::V1, version_announced: false,
            remote_addr, connected_at_ms: crate::telemetry::now_ms(), username: Some(net.username.clone()),
        });
        inner.netlinks.insert(slot, id);
    }
    // The bridge ISSI registers there and affiliates to the bridged groups,
    // so that network's server sends their calls over this link.
    let want = bridged_groups(&pools_now(state), &net.name);
    if net.bridge_issi != 0 {
        let _ = up_tx.send(protocol::build_subscriber_message(protocol::SUB_REGISTER, net.bridge_issi, &[]));
        for msg in affiliation_changes(net.bridge_issi, &BTreeSet::new(), &want) { let _ = up_tx.send(msg); }
    }
    set_status(state, &net.name, gen, |s| {
        s.connected = true;
        s.since_ms = crate::telemetry::now_ms();
        s.last_error.clear();
        s.up = Some(up_tx.clone());
        s.affiliated = if net.bridge_issi != 0 { want } else { BTreeSet::new() };
    });

    let keepalive = Keepalive::from_config(&state.config.federation);
    let mut ping = keepalive.map(Keepalive::ping_timer);
    let ids: SharedCallIds = Arc::default();
    let talkers: SharedTalkers = Arc::default();
    let mut outbound = Outbound::new(slot, ids.clone());
    if net.register_talkers { outbound = outbound.registering(talkers.clone(), net.bridge_issi); }
    let out_name = net.name.clone();
    let writer = tokio::spawn(async move {
        loop {
            let packet = tokio::select! {
                p = rx.recv() => match p {
                    Some(p) => {
                        let probe = (is_group_tx(&p) || is_group_idle(&p)).then(|| p.clone());
                        let out = outbound.packets(p);
                        if let Some(before) = probe { log_crossing(&out_name, "out", &before, out.last().map(|p| p.as_slice()).filter(|p| p.first() == Some(&protocol::CLASS_CALL_CONTROL))); }
                        out
                    }
                    None => break,
                },
                p = up_rx.recv() => match p { Some(p) => vec![p], None => break },
                _ = federation::ping_due(&mut ping) => {
                    if ws_tx.send(Message::Ping(Default::default())).await.is_err() { break; }
                    continue;
                }
            };
            let mut gone = false;
            for p in packet {
                if ws_tx.send(Message::Binary(p.into())).await.is_err() { gone = true; break; }
            }
            if gone { break; }
        }
        let _ = ws_tx.close().await;
    });

    let mut inbound = Inbound::new(slot, ids);
    let name = net.name.clone();
    loop {
        let item = tokio::select! {
            item = federation::next_frame(&mut ws_rx, keepalive) => item,
            _ = stop.changed() => break,
        };
        let item = match item {
            Ok(Some(item)) => item,
            Ok(None) => break,
            Err(_) => { warn!(network = %name, "multi network: link silent past keepalive timeout; closing"); break; }
        };
        match item {
            Ok(Message::Binary(data)) => {
                let pools = pools_now(state);
                let groups = bridged_groups(&pools, &name);
                let probe = (is_group_tx(&data) || is_group_idle(&data)).then(|| data.to_vec());
                let p = inbound.translate(data.to_vec(), |g| groups.contains(&g));
                if let Some(before) = probe { log_crossing(&name, "in", &before, p.as_deref()); }
                if let Some(p) = p {
                    crate::router::handle_packet(state.clone(), id, p).await;
                }
            }
            Ok(Message::Close(_)) => break,
            Ok(_) => {}
            Err(e) => { warn!(network = %name, error = %e, "multi network: read error"); break; }
        }
    }
    // Deregister the bridge ISSI on a deliberate stop (best effort).
    // Deregister the talkers it registered there, too.
    if stopped(stop) {
        let registered: Vec<u32> = talkers.lock().map(|t| t.keys().copied().collect()).unwrap_or_default();
        for issi in &registered {
            let _ = up_tx.send(protocol::build_subscriber_message(protocol::SUB_DEREGISTER, *issi, &[]));
        }
        if net.bridge_issi != 0 {
            let _ = up_tx.send(protocol::build_subscriber_message(protocol::SUB_DEREGISTER, net.bridge_issi, &[]));
        }
        if net.bridge_issi != 0 || !registered.is_empty() { tokio::time::sleep(Duration::from_millis(200)).await; }
    }
    drop(up_tx);
    writer.abort();
    {
        let mut inner = state.inner.write().await;
        if inner.netlinks.get(&slot) == Some(&id) { inner.netlinks.remove(&slot); }
    }
    state.cleanup_client(id).await;
    info!(network = %name, "multi network: link closed");
}

/// The connection that carries calls to group `alias` of another network,
/// if its link is up.
pub fn link_for(inner: &crate::state::Inner, alias: u32) -> Option<ClientId> {
    let (slot, _) = split_alias(alias)?;
    inner.netlinks.get(&slot).copied()
}

/// Checks a network list from the dashboard; the error says what is wrong.
pub fn validate(cfg: &MultiNetworkConfig, pools: &[TalkgroupPoolConfig]) -> Result<(), String> {
    if cfg.name.chars().count() > MAX_NAME { return Err(format!("the network name is longer than {MAX_NAME} characters")); }
    check_mni(cfg.mcc, cfg.mnc, "this server")?;
    let mut names = HashSet::new();
    for n in &cfg.networks {
        let name = n.name.trim();
        if name.is_empty() { return Err("every network needs a name".into()); }
        if name.chars().count() > MAX_NAME { return Err(format!("network name '{name}' is longer than {MAX_NAME} characters")); }
        if !names.insert(name.to_lowercase()) { return Err(format!("two networks are named '{name}'")); }
        if n.enabled && n.remote_host.trim().is_empty() { return Err(format!("network '{name}' needs a server (host:port)")); }
        if !n.remote_host.trim().is_empty() && !has_port(&n.remote_host) {
            return Err(format!("network '{name}': the server needs a port (host:port, e.g. {}:443)", n.remote_host.trim()));
        }
        if n.bridge_issi > GSSI_MASK { return Err(format!("network '{name}': {} is not a valid ISSI (0 to 16777215)", n.bridge_issi)); }
        if !valid_user_agent(n.user_agent.trim()) {
            return Err(format!("network '{name}': the connection name may only use plain characters (no umlauts), up to {MAX_USER_AGENT}"));
        }
        check_mni(n.mcc, n.mnc, name)?;
    }
    for p in pools {
        if let Some(r) = p.remote.iter().find(|r| !cfg.networks.iter().any(|n| n.name == r.network)) {
            return Err(format!("pool '{}' uses network '{}'", p.name, r.network));
        }
    }
    Ok(())
}

/// Whether `host` ends in `:port` (1..=65535): `name:port` or `[v6]:port`.
fn has_port(host: &str) -> bool {
    let host = host.trim();
    let port = match host.strip_prefix('[') {
        Some(rest) => rest.split_once("]:").map(|(_, p)| p),
        None => host.split_once(':').filter(|(_, p)| !p.contains(':')).map(|(_, p)| p),
    };
    port.and_then(|p| p.parse::<u16>().ok()).is_some_and(|p| p != 0)
}

/// MCC is 10 bits and MNC 14 bits (TS 100 392-2 clause 18.4.2.1, D-MLE-SYNC,
/// Table 18.17).
fn check_mni(mcc: u16, mnc: u16, what: &str) -> Result<(), String> {
    if mcc > 1023 { return Err(format!("{what}: MCC {mcc} is out of range (0 to 1023)")); }
    if mnc > 16383 { return Err(format!("{what}: MNC {mnc} is out of range (0 to 16383)")); }
    Ok(())
}

/// Replaces the network list on the running server, writes the config file
/// and restarts only the links whose settings changed. A network sent with
/// an empty password keeps the one it has.
pub async fn apply(state: &Arc<AppState>, mut cfg: MultiNetworkConfig, by: &str) -> Result<(), String> {
    let old = state.networks.read().map(|c| c.clone()).unwrap_or_default();
    cfg.name = cfg.name.trim().to_owned();
    for n in &mut cfg.networks {
        n.name = n.name.trim().to_owned();
        n.remote_host = n.remote_host.trim().to_owned();
        n.user_agent = n.user_agent.trim().to_owned();
        if n.password.is_empty() {
            if let Some(o) = old.networks.iter().find(|o| o.name == n.name) { n.password = o.password.clone(); }
        }
    }
    validate(&cfg, &pools_now(state))?;
    if let Ok(mut live) = state.networks.write() { *live = cfg.clone(); }
    warn!(count = cfg.networks.len(), enabled = cfg.enabled, by, "multi network: networks changed");
    reconcile(state);
    let snapshot = state.config_snapshot();
    crate::CONFIG_WRITE_APPLIED_LIVE.store(true, std::sync::atomic::Ordering::SeqCst);
    let saved = crate::dashboard::save_config(state, &snapshot).await;
    if saved.is_err() {
        crate::CONFIG_WRITE_APPLIED_LIVE.store(false, std::sync::atomic::Ordering::SeqCst);
    }
    saved.map_err(|e| format!("applied, but writing the config file failed: {e}"))
}

/// What the dashboard shows of one network (never its password).
#[derive(Debug, Serialize)]
pub struct NetworkView {
    pub name: String,
    /// Slot of its group aliases (see `alias`), so the dashboard can name
    /// them.
    pub slot: u8,
    pub enabled: bool,
    pub mcc: u16,
    pub mnc: u16,
    pub remote_host: String,
    pub path: String,
    pub username: String,
    pub has_password: bool,
    pub user_agent: String,
    pub bridge_issi: u32,
    pub register_talkers: bool,
    pub reconnect_interval_seconds: u64,
    pub tls: bool,
    pub tls_server_name: String,
    pub tls_ca_path: String,
    pub tls_pinned_cert_path: String,
    pub connected: bool,
    pub since_ms: u64,
    pub last_error: String,
    pub bridged: Vec<u32>,
    pub affiliated: Vec<u32>,
}

#[derive(Debug, Serialize)]
pub struct MultiNetworkView {
    pub enabled: bool,
    pub name: String,
    pub mcc: u16,
    pub mnc: u16,
    pub default_user_agent: &'static str,
    pub networks: Vec<NetworkView>,
}

/// The networks with the state of their links, for `/api/networks`.
pub fn snapshot(state: &AppState) -> MultiNetworkView {
    let cfg = state.networks.read().map(|c| c.clone()).unwrap_or_default();
    let pools = pools_now(state);
    let slots = state.netlinks.slots(&cfg);
    let reg = state.netlinks.lock();
    let networks = cfg.networks.iter().map(|n| {
        let s = reg.status.get(&n.name).cloned().unwrap_or_default();
        NetworkView {
            name: n.name.clone(), slot: slots.get(&n.name).copied().unwrap_or(0), enabled: n.enabled, mcc: n.mcc, mnc: n.mnc,
            remote_host: n.remote_host.clone(), path: n.path.clone(), username: n.username.clone(),
            has_password: !n.password.is_empty(), user_agent: n.user_agent.clone(), bridge_issi: n.bridge_issi,
            register_talkers: n.register_talkers, reconnect_interval_seconds: n.reconnect_interval_seconds, tls: n.tls,
            tls_server_name: n.tls_server_name.clone(),
            tls_ca_path: n.tls_ca_path.display().to_string(),
            tls_pinned_cert_path: n.tls_pinned_cert_path.display().to_string(),
            connected: s.connected, since_ms: s.since_ms, last_error: s.last_error,
            bridged: bridged_groups(&pools, &n.name).into_iter().collect(),
            affiliated: s.affiliated.into_iter().collect(),
        }
    }).collect();
    MultiNetworkView { enabled: cfg.enabled, name: cfg.name, mcc: cfg.mcc, mnc: cfg.mnc, default_user_agent: federation::USER_AGENT, networks }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RemotePoolGroup;

    fn group_tx(id: Uuid, src: u32, dst: u32) -> Vec<u8> {
        protocol::build_group_tx(&id, src, dst, 0)
    }

    #[test]
    fn talker_is_registered_and_affiliated_once_before_its_first_over() {
        let talkers: SharedTalkers = Arc::default();
        let mut out = Outbound::new(2, Arc::default()).registering(talkers.clone(), 9_000_001);
        let kinds = |v: &[Vec<u8>]| v.iter().map(|p| (p[0], p[1])).collect::<Vec<_>>();
        let reg = (protocol::CLASS_SUBSCRIBER, protocol::SUB_REGISTER);
        let aff = (protocol::CLASS_SUBSCRIBER, protocol::SUB_AFFILIATE);
        let gtx = (protocol::CLASS_CALL_CONTROL, protocol::CALL_GROUP_TX);
        let first = out.packets(group_tx(Uuid::new_v4(), 2320972, alias(2, 909)));
        assert_eq!(kinds(&first), vec![reg, aff, gtx]);
        assert_eq!(&first[1][18..22], &909u32.to_le_bytes());
        assert_eq!(kinds(&out.packets(group_tx(Uuid::new_v4(), 2320972, alias(2, 909)))), vec![gtx]);
        assert_eq!(kinds(&out.packets(group_tx(Uuid::new_v4(), 2320972, alias(2, 910)))), vec![aff, gtx]);
        // The bridge ISSI is registered already; other networks' calls stay.
        assert_eq!(kinds(&out.packets(group_tx(Uuid::new_v4(), 9_000_001, alias(2, 909)))), vec![gtx]);
        assert!(out.packets(group_tx(Uuid::new_v4(), 2320973, alias(3, 909))).is_empty());
        assert_eq!(talkers.lock().unwrap().get(&2320972).map(|g| g.len()), Some(2));
        // Switched off: passed through as before.
        let mut plain = Outbound::new(2, Arc::default());
        assert_eq!(kinds(&plain.packets(group_tx(Uuid::new_v4(), 2320972, alias(2, 909)))), vec![gtx]);
    }

    #[test]
    fn log_line_reads_talker_and_group() {
        let pkt = group_tx(Uuid::new_v4(), 2320972, 909);
        assert_eq!((src(&pkt), dest(&pkt)), (2320972, 909));
        // Short or foreign packets are not logged and do not panic.
        log_crossing("Test", "in", &[protocol::CLASS_CALL_CONTROL, protocol::CALL_GROUP_TX], None);
        log_crossing("Test", "out", &pkt, Some(&pkt));
    }

    #[test]
    fn alias_keeps_network_and_group_apart() {
        assert_eq!(alias(1, 9), 0x0100_0009);
        assert_eq!(split_alias(alias(3, 909)), Some((3, 909)));
        assert_eq!(split_alias(9), None);
        assert_eq!(split_alias(0xFF_FFFF), None);
    }

    fn gtx(pkt: &[u8]) -> (Uuid, u32, u32) {
        (call_id(pkt).unwrap(), u32::from_le_bytes(pkt[18..22].try_into().unwrap()), dest(pkt))
    }

    #[test]
    fn outbound_lets_out_only_calls_on_its_own_network_under_new_ids() {
        let ids: SharedCallIds = Arc::default();
        let mut out = Outbound::new(2, ids.clone());
        let call = Uuid::new_v4();
        // A local group call: stays here.
        assert!(out.translate(group_tx(call, 2320972, 9)).is_none());
        // A call on another network's alias: stays here.
        assert!(out.translate(group_tx(call, 2320972, alias(3, 9))).is_none());
        // Its frames stay too.
        assert!(out.translate(protocol::build_dtmf_frame(&call, 1)).is_none());
        // A call on this network's group 909: out with the real GSSI, talker
        // 1:1, under an identifier of its own there.
        let leg = Uuid::new_v4();
        let (remote, src, dst) = gtx(&out.translate(group_tx(leg, 2320972, alias(2, 909))).unwrap());
        assert_eq!((src, dst), (2320972, 909));
        assert_ne!(remote, leg);
        // A new over keeps that identifier; frames and the end use it too.
        assert_eq!(gtx(&out.translate(group_tx(leg, 2320973, alias(2, 909))).unwrap()).0, remote);
        assert_eq!(call_id(&out.translate(protocol::build_dtmf_frame(&leg, 1)).unwrap()), Some(remote));
        let idle = protocol::build_call_cause(protocol::CALL_GROUP_IDLE, &leg, 0);
        assert_eq!(call_id(&out.translate(idle).unwrap()), Some(remote));
        // After its end nothing of the call goes out any more.
        assert!(out.translate(protocol::build_dtmf_frame(&leg, 1)).is_none());
        // Registrations and services never go out.
        assert!(out.translate(protocol::build_subscriber_message(protocol::SUB_REGISTER, 7, &[])).is_none());
        assert!(out.translate(protocol::build_service(protocol::SERVICE_BTS_INFO, "{}")).is_none());
    }

    #[test]
    fn inbound_lets_in_only_bridged_groups_as_aliases() {
        let ids: SharedCallIds = Arc::default();
        let mut inb = Inbound::new(2, ids.clone());
        let bridged = |g: u32| g == 909;
        let other = Uuid::new_v4();
        assert!(inb.translate(group_tx(other, 1234, 9), bridged).is_none());
        assert!(inb.translate(protocol::build_dtmf_frame(&other, 1), bridged).is_none());
        let call = Uuid::new_v4();
        let (local, src, dst) = gtx(&inb.translate(group_tx(call, 1234, 909), bridged).unwrap());
        assert_eq!((src, dst), (1234, alias(2, 909)));
        assert_ne!(local, call);
        assert_eq!(call_id(&inb.translate(protocol::build_dtmf_frame(&call, 1), bridged).unwrap()), Some(local));
        // Registrations of that network never reach this server's tables.
        assert!(inb.translate(protocol::build_subscriber_message(protocol::SUB_REGISTER, 1234, &[909]), bridged).is_none());
        assert!(inb.translate(protocol::build_call_cause(protocol::CALL_GROUP_IDLE, &call, 0), bridged).is_some());
        assert!(inb.translate(protocol::build_dtmf_frame(&call, 1), bridged).is_none());
    }

    #[test]
    fn an_answer_over_there_comes_back_under_our_call_id() {
        let ids: SharedCallIds = Arc::default();
        let (mut out, mut inb) = (Outbound::new(1, ids.clone()), Inbound::new(1, ids));
        let leg = Uuid::new_v4();
        let remote = gtx(&out.translate(group_tx(leg, 2320972, alias(1, 909))).unwrap()).0;
        // Someone over there answers in that call.
        let back = inb.translate(group_tx(remote, 1234, 909), |_| true).unwrap();
        assert_eq!(gtx(&back), (leg, 1234, alias(1, 909)));
        // The same call coming back as our own id (a loop) is a new call here.
        let looped = inb.translate(group_tx(leg, 2320972, 909), |_| true).unwrap();
        assert_ne!(call_id(&looped), Some(leg));
    }

    fn pool(name: &str, remote: &[(&str, u32)]) -> TalkgroupPoolConfig {
        TalkgroupPoolConfig {
            name: name.into(), enabled: true, both_ways: true, groups: vec![9],
            remote: remote.iter().map(|(n, g)| RemotePoolGroup { network: (*n).into(), gssi: *g }).collect(),
        }
    }

    #[test]
    fn bridged_groups_come_from_switched_on_pools() {
        let mut off = pool("Off", &[("TP", 5)]);
        off.enabled = false;
        let pools = [pool("A", &[("TP", 909), ("Other", 1)]), pool("B", &[("TP", 9)]), off];
        assert_eq!(bridged_groups(&pools, "TP").into_iter().collect::<Vec<_>>(), vec![9, 909]);
        assert_eq!(bridged_groups(&pools, "Other").into_iter().collect::<Vec<_>>(), vec![1]);
    }

    #[test]
    fn affiliation_changes_send_only_the_difference() {
        let have: BTreeSet<u32> = [9, 10].into();
        let want: BTreeSet<u32> = [10, 11].into();
        let msgs = affiliation_changes(5, &have, &want);
        assert_eq!(msgs, vec![
            protocol::build_subscriber_message(protocol::SUB_DEAFFILIATE, 5, &[9]),
            protocol::build_subscriber_message(protocol::SUB_AFFILIATE, 5, &[11]),
        ]);
        assert!(affiliation_changes(0, &have, &want).is_empty());
        assert!(affiliation_changes(5, &want, &want).is_empty());
    }

    fn net(name: &str) -> NetworkConfig {
        NetworkConfig { name: name.into(), remote_host: "tp.example:443".into(), ..Default::default() }
    }

    #[test]
    fn validation_rejects_bad_networks() {
        let ok = MultiNetworkConfig { networks: vec![net("TP")], ..Default::default() };
        assert!(validate(&ok, &[]).is_ok());
        let dup = MultiNetworkConfig { networks: vec![net("TP"), net("tp")], ..Default::default() };
        assert!(validate(&dup, &[]).is_err());
        let mut no_host = net("TP");
        no_host.remote_host.clear();
        assert!(validate(&MultiNetworkConfig { networks: vec![no_host.clone()], ..Default::default() }, &[]).is_err());
        no_host.enabled = false;
        assert!(validate(&MultiNetworkConfig { networks: vec![no_host], ..Default::default() }, &[]).is_ok());
        let mut ua = net("TP");
        ua.user_agent = "TMO-Austria Infra 1.0\r\nX-Evil: 1".into();
        assert!(validate(&MultiNetworkConfig { networks: vec![ua], ..Default::default() }, &[]).is_err());
        let mut no_port = net("TP");
        no_port.remote_host = "brew.oe1sxm.at".into();
        assert!(validate(&MultiNetworkConfig { networks: vec![no_port], ..Default::default() }, &[]).is_err());
        assert!(has_port("brew.example.net:9000") && has_port("[::1]:443"));
        assert!(!has_port("brew.example.net") && !has_port("brew:0") && !has_port("::1") && !has_port("[::1]"));
        let bad_mni = MultiNetworkConfig { mcc: 1024, ..Default::default() };
        assert!(validate(&bad_mni, &[]).is_err());
        // A pool may not refer to a network that is not there.
        assert!(validate(&ok, &[pool("A", &[("Gone", 9)])]).is_err());
        assert!(validate(&ok, &[pool("A", &[("TP", 9)])]).is_ok());
    }

    #[test]
    fn user_agent_is_free_text_with_a_safe_fallback() {
        let mut n = net("TP");
        assert_eq!(user_agent(&n), federation::USER_AGENT);
        n.user_agent = "TMO-Austria Infra 1.0".into();
        assert_eq!(user_agent(&n), "TMO-Austria Infra 1.0");
        n.user_agent = "Wien\nX".into();
        assert_eq!(user_agent(&n), federation::USER_AGENT);
    }

    #[test]
    fn config_file_round_trip_keeps_networks_and_bridges() {
        let mut c = crate::config::Config::default();
        assert!(!c.to_toml_pretty().unwrap().contains("multi_network"), "an unused section is not written");
        c.multi_network.name = "Main".into();
        c.multi_network.mcc = 901;
        c.multi_network.mnc = 9999;
        let mut n = net("TP");
        n.user_agent = "TMO-Austria Infra 1.0".into();
        n.bridge_issi = 9_000_001;
        c.multi_network.networks.push(n);
        c.talkgroup_pools.push(pool("A", &[("TP", 909)]));
        c.talkgroup_pools.push(pool("Local", &[]));
        let text = c.to_toml_pretty().unwrap();
        let back = crate::config::Config::parse(&text).unwrap();
        assert_eq!(back.multi_network, c.multi_network);
        assert_eq!(back.talkgroup_pools, c.talkgroup_pools);
        // An old file without the new keys still loads.
        let old = crate::config::Config::parse("[[talkgroup_pools]]\nname = \"N\"\ngroups = [1, 2]\n").unwrap();
        assert!(old.talkgroup_pools[0].remote.is_empty() && old.multi_network.is_unset());
    }

    #[test]
    fn slots_stay_with_their_network() {
        let links = NetLinks::default();
        let a = MultiNetworkConfig { networks: vec![net("A"), net("B")], ..Default::default() };
        let s1 = links.slots(&a);
        let b = MultiNetworkConfig { networks: vec![net("B"), net("C")], ..Default::default() };
        let s2 = links.slots(&b);
        assert_eq!(s1["B"], s2["B"]);
        assert_ne!(s2["C"], s1["A"]);
        assert!(!s2.contains_key("A"));
    }
}

#[cfg(test)]
mod e2e {
    //! Two servers on loopback: this one with a link to a "TetraPack" server,
    //! 10 Basestations with 25 radios here, one Basestation there.
    use super::*;
    use crate::config::{Config, RemotePoolGroup};
    use futures_util::{SinkExt, StreamExt};
    use std::net::SocketAddr;
    use tokio_tungstenite::tungstenite::Message;

    type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

    async fn serve(cfg: Config) -> (Arc<AppState>, SocketAddr) {
        let state = Arc::new(AppState::new(cfg, "test.toml".into()).0);
        let app = axum::Router::new()
            .route("/brew", axum::routing::get(crate::server::brew_discovery))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await });
        (state, addr)
    }

    fn base_config() -> Config {
        let mut c = Config::default();
        c.storage.enabled = false;
        c.sms_center.enabled = false;
        c.fallback_broadcast_when_no_affiliations = true;
        c
    }

    async fn station(addr: SocketAddr, radios: &[u32], groups: &[u32]) -> Ws {
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/brew")).await.unwrap();
        for &r in radios {
            ws.send(Message::Binary(protocol::build_subscriber_message(protocol::SUB_REGISTER, r, &[]).into())).await.unwrap();
            if !groups.is_empty() {
                ws.send(Message::Binary(protocol::build_subscriber_message(protocol::SUB_AFFILIATE, r, groups).into())).await.unwrap();
            }
        }
        ws
    }

    /// Next GROUP_TX a station gets within `ms`, as (source, destination).
    async fn next_group_tx(ws: &mut Ws, ms: u64) -> Option<(Uuid, u32, u32)> {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(ms);
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            let Ok(Some(Ok(msg))) = tokio::time::timeout(left, ws.next()).await else { return None };
            if let Message::Binary(b) = msg {
                if is_group_tx(&b) {
                    let src = u32::from_le_bytes(b[18..22].try_into().unwrap());
                    return Some((call_id(&b).unwrap(), src, dest(&b)));
                }
            }
        }
    }

    async fn wait_until(what: &str, mut f: impl AsyncFnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !f().await {
            assert!(tokio::time::Instant::now() < deadline, "timed out waiting for {what}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn bridge_pool_carries_calls_both_ways_with_gssi_translation() {
        let (tp, tp_addr) = serve(base_config()).await;
        let mut cfg = base_config();
        cfg.multi_network.name = "Main".into();
        cfg.multi_network.networks.push(NetworkConfig {
            name: "TP".into(), remote_host: tp_addr.to_string(), bridge_issi: 9_000_001,
            user_agent: "TMO-Austria Infra 1.0".into(), reconnect_interval_seconds: 5, ..Default::default()
        });
        cfg.talkgroup_pools.push(TalkgroupPoolConfig {
            name: "Bridge".into(), enabled: true, both_ways: true, groups: vec![9],
            remote: vec![RemotePoolGroup { network: "TP".into(), gssi: 909 }],
        });
        let (main, main_addr) = serve(cfg).await;
        run(main.clone()).await;

        // 10 stations, 25 radios; the even stations have radios on TG 9.
        let mut stations = Vec::new();
        let mut issi = 2_320_001;
        for s in 0..10u32 {
            let n = if s < 5 { 3 } else { 2 };
            let radios: Vec<u32> = (0..n).map(|k| issi + k).collect();
            issi += n;
            let groups: &[u32] = if s % 2 == 0 { &[9] } else { &[2321] };
            stations.push(station(main_addr, &radios, groups).await);
        }
        assert_eq!(issi - 2_320_001, 25);
        let mut far = station(tp_addr, &[1234], &[909]).await;

        // The link is up: the bridge ISSI sits on TG 909 over there, under
        // the chosen connection name.
        wait_until("bridge ISSI on TG 909", async || {
            tp.inner.read().await.subscribers.get(&9_000_001).is_some_and(|s| s.groups.contains(&909))
        }).await;
        wait_until("all radios registered", async || main.inner.read().await.subscribers.len() == 25).await;
        let agents: Vec<String> = tp.inner.read().await.client_agents.values().cloned().collect();
        assert!(agents.iter().any(|a| a == "TMO-Austria Infra 1.0"), "{agents:?}");
        // Nothing of this server's registrations crossed.
        assert!(!tp.inner.read().await.subscribers.contains_key(&2_320_001));

        // A radio on station 0 talks on TG 9: heard on TG 909 over there,
        // talker ISSI 1:1.
        let call = Uuid::new_v4();
        stations[0].send(Message::Binary(protocol::build_group_tx(&call, 2_320_001, 9, 0).into())).await.unwrap();
        let (far_call, src, dst) = next_group_tx(&mut far, 2000).await.expect("call reaches the other network");
        assert_eq!((src, dst), (2_320_001, 909));
        // Like a Basestation, the link registered the talker there first
        // and affiliated it to the group.
        assert!(tp.inner.read().await.subscribers.get(&2_320_001).is_some_and(|s| s.groups.contains(&909)));
        // The radio over there answers in that call: heard on TG 9 here,
        // by the first talker's station too.
        far.send(Message::Binary(protocol::build_group_tx(&far_call, 1234, 909, 0).into())).await.unwrap();
        let (id0, src, dst) = next_group_tx(&mut stations[0], 2000).await.expect("answer reaches the first talker");
        assert_eq!((id0, src, dst), (call, 1234, 9));
        let first = next_group_tx(&mut stations[2], 2000).await.expect("call reaches TG 9");
        assert_eq!((first.1, first.2), (2_320_001, 9));
        let (_, src, dst) = next_group_tx(&mut stations[2], 2000).await.expect("answer reaches TG 9");
        assert_eq!((src, dst), (1234, 9));
        // Its call ends over there: the call ends here too (no hanging floor).
        far.send(Message::Binary(protocol::build_call_cause(protocol::CALL_GROUP_IDLE, &far_call, 0).into())).await.unwrap();
        wait_until("call ended here", async || main.inner.read().await.calls.is_empty()).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        for st in stations.iter_mut() { while next_group_tx(st, 50).await.is_some() {} }

        // A radio over there talks on TG 909: heard on TG 9 on the stations
        // with TG 9, nowhere else, and no station ever sees an alias.
        let back = Uuid::new_v4();
        far.send(Message::Binary(protocol::build_group_tx(&back, 1234, 909, 0).into())).await.unwrap();
        for (s, st) in stations.iter_mut().enumerate() {
            let got = next_group_tx(st, if s % 2 == 0 { 2000 } else { 300 }).await;
            if s % 2 == 0 {
                let (_, src, dst) = got.unwrap_or_else(|| panic!("station {s} hears the call"));
                assert_eq!((src, dst), (1234, 9));
            } else {
                assert!(got.is_none(), "station {s} has no TG 9 radio: {got:?}");
            }
        }
        // A radio here answers in the call it hears: heard over there.
        let leg_here = {
            let inner = main.inner.read().await;
            *inner.pools.runs.values().next().expect("pool call running").legs.values().next().unwrap()
        };
        stations[2].send(Message::Binary(protocol::build_group_tx(&leg_here, 2_320_007, 9, 0).into())).await.unwrap();
        let (id_far, src, dst) = next_group_tx(&mut far, 2000).await.expect("answer reaches the other network");
        assert_eq!((id_far, src, dst), (back, 2_320_007, 909));
        // The answering station ends its call: over there the call ends too,
        // so the floor of TG 909 is free again.
        stations[2].send(Message::Binary(protocol::build_call_cause(protocol::CALL_GROUP_IDLE, &leg_here, 0).into())).await.unwrap();
        wait_until("call ended over there", async || tp.inner.read().await.calls.is_empty()).await;
        wait_until("call ended here", async || main.inner.read().await.calls.is_empty()).await;

        tokio::time::sleep(Duration::from_millis(300)).await;
        for st in stations.iter_mut() { while next_group_tx(st, 50).await.is_some() {} }
        while next_group_tx(&mut far, 50).await.is_some() {}
        // A station cannot send into the other network by itself.
        let sneaky = Uuid::new_v4();
        stations[1].send(Message::Binary(protocol::build_group_tx(&sneaky, 2_320_004, alias(1, 909), 0).into())).await.unwrap();
        assert!(next_group_tx(&mut far, 400).await.is_none());
        // Nor reach TG 9 through the pool as if it came from there.
        assert!(next_group_tx(&mut stations[0], 300).await.is_none());

        // Renaming the connection reconnects the link; the old link going
        // down does not mark the new one as down.
        let mut renamed = main.networks.read().unwrap().clone();
        renamed.networks[0].user_agent = "TMO-Austria Infra 2.0".into();
        if let Ok(mut live) = main.networks.write() { *live = renamed; }
        reconcile(&main);
        wait_until("new connection name over there", async || {
            tp.inner.read().await.client_agents.values().any(|a| a == "TMO-Austria Infra 2.0")
        }).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(snapshot(&main).networks[0].connected, "the new link shows as connected");

        // The main switch off ends every link, on brings them back.
        let mut main_off = main.networks.read().unwrap().clone();
        main_off.enabled = false;
        if let Ok(mut live) = main.networks.write() { *live = main_off; }
        reconcile(&main);
        wait_until("main switch off: link gone", async || main.inner.read().await.netlinks.is_empty()).await;
        wait_until("main switch off: bridge gone there", async || !tp.inner.read().await.subscribers.contains_key(&9_000_001)).await;
        wait_until("main switch off: talkers gone there", async || !tp.inner.read().await.subscribers.contains_key(&2_320_001)).await;
        assert!(!snapshot(&main).networks[0].connected);
        if let Ok(mut live) = main.networks.write() { live.enabled = true; }
        reconcile(&main);
        wait_until("main switch on: link back", async || !main.inner.read().await.netlinks.is_empty()).await;
        wait_until("main switch on: bridge back there", async || tp.inner.read().await.subscribers.contains_key(&9_000_001)).await;

        // Switching the network off ends the link and deregisters the bridge.
        let mut off = main.networks.read().unwrap().clone();
        off.networks[0].enabled = false;
        if let Ok(mut live) = main.networks.write() { *live = off; }
        reconcile(&main);
        wait_until("bridge ISSI gone over there", async || !tp.inner.read().await.subscribers.contains_key(&9_000_001)).await;
        wait_until("link gone here", async || main.inner.read().await.netlinks.is_empty()).await;
    }

    #[tokio::test]
    async fn only_the_link_may_start_a_call_on_its_networks_group() {
        let state = AppState::for_test();
        if let Ok(mut n) = state.networks.write() { n.networks.push(NetworkConfig { name: "TP".into(), ..Default::default() }); }
        let slot = slots(&state)["TP"];
        if let Ok(mut p) = state.pools.write() {
            p.push(TalkgroupPoolConfig { name: "B".into(), enabled: true, both_ways: true, groups: vec![9],
                remote: vec![RemotePoolGroup { network: "TP".into(), gssi: 909 }] });
        }
        let (bs, link) = (Uuid::new_v4(), Uuid::new_v4());
        {
            let mut inner = state.inner.write().await;
            for (id, mode) in [(bs, ClientMode::Basestation), (link, ClientMode::Network)] {
                let (tx, rx) = mpsc::unbounded_channel();
                std::mem::forget(rx);
                inner.clients.insert(id, Client { tx, mode, version: ConnVersion::V1, version_announced: false, remote_addr: None, connected_at_ms: 0, username: None });
            }
            inner.netlinks.insert(slot, link);
        }
        let target = alias(slot, 909);
        crate::router::handle_packet(state.clone(), bs, protocol::build_group_tx(&Uuid::new_v4(), 7, target, 0)).await;
        assert!(state.inner.read().await.calls.is_empty(), "a station may not start a call on another network's group");
        crate::router::handle_packet(state.clone(), link, protocol::build_group_tx(&Uuid::new_v4(), 7, target, 0)).await;
        assert!(state.inner.read().await.calls.values().any(|c| c.destination == target), "the link may");
    }

    fn voice(id: &Uuid) -> Vec<u8> {
        protocol::build_traffic_frame(id, &[1; protocol::ACELP_CODED_FRAME_BYTES], &[2; protocol::ACELP_CODED_FRAME_BYTES])
    }

    /// Call id of the next voice frame a station gets within `ms`.
    async fn next_voice(ws: &mut Ws, ms: u64) -> Option<Uuid> {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(ms);
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            let Ok(Some(Ok(msg))) = tokio::time::timeout(left, ws.next()).await else { return None };
            if let Message::Binary(b) = msg {
                if b.first() == Some(&protocol::CLASS_FRAME) && b.get(1) == Some(&protocol::FRAME_TRAFFIC_CHANNEL) { return call_id(&b); }
            }
        }
    }

    /// Next CALL_GROUP_IDLE a station gets within `ms`.
    async fn next_idle(ws: &mut Ws, ms: u64) -> Option<Uuid> {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(ms);
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            let Ok(Some(Ok(msg))) = tokio::time::timeout(left, ws.next()).await else { return None };
            if let Message::Binary(b) = msg {
                if is_group_idle(&b) { return call_id(&b); }
            }
        }
    }

    /// The setup of the first field test: the server linked to itself.
    /// Nothing may hang: every call ends when its station ends it.
    #[tokio::test]
    async fn loop_to_the_same_server_ends_every_call() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut cfg = base_config();
        cfg.multi_network.networks.push(NetworkConfig {
            // A link to this same server must not register the talkers:
            // that would move them off their own station here.
            name: "Loop".into(), remote_host: addr.to_string(), bridge_issi: 9_000_001, register_talkers: false, ..Default::default()
        });
        cfg.talkgroup_pools.push(TalkgroupPoolConfig {
            name: "TestLink".into(), enabled: true, both_ways: true, groups: vec![9],
            remote: vec![RemotePoolGroup { network: "Loop".into(), gssi: 909 }],
        });
        let state = Arc::new(AppState::new(cfg, "test.toml".into()).0);
        let app = axum::Router::new()
            .route("/brew", axum::routing::get(crate::server::brew_discovery))
            .with_state(state.clone());
        tokio::spawn(async move { axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await });
        run(state.clone()).await;
        let mut a = station(addr, &[100], &[9]).await;
        let mut b = station(addr, &[200], &[909]).await;
        wait_until("bridge on TG 909", async || {
            state.inner.read().await.subscribers.get(&9_000_001).is_some_and(|s| s.groups.contains(&909))
        }).await;
        wait_until("radios", async || state.inner.read().await.subscribers.len() == 3).await;

        // TG 9 -> TG 909 and the end.
        let call = Uuid::new_v4();
        a.send(Message::Binary(protocol::build_group_tx(&call, 100, 9, 0).into())).await.unwrap();
        let (b_call, src, dst) = next_group_tx(&mut b, 2000).await.expect("909 hears TG 9");
        assert_eq!((src, dst), (100, 909));
        // Voice crosses, under the call id 909 knows.
        a.send(Message::Binary(voice(&call).into())).await.unwrap();
        assert_eq!(next_voice(&mut b, 2000).await, Some(b_call), "909 hears the voice");
        a.send(Message::Binary(protocol::build_call_cause(protocol::CALL_GROUP_IDLE, &call, 0).into())).await.unwrap();
        assert!(next_idle(&mut b, 2000).await.is_some());
        wait_until("no call left", async || state.inner.read().await.calls.is_empty()).await;

        // TG 909 -> TG 9, TG 9 answers in the call, the answer ends it.
        let x = Uuid::new_v4();
        b.send(Message::Binary(protocol::build_group_tx(&x, 200, 909, 0).into())).await.unwrap();
        let (leg, src, dst) = next_group_tx(&mut a, 2000).await.expect("TG 9 hears 909");
        assert_eq!((src, dst), (200, 9));
        b.send(Message::Binary(voice(&x).into())).await.unwrap();
        assert_eq!(next_voice(&mut a, 2000).await, Some(leg), "TG 9 hears the voice");
        a.send(Message::Binary(protocol::build_group_tx(&leg, 100, 9, 0).into())).await.unwrap();
        let (id, src, dst) = next_group_tx(&mut b, 2000).await.expect("909 hears the answer");
        assert_eq!((id, src, dst), (x, 100, 909));
        a.send(Message::Binary(voice(&leg).into())).await.unwrap();
        assert_eq!(next_voice(&mut b, 2000).await, Some(x), "909 hears the answer's voice");
        a.send(Message::Binary(protocol::build_call_cause(protocol::CALL_GROUP_IDLE, &leg, 0).into())).await.unwrap();
        assert_eq!(next_idle(&mut b, 2000).await, Some(x));
        wait_until("no call left after the answer", async || state.inner.read().await.calls.is_empty()).await;
    }
}
