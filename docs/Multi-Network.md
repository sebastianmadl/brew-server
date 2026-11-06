# Multi Network: bridges to other Brew networks

brew-server can connect to other Brew networks (for example TetraPack or a
neighbouring operator) and bridge talkgroups across them. This is part 1 of
the Multi Network plan: talkgroup bridges. Radios of other networks on a
station (migration) and individual calls or SDS across networks come later.

## How it works

- For each other network this server opens **one outbound connection in
  plain Basestation mode**, with the Basestation login that network gave out.
  The far server needs nothing new: to it this server is one more
  Basestation.
- The connection introduces itself with a **free connection name**
  (`User-Agent`, e.g. `TMO-Austria Infra 1.0`). Empty: `brew-server/<version>`.
- **Credentials stay on this server.** Stations never get them.
- What crosses is set by the **talkgroup pools**: a pool may list groups of
  other networks next to this server's own groups. Such a pool is a bridge: a
  call on one member is played on the others, each as an ordinary group call
  on its own GSSI. GSSIs are translated (TG 9 here, TG 909 there); the
  talker's ISSI is passed on 1:1.
- A **bridge ISSI** (one ISSI of that network) is registered there and
  affiliated to the bridged groups, so that network's server sends their
  calls here. With `bridge_issi = 0` nothing is registered (only for a server
  that routes groups without affiliations).
- **Nothing else crosses**, in either direction: no registrations, no
  services, no calls on groups that are not bridged. A station cannot send
  into another network by itself.
- A **main switch** (`enabled`, on the Multi Network page next to "This
  server") closes all links at once; settings and pools stay.
- Networks are edited live (dashboard page **Multi Network**, admins only).
  Only a network whose settings changed reconnects; no restart. An empty
  password on save keeps the saved one.

## Config

```toml
[multi_network]
enabled = true            # main switch: false closes every link (default on)
name = "TMO-Austria"      # this server's network, for the dashboard
mcc = 901                 # its MNI (MCC 0..1023, MNC 0..16383), 0 = not set
mnc = 9999

[[multi_network.networks]]
name = "TetraPack"        # pools refer to the network by this name
enabled = true
mcc = 901
mnc = 9000
remote_host = "brew.example.net:443"
path = "/brew"
username = "1234567"      # Basestation login there
password = "secret"
user_agent = "TMO-Austria Infra 1.0"
bridge_issi = 9000001     # ISSI of the bridge in that network, 0 = none
reconnect_interval_seconds = 15
tls = true                # wss://, settings as for [[federation.peers]]
# tls_ca_path, tls_server_name, tls_pinned_cert_path

[[talkgroup_pools]]
name = "Bridge TP"
groups = [9]              # this server's groups
[[talkgroup_pools.remote]]
network = "TetraPack"     # groups of other networks
gssi = 909
```

On the dashboard the pool editor (Routing page) takes these groups as
`network:GSSI`, e.g. `TetraPack:909`.

With `both_ways = false` the first member is the source: the first of
`groups`, or the first `remote` group when `groups` is empty (for example to
listen to a group of another network on several groups here).

## Behaviour

- The pool rules apply unchanged (one talker per group, a group with its own
  call joins after it, legs end with the call).
- The keepalive of the links is `[federation] keepalive_interval_seconds` /
  `keepalive_timeout_seconds`.
- A network that is used by a pool cannot be deleted or renamed; remove its
  groups from the pools first.
- Whether the far server accepts a talker ISSI that is not registered there
  depends on that server. Registering the talkers there ("register all ISSI",
  with a filter and a collision check) is the next step.

## Standards

Brew is not an ETSI interface, and the ETSI inter-system interface (ISI,
EN 300 392-3) is not used: none of this is defined by ETSI. The only ETSI
values used are the identity sizes: GSSI 24 bits (TS 100 392-2 V3.10.1 clause
16.10.28, Table 16.60), MCC 10 bits and MNC 14 bits (clause 18.4.2.1, Table
18.17). Inside the server a group of another network is addressed by an alias
above the 24 GSSI bits (`slot << 24 | gssi`), so it can never collide with a
real group; stations never see it.
