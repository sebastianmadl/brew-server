//! Fleet Control phase 8a: key management, the server as authentication
//! centre (AuC).
//!
//! The server keeps each radio's authentication key K (EN 300 392-7 clause
//! 4.1.1: K is never visible outside the authentication centre). A
//! Basestation that authenticates on the server's behalf only gets
//! authentication data sets (RS, KS, KS'): KS = TA11(K, RS) for the
//! authentication of the radio (clause 4.1.2), KS' = TA21(K, RS) for the
//! authentication of the infrastructure (clause 4.1.3). RAND1 is drawn by the
//! Basestation at every authentication, so nothing it holds is a precomputed
//! response. Each set is used once.
//!
//! The station keeps a stock of sets per radio so it can authenticate while
//! the server is unreachable; the server tops the stock up whenever a set is
//! used, for the radios that were registered at that station or at one of
//! its neighbours in the last `days` days. A radio the station has no set for
//! is asked for on demand (`AuthVectorRequest`).
//!
//! K is only written (provisioned), never read back: the dashboard shows the
//! check value SHA-256(K)[:8]. A K generated here is shown once, to load it
//! into the radio with the CPS / KVL.
//!
//! Files: `<storage path>.fleet-keys` (AES-256-GCM, JSON inside) and the
//! master key (`[keys] master_key_file`, default `<storage path>.fleet-master-key`,
//! 64 hex digits, created with mode 600 when there is no store yet).

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Station capability: understands the key management commands.
pub const CAP_KEYS: &str = "keys-v1";
/// Export file of a FlowStation's local keys.
pub const EXPORT_FORMAT: &str = "bost-keys-v1";
/// Authentication data sets sent in one `AuthVectors`.
const MAX_SETS_PER_MESSAGE: u32 = 50;

// ───────────────────────────── small crypto helpers ─────────────────────────────

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| s.get(i..i + 2).and_then(|p| u8::from_str_radix(p, 16).ok())).collect()
}

pub(crate) fn unhex_n<const N: usize>(s: &str) -> Option<[u8; N]> {
    unhex(s)?.try_into().ok()
}

/// K as typed or pasted: spaces, `:`, `-`, tabs and line breaks are dropped,
/// then exactly 32 hex digits.
pub fn parse_k(s: &str) -> Result<[u8; 16], String> {
    let digits: String = s.chars().filter(|c| !matches!(c, ' ' | ':' | '-' | '\t' | '\r' | '\n')).collect();
    if digits.len() != 32 {
        return Err(format!("K must be 32 hex digits (128 bit), got {}", digits.len()));
    }
    unhex_n::<16>(&digits).ok_or_else(|| "K may only contain hex digits 0-9 and a-f".to_string())
}

/// The check value shown instead of K: SHA-256(K), first 8 bytes, as hex.
pub fn check_value(k: &[u8; 16]) -> String {
    let d = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, k);
    hex(&d.as_ref()[..8]).to_uppercase()
}

/// K in groups of four for reading it out.
pub fn grouped(k: &[u8; 16]) -> String {
    let h = hex(k).to_uppercase();
    h.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).unwrap_or("")).collect::<Vec<_>>().join(" ")
}

pub(crate) fn random<const N: usize>() -> [u8; N] {
    use aws_lc_rs::rand::SecureRandom;
    let mut b = [0u8; N];
    aws_lc_rs::rand::SystemRandom::new().fill(&mut b).expect("system random");
    b
}

fn seal(key: &[u8; 32], plain: &[u8]) -> ([u8; 12], Vec<u8>) {
    use aws_lc_rs::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
    let nonce = random::<12>();
    let k = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, key).expect("32-byte key"));
    let mut buf = plain.to_vec();
    k.seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut buf).expect("seal");
    (nonce, buf)
}

fn open(key: &[u8; 32], nonce: &[u8; 12], data: &[u8]) -> Option<Vec<u8>> {
    use aws_lc_rs::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
    let k = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, key).ok()?);
    let mut buf = data.to_vec();
    let plain = k.open_in_place(Nonce::assume_unique_for_key(*nonce), Aad::empty(), &mut buf).ok()?;
    Some(plain.to_vec())
}

fn pbkdf2(password: &str, salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut out = [0u8; 32];
    let n = std::num::NonZeroU32::new(iterations.max(1)).expect("non-zero");
    aws_lc_rs::pbkdf2::derive(aws_lc_rs::pbkdf2::PBKDF2_HMAC_SHA256, n, salt, password.as_bytes(), &mut out);
    out
}

/// An export file of a FlowStation's local keys.
#[derive(Debug, Serialize, Deserialize)]
pub struct ExportFile {
    pub format: String,
    pub kdf: String,
    pub iterations: u32,
    pub salt: String,
    pub nonce: String,
    pub data: String,
}

/// Opens an export file with its password; the text inside is `ISSI;K` per line.
pub fn open_export(file: &str, password: &str) -> Result<String, String> {
    let f: ExportFile = serde_json::from_str(file.trim()).map_err(|_| "not a FlowStation key export file".to_string())?;
    if f.format != EXPORT_FORMAT || f.kdf != "pbkdf2-hmac-sha256" {
        return Err(format!("unknown export format {} / {}", f.format, f.kdf));
    }
    if !(10_000..=10_000_000).contains(&f.iterations) {
        return Err("export file: iteration count out of range".into());
    }
    let salt = unhex(&f.salt).ok_or("export file: bad salt")?;
    let nonce = unhex_n::<12>(&f.nonce).ok_or("export file: bad nonce")?;
    let data = unhex(&f.data).ok_or("export file: bad data")?;
    let key = pbkdf2(password, &salt, f.iterations);
    let plain = open(&key, &nonce, &data).ok_or("wrong password or damaged file")?;
    String::from_utf8(plain).map_err(|_| "export file: not text inside".to_string())
}

/// Writes an export file (the FlowStation side does the same; used by tests).
#[cfg(test)]
pub fn make_export(lines: &str, password: &str, iterations: u32) -> String {
    let salt = random::<16>();
    let key = pbkdf2(password, &salt, iterations);
    let (nonce, data) = seal(&key, lines.as_bytes());
    serde_json::to_string(&ExportFile {
        format: EXPORT_FORMAT.into(),
        kdf: "pbkdf2-hmac-sha256".into(),
        iterations,
        salt: hex(&salt),
        nonce: hex(&nonce),
        data: hex(&data),
    })
    .expect("serializes")
}

// ───────────────────────────── authentication data ─────────────────────────────

/// One authentication data set as it goes to a station (hex). `exp_ms`: the station drops the
/// set unused after this time (Unix ms); `rev`: the K revision it was made from, a station drops
/// sets of an older revision. Older stations ignore both.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthVector {
    pub rs: String,
    pub ks: String,
    pub ks_p: String,
    #[serde(default)]
    pub exp_ms: u64,
    #[serde(default)]
    pub rev: u32,
}

/// A fresh set for `k`: random RS, KS = TA11(K, RS), KS' = TA21(K, RS).
pub fn new_vector(k: &[u8; 16]) -> AuthVector {
    let rs = random::<10>();
    AuthVector { rs: hex(&rs), ks: hex(&crate::taa1::ta11(k, &rs)), ks_p: hex(&crate::taa1::ta21(k, &rs)), exp_ms: 0, rev: 0 }
}

const DAY_MS: u64 = 24 * 3600 * 1000;

/// Number of sets a station holds per radio, as it reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssiCount {
    pub issi: u32,
    pub count: u32,
}

// ───────────────────────────── store ─────────────────────────────

/// Settings (Server settings, admins).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Sets per radio and station (1–50).
    pub stock: u32,
    /// Stock for radios registered at the station or a neighbour in the last
    /// `days` days (1–365).
    pub days: u32,
    /// Days a set stays valid at a station (1–90); after that the station drops it unused.
    pub set_days: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self { stock: 10, days: 30, set_days: 7 }
    }
}

/// The last authentication of a radio reported by a station.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LastAuth {
    /// Station (control login).
    pub station: String,
    pub at_ms: u64,
    pub ok: bool,
    pub mutual: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Sub {
    /// K as hex (only inside the encrypted store).
    k: String,
    kcv: String,
    /// `manual`, `generated`, `import`, `flowstation`.
    source: String,
    by: Option<String>,
    set_ms: u64,
    #[serde(default)]
    model: String,
    #[serde(default)]
    last_auth: Option<LastAuth>,
    /// Counts the K changes of this radio (sent with each data set).
    #[serde(default)]
    rev: u32,
}

