# geesed v0

`geesed` v0 is deliberately small: it boots, listens on a control socket, answers a `status` ping, and shuts down cleanly. Profile CRUD and ACP routing land in later issues; see phlax/geese#10 for the sequence.

`geese status` can be pointed at a non-default daemon socket by setting `GEESED_SOCKET`.

## Profile CRUD (phlax/geese#16)

Profile CRUD (`profile.list`, `profile.get`, `profile.create`, `profile.delete`, `profile.lock`, `profile.unlock`, `profile.copy`) is now dispatched through `geesed` via JSON-RPC 2.0. The `geese` CLI exposes each verb (`geese list`, `geese new <name>`, `geese path <name>`, `geese delete <name>`, `geese lock <name>`, `geese unlock <name>`, `geese copy <src> <dest>`), all of which call `geese-client::ensure_running()` to autospawn the daemon when it isn't running. The storage root defaults to `$GEESE_ROOT` (or the platform data dir); tests pass an explicit root via `RunOpts::geese_root`. See phlax/geese#16 for the full design.

