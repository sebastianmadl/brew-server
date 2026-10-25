# Late entry for group calls

A group call's participants are fixed when its `GROUP_TX` arrives, and voice
frames go only to those connections. Without late entry, a Basestation that
affiliated to the group during the over heard nothing until the next one.

## What happens

When a connection sends `REGISTER`, `REREGISTER` or `AFFILIATE`, brew-server
adds it to every running group call to the ISSI's groups that does not reach it
yet, and sends it that call's `GROUP_TX`.

- The `GROUP_TX` that set the call up is kept per call and dropped when the
  call ends. A call set up without one (SIP bridge) gets a synthesized header.
- The header goes out in the connection's own layout (v0 or v1). A dispatch
  console gets an emergency call with raised priority.
- It is sent while the router holds its lock, so no voice frame of the call
  can reach the new participant ahead of it.
- Federation peer links are left to their routes. The talker's own connection
  and connections already in the call are skipped.

This is also what an announced cell reselection between two Basestations
needs: the target Basestation registers the ISSI and joins its call before the
radio arrives.

There is nothing to configure.
