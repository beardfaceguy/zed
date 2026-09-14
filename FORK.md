# About this fork

This is [beardfaceguy/zed](https://github.com/beardfaceguy/zed), a fork of
[zed-industries/zed](https://github.com/zed-industries/zed).

Almost everything here is hardening of the **agent stack** — the Agent Client
Protocol (ACP) used to talk to external coding agents, and the Model Context
Protocol (MCP) context servers those agents rely on. The recurring themes are
lifecycle bugs (processes and connections that are never cleaned up or never
recovered), unbounded memory growth, and giving external agents less implicit
trust.

Work is tracked in Vikunja project **74 (Zed)**, which holds the upstream issue
and PR numbers for anything filed with Zed.

## Changes on `main`

Grouped by theme rather than commit order.

### Process and connection lifecycle

| Change | Why |
| --- | --- |
| Prevent Unix child process tree leaks | Agent and MCP child processes outlived the Zed window that spawned them. |
| Auto-cancel hung external agent tool calls | A tool call that never returned hung the session with no recovery. Upstream issue [#56734](https://github.com/zed-industries/zed/issues/56734), PR [#56736](https://github.com/zed-industries/zed/pull/56736). |
| Resume MCP SSE streams after abrupt termination | A dead HTTP SSE stream silently hung every in-flight request. Upstream issue [#56775](https://github.com/zed-industries/zed/issues/56775), PR [#56776](https://github.com/zed-industries/zed/pull/56776). |
| Reset the MCP request idle timer on inbound activity | Requests were cancelled mid-flight despite the server still sending progress notifications. Upstream issue [#56774](https://github.com/zed-industries/zed/issues/56774), PR [#56773](https://github.com/zed-industries/zed/pull/56773). |

### Memory

| Change | Why |
| --- | --- |
| Bound pending terminal output | ACP terminal scrollback buffers were never freed after a tool call exited, reaching 60–69 MB in a two-minute session against an 83 MB total-leak baseline. Upstream issue [#57099](https://github.com/zed-industries/zed/issues/57099). |
| Regression tests for terminal scrollback growth, and a display-only scrollback test fixed against the current API | Keeps the above from silently returning. |

### Trust boundaries for external agents

| Change | Why |
| --- | --- |
| Sandbox external ACP terminal requests (PR [#1](https://github.com/beardfaceguy/zed/pull/1)) | An external agent could ask Zed to run terminal commands with no confinement. |
| Scoped permission rules for external ACP agents (PR [#2](https://github.com/beardfaceguy/zed/pull/2)) | Permission decisions were all-or-nothing. |
| Setting to auto-approve external ACP tool calls | Opt-in convenience for trusted agents. Upstream PR [#56722](https://github.com/zed-industries/zed/pull/56722) was closed for missing a linked issue and formatting problems — the origin of the PR hygiene rules in `AGENTS.md`. |

### Reconnection and cleanup (merged from PRs #7-#9)

Merged into `main` together, which also advanced upstream from 2026-07-15 to
2026-07-20.

| Change | Why |
| --- | --- |
| Restart dead ACP and context server connections (PR [#7](https://github.com/beardfaceguy/zed/pull/7)) | An ACP connection was never retired or respawned after a transport failure, so every later `session/new` failed with "oneshot canceled". Adds restart-on-demand, atomic shutdown-receiver claiming, and crash-backoff reset after stability. |
| Reap stdio MCP servers that exit on their own | A server with an idle-shutdown watchdog exits while the transport is still alive; `async-process` only reaps the child when its handle drops, so each one sat in `Z` state for the life of the window. |
| Local security Git hooks (PR [#8](https://github.com/beardfaceguy/zed/pull/8)) | Committed pre-commit and pre-push guards, bounded scan runtimes, and a reusable release build helper. Verified by `script/test-git-hooks`. |
| Update vulnerable locked dependencies (PR [#9](https://github.com/beardfaceguy/zed/pull/9)) | Refreshes `Cargo.lock` and `uv.lock` against advisory findings. |

PR #9 also carried a squashed copy of PR #8. The content was identical so the
merge was conflict-free, but the hook scripts appear twice in history.

### Agent capability support

| Change | Why |
| --- | --- |
| Include MCP server instructions in native agent context (PR [#3](https://github.com/beardfaceguy/zed/pull/3)) | Server-provided instructions were being dropped. |
| Advertise a parameterized model picker to all ACP agents (PR [#4](https://github.com/beardfaceguy/zed/pull/4)) | Model selection was limited to specific agents. |
| Update Agent Client Protocol to 1.2.0 (PR [#5](https://github.com/beardfaceguy/zed/pull/5)) | Protocol version bump. |
| Expose retry and truncation for capable ACP agents (PR [#6](https://github.com/beardfaceguy/zed/pull/6)) | Surfaces capabilities agents already advertised. |

## Work not on `main`

| Branch | Contents |
| --- | --- |
| `integration/upstream-2026-07-20`, `integration/upstream-2026-08-12` | Upstream sync lineage (PR [#10](https://github.com/beardfaceguy/zed/pull/10)). PRs #7-#9 originally targeted the first of these rather than `main`, which is why they sat unmerged. |
| `resubmit/*` | Upstream PRs being refiled with better descriptions and linked issues. |

## In progress

| Branch | Contents |
| --- | --- |
| `fix/cgroup-fallback` | Two fixes, currently uncommitted. Spawning a process tree fails with permission denied when the login session scope is root-owned, because neither creating a child cgroup nor attaching a transient systemd scope is permitted; this degrades to Unix process-group sessions with a warning instead of failing the agent spawn. Separately, `session/new` could race ahead of `ContextServerStore` reconciliation and hand the agent an empty `mcpServers` list, so session creation now waits on a readiness signal. |
| `feat/agent-explain-popover` | Select text in an agent chat, right-click **Explain**, and get a draggable popover answered by an isolated agent session that does not pollute the main thread. Tracked in Vikunja project 199. Spun out as a standalone desktop tool at [beardfaceguy/mimir](https://github.com/beardfaceguy/mimir). |

### The cgroup fallback blocks tests, not just agents

On a host whose login session scope is root-owned, two tests fail before they do
any real work, because neither can spawn a child process at all:

- `context_server`: `server_that_exits_on_its_own_is_reaped`
- `agent_servers`: `startup_returns_error_when_agent_exits_before_initialization`

Both fail with `failed to create cgroup ...: Permission denied` and
`current cgroup is not within app.slice`. Applying the `fix/cgroup-fallback`
change to `crates/util/src/process.rs` turns both green. Treat a failure in
either as an environment problem until that branch lands.

Rebasing that branch onto current `main` is mostly clean — `process.rs`,
`acp.rs` and the install scripts all apply as-is. Only
`crates/project/src/context_server_store.rs` conflicts, because PR #7 rewrote it.

## Upstreaming

Most of this is intended to go upstream; several items already have Zed issues
and PRs, linked above. Before opening anything against `zed-industries/zed`, read
the PR hygiene rules in [`AGENTS.md`](./AGENTS.md) — file an issue first, and
check the PR body against the render checklist in Vikunja task 387. PR
[#56722](https://github.com/zed-industries/zed/pull/56722) was closed on exactly
those grounds.
