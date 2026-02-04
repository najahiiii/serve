# Serve — Yet another HTTP serve file server

This repository contains two Rust binaries for a simple file server:

- `serve/` — Axum-based HTTP file server
- `serve-cli/` — companion CLI for interacting with the server

## Features

- Directory listing with HTML template
- File download with proper `Content-Length`, `Accept-Ranges` and optional `view=true`
- Authenticated file uploads (`X-Serve-Token`)
- Authenticated delete endpoint for files/directories
- Optional upload path overrides via header, form field, query
- Configurable defaults via TOML/config/env/flags
- Gzip for text responses
- Logging for upload/download including IP + User-Agent

## Build

Install the Rust toolchain (Cargo + rustc).

Use Makefile:

```bash

make build    # builds both binaries into dist/
make install  # (sudo) installs to /usr/local/bin
make clean
```

## Configuration

The server reads config with precedence: CLI overrides → env vars → TOML file (auto-located) → defaults.

An example configuration is provided at `serve/config.example.toml`.

Upload token, root paths, extension whitelist, blacklist can be customized.

## Starting the server

```bash
./dist/serve run --config /path/to/config.toml
```

`serve` commands and options:

| Command       | Description                                                    |
| ------------- | -------------------------------------------------------------- |
| `run`         | Run the HTTP file server                                       |
| `init-config` | Generate a default config at `$HOME/.config/serve/config.toml` |
| `show-config` | Print the effective configuration and exit                     |
| `version`     | Print version/build information                                |

`serve run` / `serve show-config` options:

| Arg                       | Description                             | Default         |
| ------------------------- | --------------------------------------- | --------------- |
| `--config <FILE>`         | Path to configuration file (TOML)       | auto-located    |
| `--port <PORT>`           | Override listening port                 | from config/env |
| `--upload-token <TOKEN>`  | Override upload token                   | from config/env |
| `--max-file-size <BYTES>` | Override maximum upload size            | from config/env |
| `--root <PATH>`           | Override root directory to serve        | from config/env |
| `--show-token`            | (show-config only) display upload token | off             |

## systemd deployment

Systemd unit example in `deploy/systemd/serve.service`.

Steps:

1. Copy binaries to `/usr/local/bin/`
2. Create config directory `/etc/serve/` and runtime dir `/var/lib/serve`
3. Place config file at `/etc/serve/serve.toml`
4. Copy unit files to `/etc/systemd/system/`
5. `sudo systemctl daemon-reload`
6. `sudo systemctl enable --now serve.service`

Ensure user/group `serve` exists or adjust `User=`/`Group=` in unit files.

## Reverse proxy example

An OpenResty/Nginx v1.25+ server block example is available at `deploy/reverse-proxy/serve`. It demonstrates HTTP/2 + QUIC (HTTP/3) listeners, TLS, real-IP headers, and `proxy_set_header` values compatible with the backend. Adjust `server_name`, certificate paths, and upstream target before production use.

## CLI helper

`serve-cli/` provides a Rust-based helper tool:

```bash
cargo build --package serve-cli --release
./target/release/serve-cli list --host https://files.example.com root
./target/release/serve-cli info --host https://files.example.com \
    01ARZ3NDEKTSV4RRFFQ69G5FAV
./target/release/serve-cli download --host https://files.example.com \
    01ARZ3NDEKTSV4RRFFQ69G5FAV --out archive.tar
./target/release/serve-cli download --host https://files.example.com \
    root --recursive --out backups
./target/release/serve-cli upload --host https://files.example.com \
    ./archive.tar --token Inipassword_ --parent-id root
./target/release/serve-cli delete --host https://files.example.com \
    01ARZ3NDEKTSV4RRFFQ69G5FAV --token Inipassword_
```

Install via `make build` / `make install` to populate `dist/serve-cli` and `/usr/local/bin/serve-cli`.

Commands operate on catalog IDs (e.g. `root`, entries returned by `serve-cli list` or `serve-cli info`). IDs can be passed positionally (as in the examples above) or via `--id <ID>`. The server emits JSON directory listings when clients send the header `X-Serve-Client: serve-cli` (used by the helper); browsers still receive the HTML view by default.

`serve-cli` global options:

| Arg                 | Description                       | Default      |
| ------------------- | --------------------------------- | ------------ |
| `--config <FILE>`   | Path to custom configuration file | auto-located |
| `-r, --retries <N>` | Override maximum retry attempts   | 10           |

`serve-cli` commands:

| Command    | Description                      |
| ---------- | -------------------------------- |
| `config`   | Show configured defaults         |
| `download` | Download file(s) or directory    |
| `upload`   | Upload a file                    |
| `list`     | List directory contents          |
| `info`     | Show entry metadata              |
| `delete`   | Delete an entry                  |
| `setup`    | Interactive configuration helper |
| `version`  | Print version/build information  |

`serve-cli download` options:

| Arg                     | Description                                    | Default                               |
| ----------------------- | ---------------------------------------------- | ------------------------------------- |
| `--host <URL>`          | Base host URL                                  | from config                           |
| `--id <ID...>`          | One or more catalog IDs                        | required if no positional IDs         |
| `<ID...>`               | Positional catalog IDs                         | optional                              |
| `-O, --out <FILE>`      | Output file (single ID only)                   | inferred                              |
| `-R, --recursive`       | Download directories recursively               | off                                   |
| `-C, --connections [N]` | Parts per file (range requests)                | 1 (or 16 if flag used without value)  |
| `--skip`                | Skip if local file exists                      | off                                   |
| `--dup`                 | Preserve existing files by writing a duplicate | off                                   |
| `-P, --parallel [N]`    | Parallel tasks (1..8)                          | off (or 8 if flag used without value) |

`serve-cli upload` options:

| Arg                    | Description                     | Default     |
| ---------------------- | ------------------------------- | ----------- |
| `--host <URL>`         | Base host URL                   | from config |
| `<FILE>`               | File to upload                  | required    |
| `--token <TOKEN>`      | Upload token (X-Serve-Token)    | from config |
| `-p, --parent-id <ID>` | Target directory ID             | `root`      |
| `--allow-no-ext`       | Allow uploads without extension | off         |
| `--bypass`             | Bypass extension whitelist      | off         |
| `--stream`             | Use streaming upload            | off         |

`serve-cli list` options:

| Arg            | Description           | Default     |
| -------------- | --------------------- | ----------- |
| `--host <URL>` | Base host URL         | from config |
| `--id <ID>`    | Catalog ID            | `root`      |
| `<ID>`         | Positional catalog ID | `root`      |

`serve-cli info` options:

| Arg            | Description           | Default     |
| -------------- | --------------------- | ----------- |
| `--host <URL>` | Base host URL         | from config |
| `--id <ID>`    | Catalog ID            | required    |
| `<ID>`         | Positional catalog ID | required    |

`serve-cli delete` options:

| Arg               | Description                  | Default     |
| ----------------- | ---------------------------- | ----------- |
| `--host <URL>`    | Base host URL                | from config |
| `--id <ID>`       | Catalog ID                   | required    |
| `<ID>`            | Positional catalog ID        | required    |
| `--token <TOKEN>` | Delete token (X-Serve-Token) | from config |

Notes:

- `--out` only applies when downloading a single ID.
- For flags with optional values (`-C/--connections`, `-P/--parallel`), use `-C=16` / `-P=8` or place `--` before positional IDs if you want the default missing value (e.g., `serve-cli download -P -- <ID>`).

## Upload API

```bash
POST /upload?dir=<catalog_id>
Headers:
  X-Serve-Token: <token>
  X-Upload-Dir: optional catalog ID (fallback to query ?dir)
  X-Allow-No-Ext: true|1|yes to bypass extension check
  X-Allow-All-Ext: true|1|yes to bypass extension whitelist
Form:
  file=@path/to/upload
  dir=optional catalog ID (defaults to root)
```

Response JSON includes `powered_by`, `view`, `download` URL.

## Delete API

```bash
DELETE /delete?id=<catalog_id>
Headers:
  X-Serve-Token: <token>
```

Successful responses include the catalog ID, normalized path, entry type, and `"status": "deleted"`. The CLI helper wraps this via `serve-cli delete`.

## Logging

The server uses `tracing` with `RUST_LOG=info` by default. Upload and download handlers log the IP, file path, and user-agent for auditing.

## License

This project is licensed under the [MIT License](LICENSE).