/// Where a radio was registered last (Brew login of the station).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Seen {
    station: String,
    at_ms: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Data {
    /// K per network and radio, keyed `MCC-MNC/ISSI` (the same ISSI in two networks are two
    /// radios). `*/ISSI`: kept from before keys were per network, used in any network until
    /// written again for a network.
    #[serde(default)]
    subs: BTreeMap<String, Sub>,
    #[serde(default)]
    settings: Settings,
    #[serde(default)]
    seen: BTreeMap<u32, Seen>,
    /// Class 2 SCKs per network, device profiles (`fleet_sck`).
    #[serde(default)]
    sck: crate::fleet_sck::SckData,
    /// Class 3 CCKs per network and location area (`fleet_cck`).
    #[serde(default)]
    cck: crate::fleet_cck::CckData,
}

#[derive(Serialize, Deserialize)]
struct StoreFile {
    v: u32,
    nonce: String,
    data: String,
}

/// One radio as the dashboard lists it (never K).
#[derive(Debug, Clone, Serialize)]
pub struct SubView {
    /// Network (`MCC-MNC`, or `*` for a K from before keys were per network).
    pub net: String,
    pub issi: u32,
    pub kcv: String,
    pub source: String,
    pub by: Option<String>,
    pub set_ms: u64,
    pub model: String,
    pub last_auth: Option<LastAuth>,
    /// Last station it was registered at (Brew login) and when.
    pub seen: Option<(String, u64)>,
    /// Sets per station (control login).
    pub stock: BTreeMap<String, u32>,
}

/// What an import did.
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct ImportReport {
    pub added: u32,
    pub changed: u32,
    pub unchanged: u32,
    /// "line N: …"; nothing is imported when there is one.
    pub errors: Vec<String>,
    /// Check value per imported radio, to compare with the records.
    pub kcvs: Vec<(u32, String)>,
}

pub struct KeyStore {
    path: Option<PathBuf>,
    master: Option<[u8; 32]>,
    /// Why the key functions are off (no storage, missing master key, …).
    problem: Option<String>,
    data: Data,
    /// Sets each station holds per radio: as it last reported, plus what was
    /// sent since, minus what it used.
    stock: HashMap<String, BTreeMap<u32, u32>>,
    /// What the stations report about their SCK, delivery attempts (`fleet_sck`).
    pub(crate) sck_rt: crate::fleet_sck::SckRuntime,
    /// What the stations report about their CCK (`fleet_cck`).
    pub(crate) cck_rt: crate::fleet_cck::CckRuntime,
    pub(crate) dck_rt: crate::fleet_dck::DckRuntime,
    pub(crate) fallback_rt: crate::fleet_fallback::FallbackRuntime,
}

impl KeyStore {
    /// Memory only (tests, storage off): keys work until the restart.
    pub fn memory() -> Self {
        Self { path: None, master: Some(random::<32>()), problem: None, data: Data::default(), stock: HashMap::new(), sck_rt: Default::default(), cck_rt: Default::default(), dck_rt: Default::default(), fallback_rt: Default::default() }
    }

    /// The store next to the history log; `None`: storage is off.
    pub fn load(storage: Option<&Path>, master_file: Option<&Path>) -> Self {
        let Some(storage) = storage else {
            let mut s = Self::memory();
            s.master = None;
            s.problem = Some("key management needs [storage] enabled".into());
            return s;
        };
        let with = |ext: &str| {
            let mut p = storage.as_os_str().to_owned();
            p.push(ext);
            PathBuf::from(p)
        };
        let path = with(".fleet-keys");
        let master_path = master_file.map(Path::to_path_buf).unwrap_or_else(|| with(".fleet-master-key"));
        let mut store = Self { path: Some(path.clone()), master: None, problem: None, data: Data::default(), stock: HashMap::new(), sck_rt: Default::default(), cck_rt: Default::default(), dck_rt: Default::default(), fallback_rt: Default::default() };
        let master = match std::fs::read_to_string(&master_path) {
            Ok(text) => match unhex_n::<32>(text.trim()) {
                Some(m) => Some(m),
                None => {
                    store.problem = Some(format!("master key file {} is not 64 hex digits", master_path.display()));
                    return store;
                }
            },
            Err(_) if path.exists() => {
                // Never make a new master key over an existing store: it could not be read again.
                store.problem = Some(format!("master key file {} is missing; the key store cannot be read", master_path.display()));
                return store;
            }
            Err(_) => match write_master(&master_path) {
                Ok(m) => Some(m),
                Err(e) => {
                    store.problem = Some(format!("cannot create the master key file {}: {e}", master_path.display()));
                    return store;
                }
            },
        };
        store.master = master;
        if path.exists() {
            match std::fs::read(&path).ok().and_then(|b| read_store(&b, store.master.as_ref()?)) {
                Some(d) => store.data = d,
                None => {
                    store.master = None;
                    store.problem = Some(format!("key store {} cannot be decrypted with the master key", path.display()));
                }
            }
        }
        store
    }

    /// `None`: key functions available; else why not.
    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    pub(crate) fn ready(&self) -> Result<(), String> {
        match &self.problem {
            Some(p) => Err(p.clone()),
            None => Ok(()),
        }
    }

    pub(crate) fn save(&self) {
        let (Some(path), Some(master)) = (&self.path, &self.master) else { return };
        let Ok(plain) = serde_json::to_vec(&self.data) else { return };
        let (nonce, data) = seal(master, &plain);
        let Ok(bytes) = serde_json::to_vec(&StoreFile { v: 1, nonce: hex(&nonce), data: hex(&data) }) else { return };
        let tmp = path.with_extension("fleet-keys.tmp");
        let res = write_private(&tmp, &bytes).and_then(|_| std::fs::rename(&tmp, path)).and_then(|_| sync_dir(path));
        if let Err(e) = res {
            tracing::warn!(error = %e, path = %path.display(), "cannot write the key store");
        }
    }

    pub fn settings(&self) -> &Settings {
        &self.data.settings
    }

    pub fn set_settings(&mut self, s: Settings) -> Result<(), String> {
        if !(1..=50).contains(&s.stock) {
            return Err("stock must be 1-50".into());
        }
        if !(1..=365).contains(&s.days) {
            return Err("days must be 1-365".into());
        }
        if !(1..=90).contains(&s.set_days) {
            return Err("validity of a set must be 1-90 days".into());
        }
        self.data.settings = s;
        self.save();
        Ok(())
    }

    /// The entry of `issi` usable in `net`: its own, else one kept from before keys were per
    /// network.
    fn sub_of(&self, net: &str, issi: u32) -> Option<&Sub> {
        self.data.subs.get(&sub_key(net, issi)).or_else(|| self.data.subs.get(&sub_key(ANY_NET, issi)))
    }

    pub fn has_k(&self, net: &str, issi: u32) -> bool {
        self.sub_of(net, issi).is_some()
    }

    /// K for `issi` in some network (the radio's location is tracked once).
    fn has_k_anywhere(&self, issi: u32) -> bool {
        self.data.subs.keys().any(|key| split_key(key).is_some_and(|(_, i)| i == issi))
    }

