# geese

`geese` is a small Rust library and CLI for managing named `GOOSE_PATH_ROOT` profiles for goose.

## Install

```bash
cargo install --path crates/geese
```

## Example

```bash
geese new work-stable
GOOSE_PATH_ROOT="$(geese path work-stable)" goose
```

### Using the UI

If you have the goose desktop UI installed you can launch profiles into it with
`--bin` or the `GEESE_GOOSE_BIN` environment variable:

```bash
# One-off, via flag
geese launch --bin /usr/lib/goose/Goose work-stable

# Always use the UI for the current shell session
export GEESE_GOOSE_BIN=/usr/lib/goose/Goose
geese launch work-stable
```

The flag takes precedence over the environment variable; both fall back to the
bare `goose` on `PATH` when neither is set.
