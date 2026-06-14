# Goose ACP session scoping

## Summary

For `goose acp` over stdio, one OS process hosts one `GooseAcpAgent`, and that agent can host many ACP sessions at once. Each `session/new` creates a fresh session id, session row, per-session agent state, working directory, and prompt stream; existing sessions stay alive until explicitly closed/deleted/archived. So for geesed, **one goosed per profile, N sessions per goosed** is the right default: goose already supports multi-session stdio natively, and geesed should only prefer one-process-per-tab if it wants stronger isolation for process-global state such as client capability negotiation, permission/inventory services, or any future global caches. (`crates/goose/src/acp/server.rs:199-215`, `crates/goose/src/acp/server.rs:2920-2936`, `crates/goose/src/acp/server/new_session.rs:13-110`)

## Per-question findings

### 1. Can one running `goose acp` process host multiple concurrent ACP sessions?

**Yes.** In stdio mode, `goose acp` builds one `GooseAcpAgent` for the process and then serves ACP on stdin/stdout (`crates/goose/src/acp/server.rs:2920-2936`). That agent keeps an in-memory `sessions: HashMap<String, GooseAcpSession>` keyed by session id (`crates/goose/src/acp/server.rs:199-215`). `session/new` always creates a new stored session and then inserts it into that map via `activate_acp_session` / `register_acp_session`; there is no singleton/session-already-exists check (`crates/goose/src/acp/server/new_session.rs:38-110`, `crates/goose/src/acp/server.rs:1135-1164`).

The ACP SDK also models repeated session creation on one connection: `ConnectionTo::build_session(...)` creates a new `NewSessionRequest`, and `start_session()` turns the response into a separate `ActiveSession` handle (`agent-client-protocol-0.14.0/src/session.rs:35-105`, `agent-client-protocol-0.14.0/src/session.rs:401-445`).

There is one concurrency caveat: goose only allows **one active run per session**. The guard is per-session (`active_run_id` inside that session entry), not per process, so multiple sessions can run independently even though one session cannot have two simultaneous prompts (`crates/goose/src/acp/server.rs:2181-2201`).

**Probe excerpt (one stdio child, two sessions):**

```text
$ cargo run -- /tmp/goose-bin/goose
session1=20260612_4
session2=20260612_5
s1_first=ALPHA
s2_first=BETA
s1_second=ALPHA
s2_second=BETA
```

### 2. If multiple sessions are supported, are they independent?

**Yes for conversation history, working directory, active-run state, and streamed outputs.** Goose’s own server comment says the ACP session id maps directly to a `sessions` database row, and the prompt handler treats the ACP session id as “the thread ID” (`crates/goose/src/acp/server.rs:151-157`, `crates/goose/src/acp/server.rs:2347-2591`). Working-directory updates are applied to the named session only, and only that session’s agent extension manager gets the updated cwd (`crates/goose/src/acp/server/manage_sessions.rs:4-31`).

For output routing, prompt notifications are emitted with the calling session id, and the transport layer can split outbound traffic into per-session streams (`crates/goose/src/acp/server.rs:2564-2570`, `crates/goose/src/acp/transport/connection.rs:170-247`).

**Probe excerpt (mock provider saw separate histories and separate cwd values):**

```text
{"message_count":4,"texts":[...,"Remember token ALPHA...","ALPHA","What token did I ask you to remember?..."],"reply":"ALPHA"}
{"message_count":4,"texts":[...,"Remember token BETA...","BETA","What token did I ask you to remember?..."],"reply":"BETA"}
```

The same probe also showed different per-session working directories in the request bodies (`/tmp/acp-scope/one` vs `/tmp/acp-scope/two`).

What is **not** fully isolated is process-global support state: the `GooseAcpAgent` itself shares capability negotiation `OnceCell`s, the `SessionManager`, `PermissionManager`, `ProviderInventoryService`, builtins, and additional source roots across all sessions in that process (`crates/goose/src/acp/server.rs:199-215`).

### 3. Does `session/new` ever return an error if a session already exists for the process, or if too many sessions are open?