    /// Radios with a K usable in `net`.
    pub fn issis(&self, net: &str) -> Vec<u32> {
        let mut out: Vec<u32> = self
            .data
            .subs
            .keys()
            .filter_map(|key| split_key(key))
            .filter(|(n, _)| *n == net || *n == ANY_NET)
            .map(|(_, i)| i)
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// How often the K of `issi` in `net` changed.
    pub fn rev_of(&self, net: &str, issi: u32) -> Option<u32> {
        self.sub_of(net, issi).map(|s| s.rev)
    }

    pub(crate) fn k_of(&self, net: &str, issi: u32) -> Option<[u8; 16]> {
        self.sub_of(net, issi).and_then(|s| unhex_n::<16>(&s.k))
    }

    /// The networks that hold a K.
    pub fn nets(&self) -> Vec<String> {
        let mut out: Vec<String> = self.data.subs.keys().filter_map(|k| split_key(k)).map(|(n, _)| n.to_owned()).collect();
        out.dedup();
        out
    }

    /// Writes K for `issi` in `net` (`MCC-MNC`). Returns the check value and whether K changed.
    #[allow(clippy::too_many_arguments)]
    pub fn set_k(&mut self, net: &str, issi: u32, k: &[u8; 16], source: &str, by: Option<&str>, now_ms: u64) -> Result<(String, bool), String> {
        self.ready()?;
        crate::fleet_sck::check_net(net)?;
        check_issi(issi)?;
        let kcv = check_value(k);
        let key = sub_key(net, issi);
        let changed = self.data.subs.get(&key).is_none_or(|s| s.k != hex(k));
        if changed {
            let old = self.sub_of(net, issi);
            let model = old.map(|s| s.model.clone()).unwrap_or_default();
            let rev = old.map_or(0, |s| s.rev.wrapping_add(1));
            self.data.subs.insert(key, Sub { k: hex(k), kcv: kcv.clone(), source: source.into(), by: by.map(str::to_owned), set_ms: now_ms, model, last_auth: None, rev });
            // Sets made from the old K are worthless now.
            for st in self.stock.values_mut() {
                st.remove(&issi);
            }
            self.save();
        }
        Ok((kcv, changed))
    }

    /// A new random K for `issi` (shown once by the caller).
    pub fn generate(&mut self, net: &str, issi: u32, by: Option<&str>, now_ms: u64) -> Result<([u8; 16], String), String> {
        let k = random::<16>();
        let (kcv, _) = self.set_k(net, issi, &k, "generated", by, now_ms)?;
        Ok((k, kcv))
    }

    /// Removes the K of `issi` in `net` (`*`: the one kept from before keys were per network).
    pub fn delete(&mut self, net: &str, issi: u32) -> Result<bool, String> {
        self.ready()?;
        let had = self.data.subs.remove(&sub_key(net, issi)).is_some();
        if had {
            for st in self.stock.values_mut() {
                st.remove(&issi);
            }
            self.save();
        }
        Ok(had)
    }

    pub fn set_model(&mut self, net: &str, issi: u32, model: &str) -> Result<(), String> {
        self.ready()?;
        let key = if self.data.subs.contains_key(&sub_key(net, issi)) { sub_key(net, issi) } else { sub_key(ANY_NET, issi) };
        let s = self.data.subs.get_mut(&key).ok_or("no K for this ISSI")?;
        s.model = model.chars().take(40).collect();
        self.save();
        Ok(())
    }

    /// `ISSI;K` per line (also `,` or tab; `#` comments and a header line
    /// are skipped). Nothing is imported when a line is wrong.
    pub fn import(&mut self, net: &str, text: &str, source: &str, by: Option<&str>, now_ms: u64) -> Result<ImportReport, String> {
        self.ready()?;
        crate::fleet_sck::check_net(net)?;
        let mut report = ImportReport::default();
        let mut rows: Vec<(u32, [u8; 16])> = Vec::new();
        let mut seen = HashSet::new();
        for (n, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.splitn(2, [';', ',', '\t']);
            let (a, b) = (parts.next().unwrap_or("").trim(), parts.next().unwrap_or("").trim());
            if n == 0 && a.parse::<u32>().is_err() && b.parse::<u32>().is_err() && unhex(b).is_none() {
                continue; // header
            }
            let issi = match a.parse::<u32>() {
                Ok(i) if check_issi(i).is_ok() => i,
                _ => {
                    report.errors.push(format!("line {}: \"{a}\" is not an ISSI (1-16777215)", n + 1));
                    continue;
                }
            };
            match parse_k(b) {
                Ok(k) => {
                    if !seen.insert(issi) {
                        report.errors.push(format!("line {}: ISSI {issi} is listed twice", n + 1));
                        continue;
                    }
                    rows.push((issi, k));
                }
                Err(e) => report.errors.push(format!("line {}: {e}", n + 1)),
            }
        }
        if rows.is_empty() && report.errors.is_empty() {
            report.errors.push("no keys found (expected ISSI;K per line)".into());
        }
        if !report.errors.is_empty() {
            return Ok(report);
        }
        for (issi, k) in rows {
            let existed = self.data.subs.contains_key(&sub_key(net, issi));
            let (kcv, changed) = self.set_k(net, issi, &k, source, by, now_ms)?;
            match (existed, changed) {
                (false, _) => report.added += 1,
                (true, true) => report.changed += 1,
                (true, false) => report.unchanged += 1,
            }
            report.kcvs.push((issi, kcv));
        }
        Ok(report)
    }

    /// `n` fresh sets for `issi` (none without K), valid for `set_days` from `now_ms`, with the
    /// K revision.
    pub fn vectors(&self, net: &str, issi: u32, n: u32, now_ms: u64) -> Vec<AuthVector> {
        let Some(k) = self.k_of(net, issi) else { return Vec::new() };
        let exp_ms = now_ms + u64::from(self.data.settings.set_days) * DAY_MS;
        let rev = self.rev_of(net, issi).unwrap_or(0);
        (0..n.min(MAX_SETS_PER_MESSAGE)).map(|_| AuthVector { exp_ms, rev, ..new_vector(&k) }).collect()
    }

    /// The station reported its whole stock.
    pub fn set_stock(&mut self, station: &str, net: &str, counts: &[IssiCount]) {
        let map = counts.iter().filter(|c| self.has_k(net, c.issi)).map(|c| (c.issi, c.count)).collect();
        self.stock.insert(station.to_owned(), map);
    }

    pub fn add_stock(&mut self, station: &str, issi: u32, n: u32) {
        *self.stock.entry(station.to_owned()).or_default().entry(issi).or_default() += n;
    }

    pub fn stock_of(&self, station: &str, issi: u32) -> u32 {
        self.stock.get(station).and_then(|m| m.get(&issi)).copied().unwrap_or(0)
    }

    pub fn forget_station(&mut self, station: &str) {
        self.stock.remove(station);
    }

    /// An authentication the station ran (one set used).
    #[allow(clippy::too_many_arguments)]
    pub fn note_auth(&mut self, station: &str, net: &str, brew_login: Option<&str>, issi: u32, ok: bool, mutual: bool, left: Option<u32>, now_ms: u64) {
        match left {
            Some(n) => {
                self.stock.entry(station.to_owned()).or_default().insert(issi, n);
            }
            None => {
                if let Some(c) = self.stock.get_mut(station).and_then(|m| m.get_mut(&issi)) {
                    *c = c.saturating_sub(1);
                }
            }
        }
        let key = if self.data.subs.contains_key(&sub_key(net, issi)) { sub_key(net, issi) } else { sub_key(ANY_NET, issi) };
        if let Some(s) = self.data.subs.get_mut(&key) {
            s.last_auth = Some(LastAuth { station: station.to_owned(), at_ms: now_ms, ok, mutual });
        }
        if let Some(b) = brew_login {
            self.note_seen(issi, b, now_ms);
        }
        self.save();
    }

    /// Radio `issi` is registered at the station with Brew login `station`.
    /// Returns true when that is new (saved then).
    pub fn note_seen(&mut self, issi: u32, station: &str, now_ms: u64) -> bool {
        if !self.has_k_anywhere(issi) {
            return false;
        }
        let e = self.data.seen.get(&issi);
        // Same place: refresh the time only every hour (no write per tick).
        if e.is_some_and(|e| e.station == station && now_ms.saturating_sub(e.at_ms) < 3_600_000) {
            return false;
        }
        let moved = e.is_none_or(|e| e.station != station);
        self.data.seen.insert(issi, Seen { station: station.to_owned(), at_ms: now_ms });
        self.save();
        moved
    }

    /// The radios a station (Brew login `brew`) keeps a stock for: K here and
    /// registered at it or at one of `neighbours` in the last `days` days.
    pub fn targets(&self, net: &str, brew: Option<&str>, neighbours: &HashSet<String>, now_ms: u64) -> Vec<u32> {
        let keep = self.data.settings.days as u64 * 86_400_000;
        self.data
            .seen
            .iter()
            .filter(|(issi, s)| {
                self.has_k(net, **issi) && now_ms.saturating_sub(s.at_ms) <= keep && (brew == Some(s.station.as_str()) || neighbours.contains(&s.station))
            })
            .map(|(i, _)| *i)
            .collect()
    }

    pub(crate) fn sck(&self) -> &crate::fleet_sck::SckData {
        &self.data.sck
    }

    pub(crate) fn sck_mut(&mut self) -> &mut crate::fleet_sck::SckData {
        &mut self.data.sck
    }

    pub(crate) fn cck(&self) -> &crate::fleet_cck::CckData {
        &self.data.cck
    }

    pub(crate) fn cck_mut(&mut self) -> &mut crate::fleet_cck::CckData {
        &mut self.data.cck
    }

    /// Where the radio was registered last (Brew login) and when.
    pub(crate) fn seen_of(&self, issi: u32) -> Option<(&str, u64)> {
        self.data.seen.get(&issi).map(|s| (s.station.as_str(), s.at_ms))
    }

    /// The model (device profile name) of a radio.
    pub(crate) fn model_of(&self, net: &str, issi: u32) -> Option<&str> {
        self.sub_of(net, issi).map(|s| s.model.as_str()).filter(|m| !m.is_empty())
    }

    pub fn list(&self) -> Vec<SubView> {
        self.data
            .subs
            .iter()
            .filter_map(|(key, s)| split_key(key).map(|(net, issi)| (net, issi, s)))
            .map(|(net, issi, s)| SubView {
                net: net.to_owned(),
                issi,
                kcv: s.kcv.clone(),
                source: s.source.clone(),
                by: s.by.clone(),
                set_ms: s.set_ms,
                model: s.model.clone(),
                last_auth: s.last_auth.clone(),
                seen: self.data.seen.get(&issi).map(|x| (x.station.clone(), x.at_ms)),
                stock: self.stock.iter().filter_map(|(st, m)| m.get(&issi).map(|c| (st.clone(), *c))).collect(),
            })
            .collect()
    }
}

/// Network of a K kept from before keys were per network.
pub const ANY_NET: &str = "*";

fn sub_key(net: &str, issi: u32) -> String {
    format!("{net}/{issi}")
}

fn split_key(key: &str) -> Option<(&str, u32)> {
    let (net, issi) = key.split_once('/')?;
    Some((net, issi.parse().ok()?))
}

fn check_issi(issi: u32) -> Result<(), String> {
    if (1..=0xFF_FFFF).contains(&issi) { Ok(()) } else { Err(format!("{issi} is not an ISSI (1-16777215)")) }
}

fn read_store(bytes: &[u8], master: &[u8; 32]) -> Option<Data> {
    let f: StoreFile = serde_json::from_slice(bytes).ok()?;
    let plain = open(master, &unhex_n::<12>(&f.nonce)?, &unhex(&f.data)?)?;
    let mut d: Data = serde_json::from_slice(&plain).ok()?;
    // Before keys were per network the key was the bare ISSI: kept as `*/ISSI`.
    let old: Vec<String> = d.subs.keys().filter(|k| !k.contains('/')).cloned().collect();
    for k in old {
        if let (Some(s), Ok(issi)) = (d.subs.remove(&k), k.parse::<u32>()) {
            d.subs.insert(sub_key(ANY_NET, issi), s);
        }
    }
    Some(d)
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

/// The rename on disk too (directory entry), so a power cut keeps the old or the new store.
fn sync_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::File::open(dir)?.sync_all()?;
    }
    Ok(())
}

