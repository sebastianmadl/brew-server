//! Accounts page of the dashboard: the logins Basestations and terminals use
//! on the Brew, telemetry and control listeners, edited live (the next
//! connection uses them, no restart) and written to the config file.

use crate::state::{AppState, Users};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Debug, Serialize, PartialEq)]
pub struct AccountView {
    pub username: String,
    pub brew: bool,
    pub telemetry: bool,
    pub control: bool,
}

#[derive(Debug, Serialize)]
pub struct ServiceView {
    /// The listener runs (`enabled` in its config section).
    pub enabled: bool,
    /// No login is required: the list is empty (telemetry, control) or
    /// `[auth] enabled = false` (Brew).
    pub open: bool,
}

#[derive(Debug, Serialize)]
pub struct AccountsView {
    pub brew: ServiceView,
    pub telemetry: ServiceView,
    pub control: ServiceView,
    pub accounts: Vec<AccountView>,
}

#[derive(Debug, Deserialize)]
pub struct AccountRequest {
    pub username: String,
    /// Empty keeps the password the account already has on a service.
    #[serde(default)]
    pub password: String,
    pub brew: bool,
    pub telemetry: bool,
    pub control: bool,
}

fn read(u: &Users) -> std::collections::HashMap<String, String> {
    u.read().map(|m| m.clone()).unwrap_or_default()
}

fn view(state: &AppState) -> AccountsView {
    let (b, t, c) = (read(&state.logins.brew), read(&state.logins.telemetry), read(&state.logins.control));
    let mut all: BTreeMap<&String, AccountView> = BTreeMap::new();
    for (map, which) in [(&b, 0), (&t, 1), (&c, 2)] {
        for name in map.keys() {
            let a = all.entry(name).or_insert_with(|| AccountView { username: name.clone(), brew: false, telemetry: false, control: false });
            match which { 0 => a.brew = true, 1 => a.telemetry = true, _ => a.control = true }
        }
    }
    let cfg = &state.config;
    AccountsView {
        brew: ServiceView { enabled: true, open: !cfg.auth.enabled },
        telemetry: ServiceView { enabled: cfg.telemetry.enabled, open: t.is_empty() },
        control: ServiceView { enabled: cfg.control.enabled, open: c.is_empty() },
        accounts: all.into_values().collect(),
    }
}

/// Checks a request against the current lists; the error says what is wrong.
fn check(req: &AccountRequest, lists: [&std::collections::HashMap<String, String>; 3]) -> Result<(), String> {
    let name = &req.username;
    if name.is_empty() || name.len() > 64 || name.chars().any(|c| c == ':' || c.is_whitespace() || c.is_control()) {
        return Err("username: 1 to 64 characters, no spaces or ':'".into());
    }
    if !(req.brew || req.telemetry || req.control) {
        return Err("choose at least one of Brew, telemetry and control".into());
    }
    if req.brew && !crate::server::is_valid_brew_username(name) {
        return Err("a Brew login is the station's or terminal's number: 1 to 7 digits".into());
    }
    if req.password.chars().any(char::is_control) || req.password.len() > 128 {
        return Err("password: up to 128 characters, no control characters".into());
    }
    if req.password.is_empty() {
        let wanted = [req.brew, req.telemetry, req.control];
        if wanted.iter().zip(lists).any(|(w, l)| *w && !l.contains_key(name)) {
            return Err("enter a password (a new login needs one)".into());
        }
    }
    Ok(())
}

fn set(u: &Users, name: &str, want: bool, password: &str) {
    if let Ok(mut m) = u.write() {
        if !want {
            m.remove(name);
        } else if !password.is_empty() {
            m.insert(name.to_owned(), password.to_owned());
        }
    }
}

async fn save(state: &Arc<AppState>) -> Result<(), String> {
    let cfg = state.config_snapshot();
    crate::CONFIG_WRITE_APPLIED_LIVE.store(true, std::sync::atomic::Ordering::SeqCst);
    let saved = crate::dashboard::save_config(state, &cfg).await;
    if saved.is_err() {
        crate::CONFIG_WRITE_APPLIED_LIVE.store(false, std::sync::atomic::Ordering::SeqCst);
    }
    saved.map_err(|e| format!("applied, but writing the config file failed: {e}"))
}

/// `GET /api/accounts` (admins).
pub async fn list(State(state): State<Arc<AppState>>) -> Json<AccountsView> {
    Json(view(&state))
}