**Not for either reason in the ACP server code inspected here.** `NewSessionRequest` does not carry a session id to “reuse”, and goose always creates a new session row inside `handle_new_session` (`agent-client-protocol/src/v1/agent.rs:911-950`, `crates/goose/src/acp/server/new_session.rs:38-110`). There is no “already have a session” branch and no count check before `register_acp_session` inserts into the ACP session map (`crates/goose/src/acp/server.rs:1135-1164`).

There *is* a separate `AgentManager` LRU cache with a default capacity of 100 agents (`crates/goose/src/execution/manager.rs:18-19`, `crates/goose/src/execution/manager.rs:35-64`). But when that cache fills, goose **evicts** an old cache entry instead of rejecting the new session (`crates/goose/src/execution/manager.rs:259-277`). That cache is not the same thing as the ACP session table: active ACP sessions are also held in `GooseAcpAgent.sessions`, so the code shown here does not impose a hard “max open ACP sessions” error path.

### 4. What happens to existing sessions when a new one is created?

**Nothing.** `handle_new_session` creates and activates the new session, but it does not close, delete, archive, or overwrite existing session entries (`crates/goose/src/acp/server/new_session.rs:38-110`). Existing sessions are only removed by explicit close/delete/archive flows (`crates/goose/src/acp/server.rs:2873-2890`, `crates/goose/src/acp/server/manage_sessions.rs:75-86`, `crates/goose/src/acp/server/manage_sessions.rs:171-183`).

The probe confirms this behavior in practice: after creating two sessions, both still answered their own second prompt with their own remembered token instead of being reset or merged.

### 5. Is there any per-process state shared across sessions, or is everything per-session?

**There is some of both.**

Per-process/shared in one `goose acp` child:

- the single `GooseAcpAgent` object itself, including client capability `OnceCell`s, builtins, source roots, `SessionManager`, `PermissionManager`, and `ProviderInventoryService` (`crates/goose/src/acp/server.rs:199-215`)
- the `AgentManager`, which owns a process-global LRU of session agents and an optional shared `default_provider` slot (`crates/goose/src/execution/manager.rs:35-64`, `crates/goose/src/execution/manager.rs:109-117`)

Per-session:

- each active ACP session’s `GooseAcpSession`, including its `agent`, tool-request bookkeeping, cancel token, and active run id (`crates/goose/src/acp/server.rs:160-167`, `crates/goose/src/acp/server.rs:1135-1164`)
- each session agent’s provider restore and extension/MCP loading, both keyed off the session id / stored session row (`crates/goose/src/execution/manager.rs:217-249`)
- session history, cwd, project id, and extension_data stored in the session record itself (`crates/goose/src/acp/server.rs:1068-1118`, `crates/goose/src/acp/server/manage_sessions.rs:4-31`)

So “provider connections, MCP servers, tool registrations” are **mostly per-session agents**, but they sit inside a process that still has meaningful shared services and caches.

## Recommendation for geesed

Use **one goosed per profile, N sessions per goosed**.

Why:

- `goose acp` stdio already supports repeated `session/new` on one connection.
- History/cwd/output isolation are session-scoped, which is the main thing tabs need.
- The cheap option avoids needless extra goose processes, repeated ACP initialization, and repeated model/provider/tool warm-up.
- There is no evidence here that geesed needs to rewrite session ids on the wire or maintain one-stdio-child-per-tab just to get correct session semantics.

Only prefer **one goosed per tab/connection** if you explicitly want stronger isolation for process-global state such as ACP capability negotiation, permission state, provider inventory/default-provider state, or future global caches. The middle “geesed multiplexes a single child because goose cannot” option does not look necessary from these findings.

## Risks / caveats

- This write-up inspects current goose source under `aaif-goose/goose` and probes the released `goose` CLI binary; ACP internals are moving fast, so re-run the probe when upgrading goose.
- The answer above is specifically for **`goose acp` over stdio**. Goose’s HTTP/WS transport is different: one process can host many connections, but `ConnectionRegistry::create_connection()` creates a fresh `GooseAcpAgent` per connection (`crates/goose/src/acp/transport/connection.rs:88-139`).
- Concurrency is session-scoped, not unlimited inside a session: one session can only have one active run at a time (`crates/goose/src/acp/server.rs:2181-2201`).
- The protocol baseline only requires support for the core session methods; it does not itself impose a one-session-per-process model (`agent-client-protocol/src/v1/agent.rs:3761-3769`).