fn write_master(path: &Path) -> std::io::Result<[u8; 32]> {
    let m = random::<32>();
    write_private(path, format!("{}\n", hex(&m)).as_bytes())?;
    tracing::warn!(path = %path.display(), "created the Fleet Control master key; keep a copy of it apart from the key store");
    Ok(m)
}

// ───────────────────────────── neighbours ─────────────────────────────

/// Stations (Brew logins) next to `login`: their cells list one of its cells
/// as neighbour, or its cells list theirs (from the cells' broadcasts in the
/// server's cell list).
pub fn neighbour_stations(dir: &[crate::fleet_bulk::DirCell], login: &str) -> HashSet<String> {
    let own: Vec<&crate::fleet_bulk::DirCell> = dir.iter().filter(|d| d.station == login).collect();
    let names = |d: &crate::fleet_bulk::DirCell, o: &crate::fleet_bulk::DirCell| {
        d.neighbours.as_ref().is_some_and(|l| l.iter().any(|&(mcc, mnc, car, ext)| mcc == o.mcc && mnc == o.mnc && car == o.carrier && ext == o.carrier_ext))
    };
    dir.iter()
        .filter(|d| d.station != login && own.iter().any(|o| names(d, o) || names(o, d)))
        .map(|d| d.station.clone())
        .collect()
}

// ───────────────────────────── protocol glue ─────────────────────────────

use crate::control::{send_command, ControlCommand};
use crate::state::AppState;
use std::sync::Arc;

/// Is station `id` online, with `keys-v1` and its keys from the server?
pub(crate) fn uses_server(state: &AppState, id: &str) -> Option<Option<String>> {
    let fleet = state.fleet.lock().unwrap_or_else(|e| e.into_inner());
    let s = fleet.get(id)?;
    (s.online && s.capabilities.iter().any(|c| c == CAP_KEYS) && s.key_source.as_deref() == Some("server")).then(|| s.brew_login.clone())
}

/// The network (`MCC-MNC`) of station `id`.
pub(crate) fn station_net(state: &AppState, id: &str) -> Option<String> {
    let fleet = state.fleet.lock().unwrap_or_else(|e| e.into_inner());
    fleet.get(id).map(|s| crate::fleet_sck::net_name(s.mcc, s.mnc))
}

pub(crate) fn send(state: &Arc<AppState>, id: &str, cmd: ControlCommand) {
    let (state, id) = (state.clone(), id.to_owned());
    tokio::spawn(async move {
        let _ = send_command(&state, &id, cmd).await;
    });
}

/// After a hello: a station that takes its keys from the server gets the list
/// of radios with K (it answers with its stock, which is then topped up).
pub fn after_hello(state: &Arc<AppState>, id: &str) {
    if uses_server(state, id).is_none() {
        state.fleet_keys.lock().unwrap_or_else(|e| e.into_inner()).forget_station(id);
        return;
    }
    let Some(net) = station_net(state, id) else { return };
    let issis = state.fleet_keys.lock().unwrap_or_else(|e| e.into_inner()).issis(&net);
    send(state, id, ControlCommand::AuthSubscribers { issis, drop: Vec::new() });
}

/// K of these radios (network, ISSI) changed or was removed: every station of that network
/// drops their old sets and gets the new list.
pub fn keys_changed(state: &Arc<AppState>, net: &str, changed: Vec<u32>) {
    let ids: Vec<String> = state.fleet.lock().unwrap_or_else(|e| e.into_inner()).stations().into_iter().map(|s| s.id).collect();
    for id in ids {
        let Some(sn) = station_net(state, &id) else { continue };
        if uses_server(state, &id).is_none() || (net != ANY_NET && sn != net) {
            continue;
        }
        let issis = state.fleet_keys.lock().unwrap_or_else(|e| e.into_inner()).issis(&sn);
        send(state, &id, ControlCommand::AuthSubscribers { issis, drop: changed.clone() });
    }
    crate::fleet_dck::drop_radios(state, &changed);
}

/// Top the stock of `issi` at `id` up to the setting.
fn top_up(state: &Arc<AppState>, id: &str, issi: u32) {
    let Some(net) = station_net(state, id) else { return };
    let vectors = {
        let mut keys = state.fleet_keys.lock().unwrap_or_else(|e| e.into_inner());
        let want = keys.settings().stock;
        let have = keys.stock_of(id, issi);
        if have >= want || !keys.has_k(&net, issi) {
            return;
        }
        let v = keys.vectors(&net, issi, want - have, crate::fleet::now_ms());
        keys.add_stock(id, issi, v.len() as u32);
        v
    };
    send(state, id, ControlCommand::AuthVectors { issi, vectors });
}

/// The station's stock (answer to `AuthSubscribers`): top up its radios.
pub async fn on_stock(state: &Arc<AppState>, id: &str, counts: &[IssiCount]) {
    if uses_server(state, id).is_none() {
        return;
    }
    let Some(net) = station_net(state, id) else { return };
    state.fleet_keys.lock().unwrap_or_else(|e| e.into_inner()).set_stock(id, &net, counts);
    top_up_station(state, id).await;
}

/// The station has no set for `issi` (a radio it does not keep a stock for).
/// Sets only for a radio of the station's own network (`MCC-MNC`).
pub fn on_request(state: &Arc<AppState>, id: &str, issi: u32) {
    let Some(brew) = uses_server(state, id) else { return };
    let Some(net) = station_net(state, id) else { return };
    let has = {
        let mut keys = state.fleet_keys.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(b) = &brew {
            keys.note_seen(issi, b, crate::fleet::now_ms());
        }
        keys.has_k(&net, issi)
    };
    if has {
        top_up(state, id, issi);
    } else {
        log_station_once(state, id, "keys_refused", format!("noK/{id}/{issi}"), 3600 * 1000, |label| {
            format!("Basestation {label}: authentication data for ISSI {issi} refused, no K for network {net} on this server")
        });
        send(state, id, ControlCommand::AuthVectors { issi, vectors: Vec::new() });
    }
}

/// One set used.
pub fn on_report(state: &Arc<AppState>, id: &str, issi: u32, ok: bool, mutual: bool, left: Option<u32>) {
    let brew = state.fleet.lock().unwrap_or_else(|e| e.into_inner()).get(id).and_then(|s| s.brew_login.clone());
    let net = station_net(state, id).unwrap_or_default();
    state.fleet_keys.lock().unwrap_or_else(|e| e.into_inner()).note_auth(id, &net, brew.as_deref(), issi, ok, mutual, left, crate::fleet::now_ms());
    if !ok {
        let names = crate::control::station_names(state);
        state.fleet.lock().unwrap_or_else(|e| e.into_inner()).log_station(
            id,
            None,
            "keys_auth_failed",
            |label| format!("Basestation {label}: ISSI {issi} failed authentication"),
            crate::fleet::now_ms(),
            &|k| names.get(k).cloned(),
        );
    }
    if uses_server(state, id).is_some() {
        top_up(state, id, issi);
    }
}

