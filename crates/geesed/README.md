# geesed

`geesed` is the small control-plane daemon for `geese` v0: it starts in the foreground, owns a single unix control socket, answers a `status` ping, and exits cleanly on `SIGINT` or `SIGTERM`.

Run it with `cargo run -p geesed`.

Talk to it manually with `printf '{"jsonrpc":"2.0","id":1,"method":"status"}\n' | socat - UNIX-CONNECT:"${XDG_RUNTIME_DIR}/geese/control.sock"`.
