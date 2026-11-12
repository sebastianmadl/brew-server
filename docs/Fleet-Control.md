# Fleet Control

Fleet Control lets brew-server (the SwMI) manage the Basestations (FlowStation)
connected to it: see which stations allow it, their software and state, and
later change their configuration, update and restart them. It runs over the
existing Basestation control channel (WebSocket, subprotocol
`bluestation-control-v1`, `[control]`), so it works behind NAT and needs no
new port.

This document is the protocol and behaviour reference for phase 1
(foundation). Later phases add commands to the same scheme; each new command
is announced as a capability first, so older stations and servers keep
working.

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

Later phases add for example `update-v1`, `reboot-v1`, `config-v1`.

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
refuses every Fleet Control command). Later phases add codes such as
`invalid_value`, `call_active`, `busy`.

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
- **Remove** (Fleet Control management) takes a station off the list; it comes
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

Phases 2–7 add configuration changes, updates and restarts.

Stations and log are kept in `<[storage] path>.fleet` (JSON, rewritten through
a temporary file) and survive restarts; without `[storage] enabled` they are
kept in memory only.

### 5.3 Rights

Dashboard users (Server settings → Users), next to the page ticks:

| Right | Page id | Allows |
| --- | --- | --- |
| Fleet Control visibility | `fleet` | see the Fleet Manager and the change log |
| Fleet Control management | `fleet_manage` | visibility plus every button (Ping, Remove; later update, restart, config) |

Admins have both. Viewer (not logged in) may get visibility, never
management. Neither right is given to users taken over from the config.

### 5.4 HTTP API

| Route | Right | Body | Answer |
| --- | --- | --- | --- |
| `GET /api/fleet` | visibility | – | `{offered, protocol, can_manage, stations: [...], log: [...]}` |
| `POST /api/fleet/ping` | management | `{"id": "<control login>"}` | `{ok, code, message, ms}`; 404 not connected, 504 no answer in 5 s |
| `POST /api/fleet/remove` | management | `{"id": "<control login>"}` | `{ok: true}`; 404 not listed |

A station in `stations`: `id`, `label`, `brew_login`, `version`, `mcc`, `mnc`,
`protocol`, `capabilities`, `online`, `first_seen_ms`, `last_seen_ms`. A log
entry: `id`, `at_ms`, `station`, `label`, `user` (null = the station), `kind`,
`text`.

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