async fn top_up_station(state: &Arc<AppState>, id: &str) {
    let Some(brew) = uses_server(state, id) else { return };
    let dir = crate::fleet_bulk::directory(&*state.inner.read().await);
    let neighbours = brew.as_deref().map(|b| neighbour_stations(&dir, b)).unwrap_or_default();
    let Some(net) = station_net(state, id) else { return };
    let targets = state.fleet_keys.lock().unwrap_or_else(|e| e.into_inner()).targets(&net, brew.as_deref(), &neighbours, crate::fleet::now_ms());
    for issi in targets {
        top_up(state, id, issi);
    }
}

/// Every 15 s: where the radios with K are registered, and the stock of every
/// station that takes its keys from the server.
pub async fn run(state: Arc<AppState>) {
    let mut every = tokio::time::interval(std::time::Duration::from_secs(15));
    loop {
        every.tick().await;
        tick(&state).await;
    }
}

pub async fn tick(state: &Arc<AppState>) {
    let now = crate::fleet::now_ms();
    let registered: Vec<(u32, String)> = {
        let inner = state.inner.read().await;
        inner
            .subscribers
            .iter()
            .filter_map(|(issi, s)| inner.clients.get(&s.client_id).and_then(|c| c.username.clone()).map(|u| (*issi, u)))
            .collect()
    };
    {
        let mut keys = state.fleet_keys.lock().unwrap_or_else(|e| e.into_inner());
        for (issi, station) in &registered {
            keys.note_seen(*issi, station, now);
        }
    }
    crate::fleet_sck::tick(state, &registered);
    crate::fleet_cck::tick(state);
    let dir = crate::fleet_bulk::directory(&*state.inner.read().await);
    let ids: Vec<String> = state.fleet.lock().unwrap_or_else(|e| e.into_inner()).stations().into_iter().map(|s| s.id).collect();
    for id in ids {
        let Some(brew) = uses_server(state, &id) else { continue };
        let neighbours = brew.as_deref().map(|b| neighbour_stations(&dir, b)).unwrap_or_default();
        let Some(net) = station_net(state, &id) else { continue };
        let targets = state.fleet_keys.lock().unwrap_or_else(|e| e.into_inner()).targets(&net, brew.as_deref(), &neighbours, now);
        for issi in targets {
            top_up(state, &id, issi);
        }
    }
}

// ───────────────────────────── dashboard API ─────────────────────────────

use axum::{extract::State, http::{HeaderMap, StatusCode}, response::{IntoResponse, Response}, Json};

#[derive(Deserialize)]
pub struct SourceReq {
    id: String,
    /// `local` or `server`.
    source: String,
    /// The station's own K list goes (needed while it is not empty).
    #[serde(default)]
    drop_local_keys: bool,
    #[serde(default)]
    during_call: bool,
}

/// `POST /api/fleet/keys/source`: switch a station between its own K list (`local`) and the
/// server's keys (`server`) with a config change (`config-v1`, the station restarts and must
/// confirm like every config change). Needs Fleet Control keys and Fleet Control management.
pub async fn api_source(State(state): State<Arc<AppState>>, headers: HeaderMap, Json(req): Json<SourceReq>) -> Response {
    let a = crate::dashboard::access(&state, &headers);
    if !a.manages_keys() || !a.manages_fleet() {
        return (StatusCode::FORBIDDEN, "This needs the rights Fleet Control keys and Fleet Control management").into_response();
    }
    if req.source != "local" && req.source != "server" {
        return (StatusCode::BAD_REQUEST, "source: local or server").into_response();
    }
    if let Some(net) = station_net(&state, &req.id) {
        if let Some(r) = net_refused(&state, &a, &net) {
            return r;
        }
    } else if a.key_nets.is_some() {
        return (StatusCode::FORBIDDEN, "The station's network is not known yet").into_response();
    }
    let station = {
        let fleet = state.fleet.lock().unwrap_or_else(|e| e.into_inner());
        fleet.get(&req.id).map(|s| {
            (
                s.online,
                s.capabilities.iter().any(|c| c == crate::fleet_config::CONFIG_CAPABILITY),
                s.capabilities.iter().any(|c| c == CAP_KEYS),
                s.cfg.reported_hash.clone().or_else(|| s.cfg.copy.as_ref().map(|c| c.hash.clone())),
                s.cfg.apply.is_some() || s.cfg.pending.is_some(),
                s.local_keys.unwrap_or(0),
                s.local_issis.clone(),
            )
        })
    };
    let Some((online, config, keys, hash, busy, local, local_issis)) = station else {
        return (StatusCode::NOT_FOUND, "No such station in the Fleet Manager").into_response();
    };
    if !config || !keys {
        return (StatusCode::BAD_REQUEST, "This station cannot do it (needs config-v1 and keys-v1)").into_response();
    }
    if !online {
        return (StatusCode::CONFLICT, "The station is offline").into_response();
    }
    if busy {
        return (StatusCode::CONFLICT, "A config change on this station is not finished yet").into_response();
    }
    let Some(hash) = hash else { return (StatusCode::CONFLICT, "The station's config hash is not known yet").into_response() };
    if req.source == "server" && local > 0 && !req.drop_local_keys {
        return (StatusCode::CONFLICT, format!("The station holds {local} keys of its own: export them on the station and import them here first")).into_response();
    }
    // Two-step switch, step 1: this server must hold K for every radio of the station's own list
    // (stations without `local_issis` are not checked). Step 2 is on the station: it keeps its own
    // keys until the first authentication with this server's data succeeds.
    if req.source == "server" {
        if let Some(list) = local_issis.filter(|l| !l.is_empty()) {
            let missing = {
                let Some(net) = station_net(&state, &req.id) else {
                    return (StatusCode::CONFLICT, "The station's network is not known yet").into_response();
                };
                let keys = state.fleet_keys.lock().unwrap_or_else(|e| e.into_inner());
                missing_k(&keys, &net, &list)
            };
            if !missing.is_empty() {
                let shown: Vec<String> = missing.iter().take(10).map(|i| i.to_string()).collect();
                let more = if missing.len() > 10 { format!(" and {} more", missing.len() - 10) } else { String::new() };
                let msg = format!("This server has no K for {} radio(s) of the station: {}{more}. Import them first", missing.len(), shown.join(", "));
                log(&state, a.user.as_deref(), format!("station {}: switch to server keys refused, {} radio(s) without K here", req.id, missing.len()));
                return (StatusCode::CONFLICT, msg).into_response();
            }
        }
    }
    let changes = serde_json::json!({"keys": {"source": req.source, "drop_local_keys": req.drop_local_keys}});
    let summary = vec![format!(
        "Key source: {}{}",
        if req.source == "server" { "server (keys from this server)" } else { "local (keys in the station's config)" },
        if req.drop_local_keys && local > 0 { format!(", {local} keys of the station removed") } else { String::new() }
    )];
    match crate::fleet_config::send_change_coded(&state, &req.id, hash, changes, summary.clone(), a.user.clone(), req.during_call).await {
        Ok(_) => {
            log(&state, a.user.as_deref(), format!("station {}: {}", req.id, summary[0]));
            Json(serde_json::json!({"ok": true})).into_response()
        }
        Err((code, msg)) => (StatusCode::CONFLICT, format!("{code}: {msg}")).into_response(),
    }
}

/// The radios of `list` this server holds no K for in `net`.
pub(crate) fn missing_k(keys: &KeyStore, net: &str, list: &[u32]) -> Vec<u32> {
    let mut m: Vec<u32> = list.iter().copied().filter(|i| !keys.has_k(net, *i)).collect();
    m.sort_unstable();
    m.dedup();
    m
}

/// 8f: `None` when `a` may manage the keys of `net`, else the refusal (logged).
pub(crate) fn net_refused(state: &AppState, a: &crate::dash_users::Access, net: &str) -> Option<Response> {
    if a.keys_for_net(net.trim()) {
        return None;
    }
    let who = a.user.clone().unwrap_or_else(|| "(no login)".into());
    tracing::warn!(user = %who, net, "key management: network refused");
    log(state, a.user.as_deref(), format!("access refused: {who} does not manage the keys of network {}", net.trim()));
    let mine = a.key_nets.as_ref().map(|n| n.iter().cloned().collect::<Vec<_>>().join(", ")).unwrap_or_default();
    Some((StatusCode::FORBIDDEN, format!("You manage the keys of network(s) {mine} only")).into_response())
}

pub(crate) fn forbidden() -> Response {
    (StatusCode::FORBIDDEN, "This needs the right Fleet Control keys").into_response()
}

pub(crate) fn bad(e: impl Into<String>) -> Response {
    (StatusCode::BAD_REQUEST, e.into()).into_response()
}