/// `POST /api/accounts` (admins): adds a login or changes which services it
/// is on and its password. Applies to the next connection at once.
pub async fn upsert(State(state): State<Arc<AppState>>, Json(mut req): Json<AccountRequest>) -> Response {
    req.username = req.username.trim().to_owned();
    let l = &state.logins;
    let (b, t, c) = (read(&l.brew), read(&l.telemetry), read(&l.control));
    if let Err(e) = check(&req, [&b, &t, &c]) {
        return (StatusCode::BAD_REQUEST, e).into_response();
    }
    set(&l.brew, &req.username, req.brew, &req.password);
    set(&l.telemetry, &req.username, req.telemetry, &req.password);
    set(&l.control, &req.username, req.control, &req.password);
    tracing::warn!(user = %req.username, brew = req.brew, telemetry = req.telemetry, control = req.control, "login changed from the dashboard");
    match save(&state).await {
        Ok(()) => Json(view(&state)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// `DELETE /api/accounts/{username}` (admins): removes the login from all
/// three services. Connections already up stay until they reconnect.
pub async fn remove(State(state): State<Arc<AppState>>, Path(name): Path<String>) -> Response {
    let l = &state.logins;
    for u in [&l.brew, &l.telemetry, &l.control] {
        set(u, &name, false, "");
    }
    tracing::warn!(user = %name, "login removed from the dashboard");
    match save(&state).await {
        Ok(()) => Json(view(&state)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn req(name: &str, pw: &str, b: bool, t: bool, c: bool) -> AccountRequest {
        AccountRequest { username: name.into(), password: pw.into(), brew: b, telemetry: t, control: c }
    }

    #[test]
    fn checks_name_password_and_services() {
        let empty = HashMap::new();
        let have = HashMap::from([("1000001".to_string(), "x".to_string())]);
        assert!(check(&req("1000001", "pw", true, true, true), [&empty, &empty, &empty]).is_ok());
        assert!(check(&req("100000001", "pw", true, false, false), [&empty, &empty, &empty]).is_err(), "Brew needs 1-7 digits");
        assert!(check(&req("100000001", "pw", false, true, true), [&empty, &empty, &empty]).is_ok());
        assert!(check(&req("a b", "pw", false, true, false), [&empty, &empty, &empty]).is_err());
        assert!(check(&req("1000001", "pw", false, false, false), [&empty, &empty, &empty]).is_err());
        assert!(check(&req("1000001", "", true, false, false), [&empty, &empty, &empty]).is_err(), "new login needs a password");
        assert!(check(&req("1000001", "", true, false, false), [&have, &empty, &empty]).is_ok(), "keeps the old one");
        assert!(check(&req("1000001", "", true, true, false), [&have, &empty, &empty]).is_err(), "new on telemetry");
    }

    #[tokio::test]
    async fn a_login_applies_live_to_all_three_services_and_goes_again() {
        let path = std::env::temp_dir().join(format!("brew-accounts-{}.toml", uuid::Uuid::new_v4().simple()));
        let mut c = crate::config::Config::default();
        c.storage.enabled = false;
        c.sms_center.enabled = false;
        let state = Arc::new(AppState::new(c, path.clone()).0);
        let r = upsert(State(state.clone()), Json(req("1000005", "secret", true, true, true))).await;
        assert_eq!(r.status(), StatusCode::OK);
        for u in [&state.logins.brew, &state.logins.telemetry, &state.logins.control] {
            assert_eq!(u.read().unwrap().get("1000005").map(String::as_str), Some("secret"));
        }
        let snap = state.config_snapshot();
        assert_eq!(snap.auth.users.get("1000005").map(String::as_str), Some("secret"), "saved with the config");
        assert_eq!(snap.control.users.get("1000005").map(String::as_str), Some("secret"));
        // Off control, password kept.
        upsert(State(state.clone()), Json(req("1000005", "", true, true, false))).await;
        assert!(!state.logins.control.read().unwrap().contains_key("1000005"));
        assert_eq!(state.logins.brew.read().unwrap().get("1000005").map(String::as_str), Some("secret"));
        assert_eq!(view(&state).accounts, vec![AccountView { username: "1000005".into(), brew: true, telemetry: true, control: false }]);
        remove(State(state.clone()), Path("1000005".into())).await;
        assert!(view(&state).accounts.is_empty());
        assert!(std::fs::read_to_string(&path).unwrap().contains("[auth"), "config file written");
        crate::CONFIG_WRITE_APPLIED_LIVE.store(false, std::sync::atomic::Ordering::SeqCst);
        let _ = std::fs::remove_file(&path);
    }
}
