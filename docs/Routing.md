# Routing: DGNA and talkgroup pools

The **Routing** page of the dashboard (new UI, sidebar **Network -> Routing**)
does two things:

- assign a group to several radios at once with DGNA, and
- connect talkgroups inside the server (talkgroup pools).

Everyone logged in sees the page. Changing anything needs an admin login.

## DGNA to several radios

The table lists every radio registered on this server: ISSI, station and
cell, the groups Brew knows for it, the groups the station reported (with
names) and the last DGNA answer.

1. Tick the radios (or the box in the header for all of them).
2. Enter the **GSSI** and, if you like, a **group name** (up to 15
   characters; the radio shows it).
3. Choose **Assign** or **Remove**, and the attachment mode (0 to 5; leave it
   at 0 unless you need another one).
4. Press **Send**.

Each radio's command goes over the control channel to the Basestation it
registered on last. That needs:

- the Basestation's `[telemetry]` link, so the server knows which station the
  radio is on, and
- its `[control]` link, which carries the command.

With only one control connection the command goes there. A radio that cannot
be reached shows the reason right away; the radio's own answer arrives over
telemetry and shows in the **Last DGNA** column (for example "assigned 2329"
or "rejected 2329" with the station's reason).

DGNA reaches radios only. App and console terminals are listed but cannot be
selected.

## Talkgroup pools

A pool connects talkgroups: a call on one of them is played 1:1 on the others,
with voice and the end of each over.

```toml
[[talkgroup_pools]]
name = "Region East"
enabled = true
both_ways = true              # false: only calls on the first group are copied
groups = [2321, 2323, 2324, 2329, 264]
```

- **Name:** shown on the page and on the calls. Up to 40 characters, unique.
- **Groups:** at least two, each 1 to 16777215, no group twice.
- **Direction:** `both_ways = true` copies a call on any of the groups to all
  the others. `both_ways = false` copies only calls on the **first** group to
  the others; calls on the others stay on their own group.
- **On/off:** `enabled = false` keeps the pool but does nothing.

On the page every pool has a switch, **Edit** and **Delete**; **New pool**
adds one. A change applies at once and is written to the config file without
a restart. Calls that are already running keep their copies until they end.

### Behaviour

- The copy of a call ("leg") uses its own call identifier on each target
  group, with the talker's ISSI and priority. Stations on a target group see
  an ordinary group call.
- A target group that already has its own call is not interrupted. The pool
  waits until that over ends and joins it then; the page shows the group as
  "waiting".
- A leg never starts further legs, so pools that share a group do not loop.
  A group in two pools gets the copy once.
- When the talker's over ends, all its legs end too.
- Pool legs are not sent to federation peers; the original call goes there as
  before.
- Without `[[talkgroup_pools]]` nothing changes: the server routes as it did.

## Choosing the dashboard

The redesigned dashboard is the default. To keep the classic one at `/`:

```toml
[dashboard]
legacy_ui = true
```

Both stay reachable either way: the classic dashboard under `/classic`, the
new one under `/ui`.

## Logins and the config file

Two more pages of the new dashboard, both for admins:

- **Network -> Accounts:** the logins Basestations and terminals connect with.
  One login can be on Brew (the station's or terminal's number, 1 to 7
  digits), telemetry and control at once. Changes apply to the next
  connection without a restart and are written to the config file;
  connections already up stay until they reconnect. Adding the first
  telemetry or control login turns the login on for that listener: stations
  without one are refused from then on.
- **System -> Configuration:** the whole config file. Save checks it and
  applies it with a restart (running calls drop, stations reconnect, you sign
  in again).
