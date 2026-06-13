# geesed v0

`geesed` v0 is deliberately small: it boots, listens on a control socket, answers a `status` ping, and shuts down cleanly. Profile CRUD and ACP routing land in later issues; see phlax/geese#10 for the sequence.

`geese status` can be pointed at a non-default daemon socket by setting `GEESED_SOCKET`.

## Profile CRUD (phlax/geese#16)

Profile CRUD (`profile.list`, `profile.get`, `profile.create`, `profile.delete`, `profile.lock`, `profile.unlock`, `profile.copy`) is now dispatched through `geesed` via JSON-RPC 2.0. The `geese` CLI exposes each verb (`geese list`, `geese new <name>`, `geese path <name>`, `geese delete <name>`, `geese lock <name>`, `geese unlock <name>`, `geese copy <src> <dest>`), all of which call `geese-client::ensure_running()` to autospawn the daemon when it isn't running. The storage root defaults to `$GEESE_ROOT` (or the platform data dir); tests pass an explicit root via `RunOpts::geese_root`. See phlax/geese#16 for the full design.

## Goose process management (phlax/geese#19)

`geesed` gains four new control-socket methods for spawning and tracking `goose acp` child processes. One `goose acp` process is maintained per profile; stdio is captured for future ACP proxy use but is unused in this version.

### Control-socket methods

| Method | Params | Result | Behaviour |
|---|---|---|---|
| `goosed.start` | `{ "name": "<profile>" }` | `{ "pid": <u32> }` | Spawn `goose acp` for the named profile. Idempotent: returns existing pid if already running. |
| `goosed.stop` | `{ "name": "<profile>" }` | `null` | SIGTERM the child; escalates to SIGKILL after 5 s. No-op if not running. |
| `goosed.kill` | `{ "name": "<profile>" }` | `null` | SIGKILL immediately. No-op if not running. |
| `goosed.list_running` | `{}` | `[ { "name", "pid", "started_at" } ]` | Enumerate running goose processes. |

### CLI surface

```
geese start <profile>   # goosed.start
geese stop <profile>    # goosed.stop
geese kill <profile>    # goosed.kill
geese ps                # goosed.list_running
```

### Goose binary resolution

The `goose` binary is resolved once at daemon startup in this order:

1. `GEESE_GOOSE_BIN` env var (absolute path or bare name searched in `PATH`)
2. `which("goose")` from `PATH`
3. `None` — geesed starts normally but `goosed.start` returns `-32010 GooseBinaryUnavailable`

Tests pass an explicit binary via `RunOpts::with_goose_bin(path)`.

## Working directory configuration (phlax/geese#25)

`geesed` now supports configuring the working directory (`cwd`) for each goose process. When `goosed.start` spawns a `goose acp` child, the child process's working directory is set to the *resolved* cwd rather than inheriting geesed's own process cwd.

### Resolution chain

`Storage::resolve_cwd(name)` walks five tiers in priority order:

1. **Per-profile `cwd` in `profile.toml`** — set via `profile.set_cwd`
2. **`GEESE_PROFILE_CWD_<NAME>` env var** — name is uppercased, hyphens become underscores (e.g. `GEESE_PROFILE_CWD_MY_PROFILE`)
3. **Global `cwd` in `$XDG_CONFIG_HOME/geese/config.toml`** (typically `~/.config/geese/config.toml`) — set via `config.set_global`
4. **`GEESE_CWD` env var** — global escape hatch
5. **`dirs::home_dir()`** — matches goose's own default; falls back to `/` if home dir is unavailable

### New control-socket methods

| Method | Params | Result | Behaviour |
|---|---|---|---|
| `profile.set_cwd` | `{ "name": "<profile>", "cwd": "<path>" }` | `ProfileEntry` | Set per-profile cwd and persist to `profile.toml`. |
| `profile.unset_cwd` | `{ "name": "<profile>" }` | `ProfileEntry` | Clear per-profile cwd from `profile.toml`. |
| `config.get_global` | `{}` | `{ "cwd": "<path>" \| null }` | Return the global config. |
| `config.set_global` | `{ "cwd": "<path>" \| null }` | `{ "cwd": "<path>" \| null }` | Update and persist the global config. Pass `null` to clear. |

### Updated `profile.get` response

`profile.get` now returns two additional fields:

| Field | Type | Description |
|---|---|---|
| `cwd` | `string \| null` | Raw per-profile setting (null if unset). |
| `resolved_cwd` | `string` | Effective cwd after walking the full resolution chain. |

### CLI surface

```
geese cwd <profile>              # Print resolved cwd for a profile
geese set-cwd <profile> <path>   # Set per-profile cwd
geese set-cwd --unset <profile>  # Clear per-profile cwd
geese config get [key]           # Print global config (or single key, e.g. "cwd")
geese config set <key> <value>   # Set global config field (e.g. "cwd /home/user/work")
geese config unset <key>         # Clear a global config field (e.g. "cwd")
```

### Backward compatibility

Existing `profile.toml` files without a `cwd` field deserialise cleanly — the missing field defaults to `None`. No migration is required.

## cwd validation and fallback (phlax/geese#28)

When `goosed.start` (or `connect_profile`) resolves the working directory for a new goose process, it checks that the resolved path exists on disk **before** calling `Command::current_dir`. If the directory does not exist, geesed logs a warning and lets the child inherit geesed's own working directory instead:

```
WARN geesed: configured cwd does not exist; falling back to geesed's working directory
     cwd="/does/not/exist" profile="work"
```

This means a stale or typo'd cwd value never surfaces as a misleading `ENOENT` ("No such file or directory") spawn error that looks like the goose binary is missing.

### Clearing a bad cwd value

Use `geese config unset cwd` to remove a global cwd that was set incorrectly:

```sh
geese config set cwd /does/not/exist   # oops
geese config unset cwd                 # clears it
geese config get cwd                   # prints: <not set>
```