/// A key management line in the Fleet Control change log (never key values).
/// 8f: a station's request this server refuses, or an automatic action, into the change log;
/// the same `key` at most once per `period_ms` (a radio retrying must not flood the log).
pub(crate) fn log_station_once(state: &AppState, id: &str, kind: &str, key: String, period_ms: u64, text: impl Fn(&str) -> String) {
    static LAST: std::sync::Mutex<Option<HashMap<String, u64>>> = std::sync::Mutex::new(None);
    let now = crate::fleet::now_ms();
    {
        let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
        let map = last.get_or_insert_with(HashMap::new);
        map.retain(|_, t| now.saturating_sub(*t) < 24 * 3600 * 1000);
        if map.get(&key).is_some_and(|t| now.saturating_sub(*t) < period_ms) {
            return;
        }
        map.insert(key, now);
    }
    let names = crate::control::station_names(state);
    state.fleet.lock().unwrap_or_else(|e| e.into_inner()).log_station(id, None, kind, text, now, &|k| names.get(k).cloned());
}

pub(crate) fn log(state: &AppState, user: Option<&str>, text: String) {
    state.fleet.lock().unwrap_or_else(|e| e.into_inner()).log_station("", user, "keys", |_| text, crate::fleet::now_ms(), &|_| None);
    state.monitor.emit("fleet_changed", serde_json::json!({"keys": true}));
}

/// `GET /api/fleet/keys` (Fleet Control keys): radios with K (check value
/// only), stock per station, stations and their key source, settings.
pub async fn api_list(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let a = crate::dashboard::access(&state, &headers);
    if !a.manages_keys() {
        return forbidden();
    }
    let names = crate::control::station_names(&state);
    let stations: Vec<serde_json::Value> = state
        .fleet
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .stations()
        .into_iter()
        .map(|s| {
            serde_json::json!({
                "id": s.id,
                "label": crate::fleet::label(&|k| names.get(k).cloned(), &s.id, s.brew_login.as_deref()),
                "brew_login": s.brew_login,
                "online": s.online,
                "keys": s.capabilities.iter().any(|c| c == CAP_KEYS),
                "key_source": s.key_source,
                "local_keys": s.local_keys,
            })
        })
        .collect();
    let nets = crate::fleet_sck::station_nets(&state);
    let logins: HashMap<String, String> = nets.iter().filter_map(|(id, s)| s.brew_login.clone().map(|l| (id.clone(), l))).collect();
    let keys = state.fleet_keys.lock().unwrap_or_else(|e| e.into_inner());
    // 8f: a user limited to some networks sees only their radios and keys.
    let mine = |net: &str| a.keys_for_net(net);
    let only_mine = |mut v: serde_json::Value| {
        if let Some(m) = v.get_mut("nets").and_then(|n| n.as_object_mut()) {
            m.retain(|net, _| mine(net));
        }
        v
    };
    let station_ok: HashMap<String, bool> = nets.iter().map(|(id, s)| (id.clone(), mine(&s.net))).collect();
    let stations: Vec<serde_json::Value> =
        stations.into_iter().filter(|s| a.key_nets.is_none() || s["id"].as_str().is_some_and(|id| station_ok.get(id).copied().unwrap_or(false))).collect();
    let mut fallback = crate::fleet_fallback::view(&keys);
    if let (true, Some(m)) = (a.key_nets.is_some(), fallback.get_mut("stations").and_then(|x| x.as_object_mut())) {
        m.retain(|id, _| station_ok.get(id).copied().unwrap_or(false));
    }
    Json(serde_json::json!({
        "problem": keys.problem(),
        "settings": keys.settings(),
        "subs": keys.list().into_iter().filter(|s| mine(&s.net)).collect::<Vec<_>>(),
        "key_nets": keys.nets().into_iter().filter(|n| mine(n)).collect::<Vec<_>>(),
        "my_nets": a.key_nets,
        "stations": stations,
        "sck": only_mine(crate::fleet_sck::view(&keys, &nets, crate::fleet::now_ms())),
        "cck": only_mine(crate::fleet_cck::view(&keys, &logins)),
        "dck": crate::fleet_dck::view(&keys),
        "fallback": fallback,
    }))
    .into_response()
}

#[derive(Deserialize)]
pub struct SetReq {
    /// Network `MCC-MNC` (empty: this server's own network).
    #[serde(default)]
    net: String,
    issi: u32,
    k: String,
}

/// The network of a request: as given, else this server's own (`[multi_network] mcc/mnc`).
async fn req_net(state: &AppState, net: &str, any_ok: bool) -> Result<String, String> {
    let net = net.trim();
    if net == ANY_NET && any_ok {
        return Ok(ANY_NET.into());
    }
    if !net.is_empty() {
        crate::fleet_sck::check_net(net)?;
        return Ok(net.into());
    }
    let own = state.networks.read().map(|n| n.clone()).unwrap_or_default();
    if own.mcc == 0 {
        return Err("choose the network (MCC-MNC) of the radio".into());
    }
    Ok(crate::fleet_sck::net_name(own.mcc, own.mnc))
}

/// `POST /api/fleet/keys/set`: write K for one radio (typed or pasted).
pub async fn api_set(State(state): State<Arc<AppState>>, headers: HeaderMap, Json(req): Json<SetReq>) -> Response {
    let a = crate::dashboard::access(&state, &headers);
    if !a.manages_keys() {
        return forbidden();
    }
    let k = match parse_k(&req.k) {
        Ok(k) => k,
        Err(e) => return bad(e),
    };
    let net = match req_net(&state, &req.net, false).await {
        Ok(n) => n,
        Err(e) => return bad(e),
    };
    if let Some(r) = net_refused(&state, &a, &net) {
        return r;
    }
    let res = state.fleet_keys.lock().unwrap_or_else(|e| e.into_inner()).set_k(&net, req.issi, &k, "manual", a.user.as_deref(), crate::fleet::now_ms());
    match res {
        Ok((kcv, changed)) => {
            if changed {
                log(&state, a.user.as_deref(), format!("K written for ISSI {} in network {net} (check value {kcv})", req.issi));
                keys_changed(&state, &net, vec![req.issi]);
            }
            Json(serde_json::json!({"ok": true, "kcv": kcv, "changed": changed})).into_response()
        }
        Err(e) => bad(e),
    }
}

#[derive(Deserialize)]
pub struct IssiReq {
    #[serde(default)]
    net: String,
    issi: u32,
}

/// `POST /api/fleet/keys/generate`: a new random K, returned this once (to
/// load it into the radio with the CPS / KVL), afterwards write-only.
pub async fn api_generate(State(state): State<Arc<AppState>>, headers: HeaderMap, Json(req): Json<IssiReq>) -> Response {
    let a = crate::dashboard::access(&state, &headers);
    if !a.manages_keys() {
        return forbidden();
    }
    let net = match req_net(&state, &req.net, false).await {
        Ok(n) => n,
        Err(e) => return bad(e),
    };
    if let Some(r) = net_refused(&state, &a, &net) {
        return r;
    }
    let res = state.fleet_keys.lock().unwrap_or_else(|e| e.into_inner()).generate(&net, req.issi, a.user.as_deref(), crate::fleet::now_ms());
    match res {
        Ok((k, kcv)) => {
            log(&state, a.user.as_deref(), format!("K generated for ISSI {} in network {net} (check value {kcv})", req.issi));
            keys_changed(&state, &net, vec![req.issi]);
            let mut resp = Json(serde_json::json!({"ok": true, "issi": req.issi, "k": hex(&k).to_uppercase(), "grouped": grouped(&k), "kcv": kcv})).into_response();
            resp.headers_mut().insert(axum::http::header::CACHE_CONTROL, axum::http::HeaderValue::from_static("no-store"));
            resp
        }
        Err(e) => bad(e),
    }
}

#[derive(Deserialize)]
pub struct ImportReq {
    #[serde(default)]
    net: String,
    /// `ISSI;K` lines (CSV).
    #[serde(default)]
    text: Option<String>,
    /// A FlowStation export file and its password.
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    password: Option<String>,
}

