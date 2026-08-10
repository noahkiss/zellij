# Assessment: a first-class HTTP/WS API on zellij's web server

Written 2026-07-30 against v0.44.3. **Verdict: do not build it. The capability that motivated it
already ships in v0.44.3 as a CLI/IPC surface, and a web API would be sugar over it at the cost of a
new remote-control attack surface in upstream's most active directory.**

## What was asked for

Three things, on the existing token-authed axum web server:

  (a) the session tree (tabs/panes) over HTTP
  (b) a websocket streaming per-pane content diffs
  (c) action execution over POST

The motivation was a web controller that drives zellij by shelling the CLI on a 1s poll plus a
doorbell plugin, with a planned improvement where a plugin subscribes to `PaneRenderReportWithAnsi`
and relays it to the backend over `WebAccess`.

## The finding that decides it

**(b) already exists, and it is push-based.** `zellij subscribe` (added in 0.44.x, implemented in
`zellij-client/src/cli_client.rs:255` as `start_subscribe_client`) subscribes over the existing
client/server contract and prints per-pane render updates as they happen:

```
zellij --session <name> subscribe --pane-id terminal_0 --pane-id plugin_3 --format json --ansi
```

Newline-delimited JSON, one object per update:

```json
{"event":"pane_update","pane_id":"terminal_0","viewport":[...],"scrollback":null,"is_initial":true}
```

Verified empirically on this fork's build: the initial snapshot arrives on subscribe, and further
events are pushed on every render with no polling. It works on plugin panes as well as terminal
panes, `--ansi` preserves styling, `--scrollback [N]` includes scrollback in the initial delivery,
and multiple `-p` flags multiplex several panes onto one stream from one process.

The machinery behind it is entirely in the existing contract:
`ClientToServerMsg::SubscribeToPaneRenders { pane_ids, scrollback, ansi }`
(`zellij-utils/src/ipc.rs:151`) and `ServerToClientMsg::PaneRenderUpdate { pane_id, viewport,
scrollback, is_initial }` / `SubscribedPaneClosed { pane_id }` (`:209`, `:215`), emitted from
`zellij-server/src/screen.rs:5324` and `:5431`.

So the relay-plugin design is unnecessary, and so is the poll. A consumer spawns one long-lived
child process and reads its stdout.

**(a) already exists.** `zellij action list-panes --json -s -t` returns a rich flat object per pane:
`id`, `is_plugin`, `title`, `plugin_url`, `is_focused`, `is_floating`, `is_suppressed`,
`is_fullscreen`, `is_selectable`, `exited`, `exit_status`, `pane_x/y`, `pane_rows/columns`,
`pane_content_*`, `cursor_coordinates_in_pane`, `default_fg/bg`, `index_in_pane_group`, `tab_id`,
`tab_name`. `list-tabs --json` returns `Vec<TabInfo>`. Suppressed (hidden) panes are included.

**(c) already exists.** The whole `zellij action` set is CLI-dispatchable and is what the controller
already uses.

## What a web API would and would not add

It would add: HTTP/WS shape instead of subprocess shape, and browser-reachability without a shell on
the box. It would **not** add a single capability that is not already reachable.

Against that: a new authenticated remote-control endpoint that can execute arbitrary actions in the
user's terminal is a materially larger blast radius than a CLI a local process already has, and it
lives in the directory upstream is most actively changing.

## The seams, if it is ever built anyway

Costed per requirement, from source. None of these need a protobuf change — which was the main open
question, and the answer is favourable.

| Piece | Seam | Cost |
|---|---|---|
| Pane content WS | Web server is already a client with an IPC channel; send `SubscribeToPaneRenders`, forward `PaneRenderUpdate` to a browser socket | Small. No contract change. The forwarding shape mirrors `ws_handler_terminal`. |
| Session tree HTTP | Dispatch `ClientToServerMsg::Action { action: Action::ListPanes { output_json: true }, .. }`; the response returns as `ServerToClientMsg::Log { lines }` via `send_output_to_client` (`zellij-server/src/route.rs:3103`) | Small but ugly — the JSON arrives as log lines and has to be reassembled. A clean version wants a typed response message, which *would* touch the contract. |
| Action POST | `ClientToServerMsg::Action` carries the full `Action` enum | Small. No contract change. |
| Auth | `auth_middleware` is already applied with `route_layer` in `zellij-client/src/web_client/mod.rs:235-245`; routes registered before that layer inherit it | Free, provided new routes are added **above** the `.route_layer(...)` call. |

