# geese design

## Storage layout

`geese` stores profiles under `$GEESE_ROOT/profiles`, where `GEESE_ROOT` defaults to `$XDG_DATA_HOME/geese` (typically `~/.local/share/geese`).

Each profile directory is a complete `GOOSE_PATH_ROOT`:

```text
$GEESE_ROOT/
└── profiles/
    └── <name>/
        ├── profile.toml
        ├── config/
        ├── data/
        └── state/
```

`geese` creates the empty `config/`, `data/`, and `state/` directories and only manages `profile.toml`. goose owns the contents underneath those directories.

## profile.toml

Version 0 metadata is:

```toml
name = "work-stable"
locked = false
parent = "default" # optional
```

`parent` is only recorded for profiles created with `geese copy`.

## goose integration

`geese path <name>` prints the profile directory, and `geese launch <name> -- ...` executes `goose` with `GOOSE_PATH_ROOT` set to that directory so goose uses that profile's `config/`, `data/`, and `state/` trees.

## Launch binary

`geese launch` resolves the binary to execute in this order, stopping at the first match:

1. `--bin <path>` flag passed directly to `geese launch`
2. `GEESE_GOOSE_BIN` environment variable (a path or a bare name resolved against `PATH`)
3. The literal `goose` (current default behaviour)

No validation is performed on the resolved path — if the binary does not exist or is not executable, the OS error from `exec` surfaces the failure clearly.

A profile is intentionally binary-agnostic: it is just a `GOOSE_PATH_ROOT` directory and can be consumed by any goose front-end (CLI today, desktop UI tomorrow) without any change to `profile.toml`.