/// `POST /api/fleet/keys/import`: CSV lines or a FlowStation export file.
pub async fn api_import(State(state): State<Arc<AppState>>, headers: HeaderMap, Json(req): Json<ImportReq>) -> Response {
    let a = crate::dashboard::access(&state, &headers);
    if !a.manages_keys() {
        return forbidden();
    }
    let net = match req_net(&state, &req.net, false).await {
        Ok(n) => n,
        Err(e) => return bad(e),
    };
    if let Some(r) = net_refused(&state, &a, &net) {
        return r;
    }
    let (text, source) = match (req.text, req.file) {
        (_, Some(file)) => match tokio::task::spawn_blocking(move || open_export(&file, req.password.as_deref().unwrap_or(""))).await {
            Ok(Ok(t)) => (t, "flowstation"),
            Ok(Err(e)) => return bad(e),
            Err(_) => return bad("import failed"),
        },
        (Some(t), None) => (t, "import"),
        (None, None) => return bad("nothing to import"),
    };
    let res = state.fleet_keys.lock().unwrap_or_else(|e| e.into_inner()).import(&net, &text, source, a.user.as_deref(), crate::fleet::now_ms());
    match res {
        Ok(r) => {
            if r.errors.is_empty() {
                log(&state, a.user.as_deref(), format!("K imported into network {net} ({}): {} new, {} changed, {} unchanged", if source == "flowstation" { "FlowStation export" } else { "CSV" }, r.added, r.changed, r.unchanged));
                if r.added + r.changed > 0 {
                    keys_changed(&state, &net, r.kcvs.iter().map(|(i, _)| *i).collect());
                }
            }
            Json(r).into_response()
        }
        Err(e) => bad(e),
    }
}

#[derive(Deserialize)]
pub struct DeleteReq {
    #[serde(default)]
    net: String,
    issis: Vec<u32>,
}

/// `POST /api/fleet/keys/delete`: remove K of these radios.
pub async fn api_delete(State(state): State<Arc<AppState>>, headers: HeaderMap, Json(req): Json<DeleteReq>) -> Response {
    let a = crate::dashboard::access(&state, &headers);
    if !a.manages_keys() {
        return forbidden();
    }
    let net = match req_net(&state, &req.net, true).await {
        Ok(n) => n,
        Err(e) => return bad(e),
    };
    if let Some(r) = net_refused(&state, &a, &net) {
        return r;
    }
    let mut gone = Vec::new();
    {
        let mut keys = state.fleet_keys.lock().unwrap_or_else(|e| e.into_inner());
        for issi in &req.issis {
            match keys.delete(&net, *issi) {
                Ok(true) => gone.push(*issi),
                Ok(false) => {}
                Err(e) => return bad(e),
            }
        }
    }
    if !gone.is_empty() {
        log(&state, a.user.as_deref(), format!("K removed in network {net} for ISSI {}", gone.iter().map(u32::to_string).collect::<Vec<_>>().join(", ")));
        keys_changed(&state, &net, gone.clone());
    }
    Json(serde_json::json!({"ok": true, "removed": gone.len()})).into_response()
}

#[derive(Deserialize)]
pub struct ModelReq {
    #[serde(default)]
    net: String,
    issi: u32,
    model: String,
}

/// `POST /api/fleet/keys/model`: the radio type (device profile).
pub async fn api_model(State(state): State<Arc<AppState>>, headers: HeaderMap, Json(req): Json<ModelReq>) -> Response {
    let a = crate::dashboard::access(&state, &headers);
    if !a.manages_keys() {
        return forbidden();
    }
    let net = match req_net(&state, &req.net, true).await {
        Ok(n) => n,
        Err(e) => return bad(e),
    };
    if let Some(r) = net_refused(&state, &a, &net) {
        return r;
    }
    let res = state.fleet_keys.lock().unwrap_or_else(|e| e.into_inner()).set_model(&net, req.issi, req.model.trim());
    match res {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => bad(e),
    }
}