The real cost is not the seams, it is the connection lifecycle: today client connections to the
zellij server are created per browser terminal websocket (`POST /session` → `create_new_client`).
An API route needs its own connection or a shared multiplexed one, which is new lifecycle code with
its own failure modes (reconnect, session death, orphaned subscriptions).

Rough size for a minimal slice (tree endpoint + pane websocket): a new module in
`zellij-client/src/web_client/`, roughly 400-600 LOC with tests, plus browser-side verification.

**Rebase cost.** Between `v0.44.3` and `upstream/main` (56 commits total) the relevant directories
moved: `zellij-client/src/web_client` 6, `zellij-client/assets` 3, `zellij-server/src/route.rs` 8,
`zellij-utils/src/ipc.rs` 3. The web client is proportionally the most active area of the codebase
and is on a feature track (PWA, mobile layout, nested sessions), so a fork patch there is the most
expensive kind to carry.

## Security constraints if revisited

Non-negotiable, and recorded here so a future implementation does not have to re-derive them:

- Behind the existing web-server token auth, registered above the `route_layer` so the middleware
  actually applies. A route added below it is silently unauthenticated.
- Bound per the existing web-server config, never a separate listener with its own binding rules.
- Default off, gated on the web server being explicitly enabled.
- A private-network-only deployment posture is a reason the exposure is *acceptable*, not a reason to skip
  auth — the endpoint executes arbitrary actions in a live terminal.

## Recommendation

Consume the existing surfaces: one `zellij subscribe --format json` child process for content, and
`list-panes --json` / `list-tabs --json` plus `zellij action` for tree and mutations. Revisit a web
API only if a consumer appears that genuinely cannot spawn a process on the host — and even then,
prefer a thin external adapter over a fork patch in `web_client`.

## Revisited: a centralised consumer, and file reads

The trigger above came close to firing. The case: a consumer that stops deploying a component to
every machine and instead runs in one place, reaching each host remotely. Every host then has no
process belonging to that consumer — which is nearly the "cannot spawn a process on the host"
condition, except that SSH still can.

**Re-verified against the current tree, not the earlier notes.** The existing control websocket
cannot carry this today: `WebClientToWebServerControlMessagePayload` accepts only `TerminalResize`
and `TerminalMetrics`. There is no action dispatch, so nothing is reachable over the web surface as
it stands. The auth arrangement is unchanged and still as described — routes registered *above* the
`.route_layer(middleware::from_fn(auth_middleware))` call inherit it; the handful registered below
are deliberately public.

**One new requirement this case adds: reading files.** The consumer renders program logs written to
disk beside the session. Nothing in the tree reads arbitrary files on a client's behalf, so this is
wholly new work — path scoping, follow/tail semantics, streaming — call it 150-250 LOC on top of the
400-600 already costed, so **600-850 total**, in the directory with the highest rebase cost.

**A tempting argument that does not survive contact: "the terminal process already holds the file
access grants, so let it do the reading."** Measured, on a machine where the binary holds no Full
Disk Access at all: a dotfile directory under `$HOME` holding per-project logs was fully readable,
including opening a log file. Those paths are not in a protected category — the protected set is
Desktop, Documents, Downloads, iCloud, removable volumes and `~/Library`. **No grant is required by
anybody**, so grants cannot favour either design. The argument only becomes real for content inside
the protected set, where a separate consumer process would need its own grants — and would inherit
the same versioned-path fragility described in the file-access section of FORK.md.

**Connection loss is not a differentiator either.** A remote-command transport and a websocket both
drop and both need supervision plus a resubscribe. Recovery happens to be clean in both: `subscribe`
delivers an initial snapshot on connect (`is_initial`), so a reconnect yields current state with no
diff replay, no missed-event reconciliation and no sequence tracking. The failure mode is *briefly
stale*, never *silently diverged*. Note this logic gets written either way — the only question is
whether it lands in the consumer or in this fork, and "reconnect, session death, orphaned
subscriptions" was already identified above as the principal cost of building it here.

**The security step is the first one, not the last.** Exposing `Action` over HTTP grants arbitrary
code execution as the user, since pane creation takes a command. A file-read endpoint after that is
a marginal increment rather than a new category. So the decision is binary — authenticated remote
control, or none — and the constraints recorded above apply from the first route.

**Recommendation unchanged.** A remote-shell transport reuses an auth model the operator already
runs, costs nothing in this tree, and keeps the fork out of its most volatile directory. Revisit
only if that transport proves painful in practice — specifically connection-reuse latency, or the
log streaming — both of which are cheap to measure and are the same experiments either way.
