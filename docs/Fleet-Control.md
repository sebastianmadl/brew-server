# Fleet Control

Fleet Control lets brew-server (the SwMI) manage the Basestations (FlowStation)
connected to it: see which stations allow it, their software and state, and
later change their configuration, update and restart them. It runs over the
existing Basestation control channel (WebSocket, subprotocol
`bluestation-control-v1`, `[control]`), so it works behind NAT and needs no
new port.

This document is the protocol and behaviour reference for phase 1
(foundation) and phase 2 (remote update, section 8). Later phases add
commands to the same scheme; each new command is announced as a capability
first, so older stations and servers keep working.

## 1. Principles

- **Opt-in per station.** A station is managed only when its local
  administrator switches Fleet Control on (FlowStation: Infrastructure page,
  under the Brew line, "Fleet Control — Station management by SwMI
  (BrewServer)", default off). Other stations on the same server are not
  touched.
- **Backwards compatible.** The server offers Fleet Control with
  `X-Brew-Extensions`. A station that sees no offer sends nothing new; a
  server that does not know the messages never gets them. Unknown messages
  are ignored on both sides.
- **Existing commands are independent.** Kick, DGNA, SDS, live SDS, emergency
  clear, ambience listening, service restart and shutdown work as before,
  whatever the switch says. Only Fleet Control commands need the switch.
- **The station stays in charge of its configuration.** (From phase 5:) the
  server sends change requests, the station checks them like its own
  dashboard and goes back to the previous configuration if it loses the
  server afterwards.

## 2. Negotiation

### 2.1 Server offer

brew-server adds `fleet-control` to `X-Brew-Extensions` whenever `[control]
enabled = true` (no extra setting):

- in the HTTP 101 of the **control channel** (this is the offer the station
  acts on: it sends `StationHello` only on a control connection whose 101
  names `fleet-control`);
- in the HTTP 101 of the **Brew connection** of a Basestation (so the station
  can show its switch even before its control channel is set up; the switch
  then opens a dialog for the control channel).

```
X-Brew-Extensions: fleet-control
```

Same header syntax as the Brew extensions (`docs/Brew-Extensions.md` section
3.1): comma separated, trimmed, unknown names ignored.

### 2.2 Station capabilities

The station lists the Fleet Control commands it understands in
`StationHello.capabilities` while its switch is on (empty while off). The
server sends a Fleet Control command only to a station whose last hello
listed its capability.

| Capability | Commands | Phase |
| --- | --- | --- |
| `fleet-v1` | `FleetPing` | 1 |
| `update-v1` | `CheckUpdate`, `StartUpdate` (answers `UpdateInfo`, `CommandResult`; reports `UpdateProgress`) | 2 |
| `update-live-v1` | `StartUpdate` with `source` (live update, radio kept on; section 8.6) | 2 |
| `maintenance-v1` | `SetMaintenance` (red banner on the station's dashboard; section 9.1) | 3 |
| `restart-v1` | `RestartStation` (service restart or system reboot; section 9.2) | 3 |

Later phases add for example `config-v1`.

## 3. Messages

The control channel carries JSON in binary WebSocket frames, serde's
externally tagged form: a unit variant is a bare string (`"RestartService"`),
any other is `{"<Name>": {fields}}`. Server → station frames are commands,
station → server frames are responses.

### 3.1 `StationHello` (station → server)

Sent right after the control channel connects (when the server offered
`fleet-control`) and again every time the local switch changes, also when it
goes off.

```json
{"StationHello": {
  "protocol": 1,
  "version": "v0.5.9-8d7a363",
  "brew_login": "1000001",
  "mcc": 232,
  "mnc": 20,
  "fleet_control": true,
  "capabilities": ["fleet-v1"]
}}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `protocol` | u32 | Fleet Control protocol version, `1` |
| `version` | string | station software (`STACK_VERSION`) |
| `brew_login` | string or null | the station's Brew login (its number on the Brew server); null without Brew |
| `mcc`, `mnc` | u16 | network of the primary cell |
| `fleet_control` | bool | the local switch |
| `capabilities` | string list | Fleet Control commands it understands (section 2.2), empty while the switch is off |

A server must accept missing `brew_login`, `mcc`, `mnc`, `capabilities`
(defaults) and ignore unknown fields.

### 3.2 `CommandResult` (station → server)

The general answer to every Fleet Control command, correlated by `handle`.

```json
{"CommandResult": {"handle": 17, "ok": false, "code": "fleet_control_off", "message": "Fleet Control is switched off on this station"}}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `handle` | u32 | the command's handle |
| `ok` | bool | done |
| `code` | string | machine-readable outcome (below) |
| `message` | string | short English text for the dashboard |

Codes in phase 1: `ok`, `fleet_control_off` (the switch is off; the station
refuses every Fleet Control command). Phase 2 adds `busy` (an update is
already running), `call_active` (a call is in progress) and `unavailable`
(the station cannot do it, e.g. no OTA without its dashboard). Later phases
add codes such as `invalid_value`.

### 3.3 `FleetPing` (server → station)

Round-trip check (capability `fleet-v1`).

```json
{"FleetPing": {"handle": 17}}
```

Answer: `CommandResult` with `ok: true, code: "ok"` while the switch is on,
else `ok: false, code: "fleet_control_off"`. The station's control worker
answers it itself; it never reaches the radio stack.

## 4. Station behaviour (FlowStation)

- Switch: `[command] fleet_control = true|false` (default false). The
  dashboard changes it at run time: the config file is written and the
  running station uses the new value at once (no restart); the control worker
  sends a new `StationHello` within about a second.
- Control channel not configured (no `[command]`, or `enabled = false`) when
  the switch is turned on: a dialog asks for server (IP or DNS name, never
  pre-filled), port (443), TLS (on), user (pre-filled with the Brew login) and
  password (empty = take the Brew password, on the station; the password never
  goes to the browser). Saving writes `[command]` with `fleet_control = true`
  and restarts the service, because the control channel starts at boot.
- Sidebar (under Health): field "Fleet Control", green "on" / red "off",
  shown only while the server offers Fleet Control.
- Code: FlowStation `net_control/fleet.rs` (messages, answers),
  `net_control/worker.rs` (hello, switch changes), `net_dashboard/fleet.rs`
  (switch, dialog), documented in FlowStation `Docs/fleet-control.md`.

## 5. Server behaviour (brew-server)

### 5.1 Fleet Manager

Dashboard page **Fleet Control** (Infrastructure). Stations are keyed by their
**control login** (the user the station logs in with on the control channel;
kept identical to its Brew login). A station that shares a control login with
another station is seen as one.

- A hello with `fleet_control: true` adds or updates the station (software,
  network, capabilities). The name shown is the station's name on this server
  (Accounts), else its Brew number, else the control login.
- A station whose control channel drops stays listed as **offline** with the
  time it was last seen.
- A hello with `fleet_control: false` removes a listed station: at once when
  it switches off while online, else at its next login. The change log gets
  `Basestation <name> switched off Fleet Control`, and the dashboard shows
  this as an INFO popup (once per entry and browser; on the first visit only
  entries of the last 24 h).
- **Remove** (Fleet Control management, offline stations only; online ones show **Ping**) takes a station off the list; it comes
  back with its next hello while its switch is on.
- **Ping** (Fleet Control management) sends `FleetPing` and shows the result
  and the round-trip time.

### 5.2 Change log

Every change is logged with time, station, who (dashboard user, or the station
itself) and text, and kept **90 days**:

| Kind | When |
| --- | --- |
| `fleet_on` | a station switched Fleet Control on (first hello with the switch on) |
| `fleet_off` | a listed station switched Fleet Control off |
| `version` | a listed station came back with other software |
| `removed` | a user removed a station from the list |
| `update` | update run started / cancelled, a station started, waits for a call, restarts, moved to the end of the queue (phase 2) |
| `update_done` | a station is back with the new software |
| `update_alarm` | a station failed (shown red): refused, build failed, 45 min build limit, not back 5 min after the restart, back with the old software |
| `update_skip` | a station was not updated: up to date, offline twice, cancelled |
| `update_end` | the run is over, with the counts |
| `settings` | an admin changed "Updates repo" |
| `log_clear` | an admin cleared the change log (the only entry left after **Clear**) |
| `maintenance` | maintenance on (minutes, note) / off / ended (time up) (phase 3) |
| `restart` | service restart / system reboot sent; station back after it (with the seconds) (phase 3) |
| `restart_alarm` | a station not back 10 min after a restart / reboot (shown red) |

Phases 3–7 add configuration changes and restarts.

**Clear** (admins only, next to the search field) empties the change log after
a confirmation; one entry "Log cleared by <user>" stays.

Stations and log are kept in `<[storage] path>.fleet` (JSON, rewritten through
a temporary file) and survive restarts; without `[storage] enabled` they are
kept in memory only.

### 5.3 Rights

Dashboard users (Server settings → Users), next to the page ticks:

| Right | Page id | Allows |
| --- | --- | --- |
| Fleet Control visibility | `fleet` | see the Fleet Manager and the change log |
| Fleet Control management | `fleet_manage` | visibility plus every button (Ping, Remove, Check for updates, Update, Cancel, Maintenance, Restart; later config) |

Admins have both. Viewer (not logged in) may get visibility, never
management. Neither right is given to users taken over from the config.

### 5.4 HTTP API

| Route | Right | Body | Answer |
| --- | --- | --- | --- |
| `GET /api/fleet` | visibility | – | `{offered, protocol, can_manage, stations: [...], log: [...]}` |
| `POST /api/fleet/ping` | management | `{"id": "<control login>"}` | `{ok, code, message, ms}`; 404 not connected, 504 no answer in 5 s |
| `POST /api/fleet/remove` | management | `{"id": "<control login>"}` | `{ok: true}`; 404 not listed |
| `POST /api/fleet/check` | management | `{"ids": [...]}` (empty: all) | `{results: [{id, check}]}` (section 8.3) |
| `POST /api/fleet/update` | management | `{"ids": [...], "mode": "parallel"\|"rolling", "during_call": bool, "interrupt_min": n, "live": bool}` | `{ok, job}`; 409 a run is going or no station; 400 live update without an https Updates repo |
| `POST /api/fleet/update/cancel` | management | `{}` (whole run) or `{"id": "<control login>"}` | `{ok: true}`; 404 nothing to cancel |
| `POST /api/fleet/settings` | admin | `{"repo": "...", "branch": "..."}` | `{ok: true}`; 400 not a git URL / branch |
| `POST /api/fleet/log/clear` | admin | `{}` | `{ok: true}` |
| `POST /api/fleet/maintenance` | management | `{"id", "on": bool, "minutes": n (default 30, 1–240), "reason": "..."}` | `{ok: true}`; 400 no `maintenance-v1`; 404 not connected; 409 refused by the station; 504 no answer |
| `POST /api/fleet/restart` | management | `{"id", "host": bool, "during_call": bool}` | `{ok: true}`; 400 no `restart-v1`; 404 / 409 / 504 as above |

A station in `stations`: `id`, `label`, `brew_login`, `version`, `mcc`, `mnc`,
`protocol`, `capabilities`, `online`, `first_seen_ms`, `last_seen_ms`, and
from phase 2 `check` (last update check or null), `progress` (last
`UpdateProgress` or null), `updatable`, `other_source`, `live` (online with
`update-live-v1`), from phase 3 `maintenance` (`{by, reason, since_ms,
until_ms}` or null) and `restart` (`{host, by, at_ms}` while a restarted
station is not back, or null). A log entry: `id`,
`at_ms`, `station`, `label`, `user` (null = the station or the server),
`kind`, `text`. Phase 2 adds `is_admin`, `job` (the current or last run, or
null), `update_settings`, `update_source` (in effect) and `limits` to
`GET /api/fleet`.

Code: `src/fleet.rs` (Fleet Manager, log, API), `src/control.rs` (messages),
`src/fsnet.rs` (offer on the control channel), `src/server.rs` (offer on the
Brew connection), `src/dash_users.rs` (rights), `src/dashboard_ui.rs` (page).

## 6. Backwards compatibility

| Server | Station | Result |
| --- | --- | --- |
| old (no offer) | new | the station sends no hello; switch hidden; everything as before |
| new | old | no hello arrives; the station is not in the Fleet Manager; existing commands work |
| new, `[control]` off | new | not offered; switch hidden |
| new | new, switch off | hello with `fleet_control: false`; the station is not listed |

An old server that got a hello anyway (a third-party station sending it
without the offer) logs `dropping malformed control response` and carries on;
nothing breaks.

## 7. Security

- Use TLS for the control channel (`wss://`, port 443 by default in the
  dialog).
- Passwords are never sent back to a browser; an empty password field means
  "unchanged" (or "take the Brew password" in the dialog).
- Key material (K, SCK, CCK) will never travel through the configuration
  commands; keys get their own phase (key management, server as
  authentication centre, EN 300 392-7 clause 4.1.1).

## 8. Remote update (phase 2, `update-v1`)

### 8.1 Principles

- **The station keeps its own update source.** Each station updates from its
  own OTA source (FlowStation `[dashboard] ota_channel`: by default the
  repository and branch it was installed from, forks included). The server
  only says "check" and "update"; it never chooses a repository or branch.
  Moving a station to another source is a configuration change (phase 5, by
  hand per station: "apply OTA settings from the server"), never automatic.
- **Only after a check.** A station can be updated only when its last check
  (`CheckUpdate`), made with the software it runs now, found a newer version.
  When its turn comes it fetches the newest version again, so a rolling run
  always installs what is newest at that moment.
- **The station is off the air while it builds.** FlowStation's OTA frees the
  SDR for the build (10–20 min on a Raspberry Pi), then installs and
  restarts. This is why rolling runs exist.

### 8.2 Messages

Server → station (only to stations whose hello listed `update-v1`):

```json
{"CheckUpdate": {"handle": 9}}
{"StartUpdate": {"handle": 10, "during_call": false}}
```

Station → server:

```json
{"UpdateInfo": {"handle": 9, "ok": true, "current": "v0.5.9-8d7a363", "latest": "v0.6.0-1a2b3c4", "update_available": true,
  "channel": "installed", "repo": "https://github.com/sebastianmadl/bost-flowstation", "branch": "fleet-control", "message": "newer version available"}}
{"CommandResult": {"handle": 10, "ok": true, "code": "ok", "message": "Update started"}}
{"UpdateProgress": {"stage": "build", "percent": 42, "message": "   Compiling tetra-entities v0.1.0", "version": "v0.5.9-8d7a363", "elapsed_secs": 312}}
```

| Message | Fields |
| --- | --- |
| `CheckUpdate` | `handle`. The station checks its own source, fresh (no cache); it asks GitHub, so the answer can take some seconds (the server waits up to 60 s) |
| `UpdateInfo` | `handle`, `ok` (the check reached the repository), `current`, `latest` (or null), `update_available`, `channel` (`installed` / `stable` / `beta` / `custom`), `repo` (never with credentials), `branch`, `message` |
| `StartUpdate` | `handle`, `during_call` (default false: refuse with `call_active` while a call is in progress). Answer `CommandResult`: `ok`, `busy`, `call_active`, `unavailable`, `fleet_control_off` |
| `UpdateProgress` | `stage`: `download`, `build`, `install`, `restart`, `up_to_date`, `failed`; `percent` 0–100 (estimated in `build`); `message` (last log line); `version` (running software, the old one until the restart); `elapsed_secs`. Sent on every change of stage or percentage, at least every 10 s while running, also for updates started on the station's own dashboard |

The end of an update is the next `StationHello` after the restart: other
`version` = updated.

### 8.3 Check for updates

**Check for updates** (Fleet Control page) sends `CheckUpdate` to every
online station with `update-v1` (only the ticked ones, if any) and stores each
answer with the station (`check`). The table shows "Newer: <version>", "Up to
date" or "Check failed". A check is dropped when the station comes back with
other software.

**Updates repo** (Server settings, admins): repository URL and branch, free to
choose (e.g. a fork with `handover`); empty = `https://github.com/Aitorrio/bost-flowstation`,
`main`. It is a reference only: a station whose checked source differs (other
repository, compared without case, `.git`, trailing slash or credentials, or
another branch) gets the hint **⚠ other repo/branch** with both sources in the
tooltip.

### 8.4 Update run

**Update all** (nothing ticked) or **Update (n)** (ticked stations), or
**Update** on one station, opens a dialog:

| Field | Meaning |
| --- | --- |
| Rolling (one after another) | the next station starts only when the previous one is back with the new software (or failed / skipped); the network is never fully off the air |
| Parallel (all at once) | every station starts at once |
| Update during call | start at once, a running call is cut |
| Interrupt a call after (min) | without "Update during call": wait at most this long for a call to end (default 10, at most 120), then start anyway |

One run at a time. The run lives on the server (`fleet_update.rs`, driven
every 2 s by `fleet::run_updates`): it goes on with the browser closed and
survives a server restart (saved in `<storage path>.fleet`; a start whose
answer never came is sent again after the restart).

Per station:

| State | Next |
| --- | --- |
| up to date at the start (no newer version found) | skipped at once |
| offline (or no answer) when its turn comes | moved to the end of the queue once; offline again: skipped |
| `call_active` | tried again every 10 s; after the interrupt time with `during_call: true` |
| started (`ok`, or `busy`: an update was already running there and is followed) | follows `UpdateProgress` |
| no `restart` within **45 min** of the start (build limit) | ALARM, skipped |
| `restart` | waits for the station; not back within **5 min**: ALARM, skipped |
| back with other software | updated |
| back with the same software after the restart | ALARM, skipped |
| `up_to_date` | skipped |
| `failed`, or any other refusal (`unavailable`, `fleet_control_off`) | ALARM, skipped |

A failed station never stops the run: it is marked red in the table and in
the change log and the next station starts.

**Cancel all** (shown next to Update while a run goes) starts no further
station; one that is updating finishes. **Cancel** on a station takes it out
of the run while it has not started yet (queued or waiting for a call).

The Fleet Control page shows the run (mode, counts, the queue with each
station's state) and, while the run goes, per station the stage with a
progress bar, refreshed every 3 s. After the run the Update column is empty
again; the run card keeps the results.

### 8.5 Backwards compatibility

| Server | Station | Result |
| --- | --- | --- |
| phase 1 | phase 2 | the server ignores `update-v1`; no update commands are sent; `UpdateProgress` frames are dropped as unknown (logged, nothing breaks) |
| phase 2 | phase 1 | no `update-v1` in the hello: the station is never checked or updated (not counted in Update all) |
| phase 2 | phase 2, no dashboard | no `update-v1`, same as above |

### 8.6 Live update (`update-live-v1`)

**Live update (RF on)** in the update dialog (shown for stations that list
`update-live-v1`): the server sends the repository and branch of its
**Updates repo** with `StartUpdate`:

```json
{"StartUpdate": {"handle": 12, "during_call": false, "source": {"repo": "https://github.com/sebastianmadl/bost-flowstation", "branch": "fleet-control"}}}
```

The station runs its installer with them, the same as
`sudo BOST_REPO=… BOST_BRANCH=… bash contrib/install/install-bost.sh` in a
terminal, and **stays on the air while it builds**; only the service restart
at the end takes it off the air (about 2 min). Afterwards the station's own
update source is that repository and branch.

- Rolling and parallel, call wait, limits, alarms and Cancel work as in 8.4.
- No check is needed first (the station may change source); every ticked
  station (or, with nothing ticked, every online station with
  `update-live-v1`) takes part.
- Back with the same software after the restart counts as done
  ("live update finished, software unchanged"), not as an alarm.
- Only a plain `https://` repository without credentials and a plain branch
  name are sent (400 otherwise; the station checks again and answers
  `invalid_source`). An ssh Updates repo cannot be used for live updates.
- `source` is sent only to stations that list `update-live-v1`: an older
  station would ignore the field and run a normal OTA (radio off).

Code: `src/fleet_update.rs` (run state, pure), `src/fleet.rs` (checks,
settings, API, driver), `src/control.rs` (messages), `src/dashboard_ui.rs`
(page, dialog, Server settings card). Station side: FlowStation
`net_dashboard/ota_remote.rs`, `net_control/fleet.rs`, `Docs/fleet-control.md`.

## 9. Maintenance and restart (phase 3)

### 9.1 Maintenance (`maintenance-v1`)

```json
{"SetMaintenance": {"handle": 3, "on": true, "minutes": 30, "by": "oe4sxm", "reason": "new carrier"}}
```

Answer `CommandResult` (`ok`, `fleet_control_off`). While the flag is on,
every page of the station's dashboard shows a solid red banner under the top
bar: "Maintenance: work on this base station in progress", with `by`, the
minutes left and `reason`. **Maintenance** on a station (Fleet Control page,
management) asks for the duration (default 30 min, 1–240) and a note; **End
maintenance** switches it off. The flag runs out by itself on both sides, so a
closed browser never leaves it on. The server keeps it in the Fleet file and
sends it again (with the time left) when the station connects again, e.g.
after a restart. The table shows a red chip "Maintenance · user · n min left".

Phase 5 (config editing) will set it automatically while the edit dialog is
open and refresh it while the dialog stays open.

A live update (8.6) shows its own banner on the station ("Live system update:
RF on!") without this flag.

### 9.2 Restart (`restart-v1`)

```json
{"RestartStation": {"handle": 4, "host": true, "during_call": false}}
```

Answer `CommandResult`: `ok` (the station restarts its service, `host` false,
or reboots the computer, `host` true, 5 s later), `busy` (an update is
running), `call_active` (a call is in progress and `during_call` is false),
`fleet_control_off`. **Restart** on a station opens a dialog: *Restart
service* / *Reboot system*, *Restart during call*, then a confirmation. The
change log records who sent it and, from the next `StationHello`, "back after
the reboot (n s)"; a station not back within **10 min** gets an ALARM
(`restart_alarm`). The station needs no sudo rule: FlowStation's service runs
as root.

### 9.3 Backwards compatibility

A station without `maintenance-v1` / `restart-v1` gets no buttons and the API
refuses with 400; a phase 2 server never sends the new commands.