/// `POST /api/fleet/keys/settings` (admins): stock size and days.
pub async fn api_settings(State(state): State<Arc<AppState>>, headers: HeaderMap, Json(req): Json<Settings>) -> Response {
    let a = crate::dashboard::access(&state, &headers);
    if !a.admin {
        return (StatusCode::FORBIDDEN, "Admins only").into_response();
    }
    let res = state.fleet_keys.lock().unwrap_or_else(|e| e.into_inner()).set_settings(req.clone());
    match res {
        Ok(()) => {
            log(&state, a.user.as_deref(), format!("Key settings changed: {} sets per radio, radios seen in the last {} days, sets valid {} days", req.stock, req.days, req.set_days));
            Json(serde_json::json!({"ok": true})).into_response()
        }
        Err(e) => bad(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const K: &str = "0123456789ABCDEF0123456789ABCDEF";

    #[test]
    fn typed_keys_lose_separators_and_need_exactly_32_hex_digits() {
        let k = parse_k("0123 4567:89ab-cdef\n0123 4567 89AB CDEF").unwrap();
        assert_eq!(hex(&k).to_uppercase(), K);
        assert!(parse_k("0123456789ABCDEF0123456789ABCDE").is_err(), "31 digits");
        assert!(parse_k("0123456789ABCDEF0123456789ABCDEFF").is_err(), "33 digits");
        assert!(parse_k("0123456789ABCDEF0123456789ABCDEG").is_err(), "not hex");
        assert_eq!(grouped(&k), "0123 4567 89AB CDEF 0123 4567 89AB CDEF");
    }

    #[test]
    fn check_value_is_the_start_of_sha256_and_tells_one_typo() {
        let k = parse_k(K).unwrap();
        let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, &k);
        assert_eq!(check_value(&k), hex(&digest.as_ref()[..8]).to_uppercase());
        let mut typo = k;
        typo[15] ^= 1;
        assert_ne!(check_value(&k), check_value(&typo));
    }

    #[test]
    fn import_is_all_or_nothing_and_reports_check_values() {
        let mut s = KeyStore::memory();
        let bad = format!("ISSI;K\n# comment\n100;{K}\n200;12345\n");
        let r = s.import("232-1", &bad, "csv", Some("op"), 1).unwrap();
        assert_eq!(r.errors.len(), 1);
        assert!(r.errors[0].contains("line 4"), "{:?}", r.errors);
        assert!(!s.has_k("232-1", 100), "nothing imported when a line is wrong");
        let good = format!("ISSI;K\n100;{K}\n200,{}\n300\t{}\n", "11".repeat(16), "22".repeat(16));
        let r = s.import("232-1", &good, "csv", Some("op"), 1).unwrap();
        assert_eq!((r.added, r.changed, r.unchanged), (3, 0, 0));
        assert_eq!(r.kcvs[0], (100, check_value(&parse_k(K).unwrap())));
        let r = s.import("232-1", &format!("100;{K}\n200;{}\n", "33".repeat(16)), "csv", None, 2).unwrap();
        assert_eq!((r.added, r.changed, r.unchanged), (0, 1, 1));
    }

    #[test]
    fn flowstation_export_opens_with_its_password_only() {
        let file = make_export(&format!("100;{K}\n"), "long enough pw", 10_000);
        assert!(open_export(&file, "wrong password").is_err());
        assert_eq!(open_export(&file, "long enough pw").unwrap(), format!("100;{K}\n"));
        assert!(open_export("{\"format\":\"other\"}", "x").is_err());
    }

    #[test]
    fn flowstation_export_format_matches_the_station() {
        // Made with ring exactly like FlowStation's `security::export_keys` (salt 07…, nonce 09…,
        // 10 000 rounds, password "long enough pw"): the two implementations agree.
        let file = r#"{"data":"549b55bd3027a1687e93c9b193330ec298cf640b7bc58a99fe6494c6a30aaa8a69043a485a19f2c322576bf55700bbc6a94b3d81acb3373674f9aec32a547d27f2506eaa1e7981c3e67f73c8cf5b0497ff2ea2de73bf6a1bd4f2bbc957df","format":"bost-keys-v1","iterations":10000,"kdf":"pbkdf2-hmac-sha256","nonce":"090909090909090909090909","salt":"07070707070707070707070707070707"}"#;
        let text = open_export(file, "long enough pw").unwrap();
        assert_eq!(text, format!("100;{}\n2358245;{}\n", "11".repeat(16), "ab".repeat(16)));
        let mut s = KeyStore::memory();
        assert_eq!(s.import("232-1", &text, "export", None, 1).unwrap().added, 2);
    }

    #[test]
    fn sets_authenticate_like_k_and_k_never_leaves() {
        let mut s = KeyStore::memory();
        let k = parse_k(K).unwrap();
        s.set_k("232-1", 100, &k, "typed", None, 1).unwrap();
        let sets = s.vectors("232-1", 100, 3, 1000);
        assert_eq!(sets.len(), 3);
        assert!(sets.iter().all(|v| v.exp_ms == 1000 + 7 * DAY_MS && v.rev == s.rev_of("232-1", 100).unwrap()));
        assert!(s.vectors("232-1", 100, 500, 1000).len() as u32 <= MAX_SETS_PER_MESSAGE);
        assert!(s.vectors("232-1", 999, 3, 1000).is_empty(), "no K, no sets");
        let rand1 = [0x5a; 10];
        for v in &sets {
            let rs: [u8; 10] = unhex_n(&v.rs).unwrap();
            let ks: [u8; 16] = unhex_n(&v.ks).unwrap();
            let ks_p: [u8; 16] = unhex_n(&v.ks_p).unwrap();
            // EN 300 392-7 clause 4.1.1: RES1 the station computes from KS equals the radio's from K.
            assert_eq!(crate::taa1::ta12(&ks, &rand1).res, crate::taa1::ta12(&crate::taa1::ta11(&k, &rs), &rand1).res);
            assert_eq!(ks_p, crate::taa1::ta21(&k, &rs));
            assert!(!v.ks.contains(&hex(&k)) && !v.ks_p.contains(&hex(&k)));
        }
        assert_ne!(sets[0].rs, sets[1].rs, "fresh RS per set");
    }

    #[test]
    fn a_new_k_drops_the_stock_and_the_list_shows_no_key() {
        let mut s = KeyStore::memory();
        s.set_k("232-1", 100, &parse_k(K).unwrap(), "typed", None, 1).unwrap();
        s.add_stock("bs1", 100, 10);
        let (_, changed) = s.set_k("232-1", 100, &[0x44; 16], "typed", None, 2).unwrap();
        assert!(changed);
        assert_eq!(s.stock_of("bs1", 100), 0);
        let json = serde_json::to_string(&s.list()).unwrap();
        assert!(!json.to_lowercase().contains(&"44".repeat(16)), "K is write-only");
        let (k, kcv) = s.generate("232-1", 200, None, 3).unwrap();
        assert_eq!(kcv, check_value(&k));
    }

    /// The same ISSI in two networks are two radios with their own K (review OE1LHP point 2).
    #[test]
    fn a_switch_to_server_keys_needs_k_for_every_local_radio() {
        let mut s = KeyStore::memory();
        let k = parse_k(K).unwrap();
        s.set_k("232-1", 100, &k, "typed", None, 1).unwrap();
        s.set_k("232-2", 200, &k, "typed", None, 1).unwrap();
        assert_eq!(missing_k(&s, "232-1", &[100, 200, 300, 300]), vec![200, 300]);
        assert!(missing_k(&s, "232-1", &[100]).is_empty());
    }

    #[test]
    fn k_is_kept_per_network() {
        let mut s = KeyStore::memory();
        s.set_k("232-1", 100, &[0x11; 16], "typed", None, 1).unwrap();
        s.set_k("232-2", 100, &[0x22; 16], "typed", None, 1).unwrap();
        assert_eq!(s.k_of("232-1", 100), Some([0x11; 16]));
        assert_eq!(s.k_of("232-2", 100), Some([0x22; 16]));
        assert!(!s.has_k("262-1", 100), "no K in a third network");
        assert_eq!(s.issis("232-1"), vec![100]);
        assert!(s.set_k("bogus", 101, &[0x33; 16], "typed", None, 1).is_err());
        assert!(s.delete("232-1", 100).unwrap());
        assert_eq!(s.k_of("232-2", 100), Some([0x22; 16]), "the other network keeps its radio");
        let list = s.list();
        assert_eq!((list.len(), list[0].net.as_str()), (1, "232-2"));
        assert_eq!(s.nets(), vec!["232-2".to_string()]);
    }

    /// A store from before keys were per network: the bare ISSIs become `*/ISSI`, usable in any
    /// network until written for one.
    #[test]
    fn an_old_store_keeps_its_keys_for_any_network() {
        let master = [7u8; 32];
        let plain = serde_json::json!({"subs": {"100": {"k": "11".repeat(16), "kcv": "x", "source": "typed", "by": null, "set_ms": 1}}});
        let (nonce, data) = seal(&master, plain.to_string().as_bytes());
        let file = serde_json::to_vec(&StoreFile { v: 1, nonce: hex(&nonce), data: hex(&data) }).unwrap();
        let d = read_store(&file, &master).unwrap();
        let mut s = KeyStore::memory();
        s.data = d;
        assert!(s.has_k("232-1", 100) && s.has_k("262-1", 100));
        assert_eq!(s.issis("232-1"), vec![100]);
        s.set_k("232-1", 100, &[0x22; 16], "typed", None, 2).unwrap();
        assert_eq!(s.k_of("232-1", 100), Some([0x22; 16]));
        assert_eq!(s.k_of("262-1", 100), Some([0x11; 16]), "other networks still use the old entry");
        assert!(s.delete(ANY_NET, 100).unwrap());
        assert!(!s.has_k("262-1", 100));
    }

    /// Every K change counts up the revision (sent with the data sets).
    #[test]
    fn the_k_revision_counts_up() {
        let mut s = KeyStore::memory();
        s.set_k("232-1", 100, &[0x11; 16], "typed", None, 1).unwrap();
        assert_eq!(s.rev_of("232-1", 100), Some(0));
        s.set_k("232-1", 100, &[0x11; 16], "typed", None, 2).unwrap();
        assert_eq!(s.rev_of("232-1", 100), Some(0), "unchanged K");
        s.set_k("232-1", 100, &[0x12; 16], "typed", None, 3).unwrap();
        assert_eq!(s.rev_of("232-1", 100), Some(1));
    }

    #[test]
    fn the_store_file_is_encrypted_and_needs_its_master_key() {
        let dir = std::env::temp_dir().join(format!("fleet-keys-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let storage = dir.join("history.log");
        let mut s = KeyStore::load(Some(&storage), None);
        assert!(s.problem().is_none());
        s.set_k("232-1", 100, &parse_k(K).unwrap(), "typed", None, 1).unwrap();
        let raw = std::fs::read(dir.join("history.log.fleet-keys")).unwrap();
        assert!(!String::from_utf8_lossy(&raw).to_uppercase().contains(K), "no K in clear");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for f in ["history.log.fleet-keys", "history.log.fleet-master-key"] {
                assert_eq!(std::fs::metadata(dir.join(f)).unwrap().permissions().mode() & 0o777, 0o600, "{f}");
            }
        }
        assert!(KeyStore::load(Some(&storage), None).has_k("232-1", 100), "read again after a restart");
        std::fs::remove_file(dir.join("history.log.fleet-master-key")).unwrap();
        let lost = KeyStore::load(Some(&storage), None);
        assert!(lost.problem().is_some_and(|p| p.contains("missing")), "never a new master key over an existing store");
        assert!(!dir.join("history.log.fleet-master-key").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn stock_goes_to_radios_seen_here_or_next_door_recently() {
        let mut s = KeyStore::memory();
        for i in [1, 2, 3, 4] {
            s.set_k("232-1", i, &[i as u8; 16], "typed", None, 0).unwrap();
        }
        let day = 86_400_000;
        let now = 40 * day;
        s.note_seen(1, "1000001", now);
        s.note_seen(2, "1000002", now);
        s.note_seen(3, "1000003", now);
        s.note_seen(4, "1000001", now - 31 * day);
        s.note_seen(5, "1000001", now); // no K: ignored
        let next: HashSet<String> = ["1000002".to_string()].into();
        let mut t = s.targets("232-1", Some("1000001"), &next, now);
        t.sort();
        assert_eq!(t, vec![1, 2], "own and neighbour radios, not the far one, not one seen 31 days ago");
    }

    fn cell(station: &str, carrier: u16, neighbours: &[u16]) -> crate::fleet_bulk::DirCell {
        crate::fleet_bulk::DirCell {
            name: format!("{station}/0"),
            station: station.into(),
            cell: 0,
            online: true,
            mcc: 232,
            mnc: 20,
            carrier,
            carrier_ext: 0,
            location_area: 1,
            rxlev_access_min_dbm: None,
            ms_txpwr_max_dbm: None,
            service_details: None,
            neighbours: Some(neighbours.iter().map(|&c| (232, 20, c, 0)).collect()),
        }
    }

    #[test]
    fn neighbours_count_in_both_directions() {
        let dir = vec![cell("a", 1528, &[1530]), cell("b", 1530, &[]), cell("c", 1532, &[1528]), cell("d", 1534, &[])];
        let n = neighbour_stations(&dir, "a");
        assert_eq!(n, ["b".to_string(), "c".to_string()].into());
    }

    #[test]
    fn settings_stay_in_range() {
        let mut s = KeyStore::memory();
        assert!(s.set_settings(Settings { stock: 0, days: 30, set_days: 7 }).is_err());
        assert!(s.set_settings(Settings { stock: 51, days: 30, set_days: 7 }).is_err());
        assert!(s.set_settings(Settings { stock: 10, days: 366, set_days: 7 }).is_err());
        assert!(s.set_settings(Settings { stock: 10, days: 30, set_days: 0 }).is_err());
        assert!(s.set_settings(Settings { stock: 10, days: 30, set_days: 91 }).is_err());
        assert!(s.set_settings(Settings { stock: 50, days: 365, set_days: 90 }).is_ok());
    }
}
