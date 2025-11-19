# Serve — Yet another HTTP serve file server

This repository contains two Rust binaries for a simple file server:

- `serve/` — Axum-based HTTP file server
- `serve-cli/` — companion CLI for interacting with the server

## Features

- Directory listing with HTML template
- File download with proper `Content-Length`, `Accept-Ranges` and optional `view=true`
- Authenticated file uploads (`X-Upload-Token`)
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

CLI supports `--root`, `--port`, `--upload-token`, `--max-file-size`, etc.  `init-config` subcommand writes `$HOME/.config/serve/config.toml` template.

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
./target/release/serve-cli list --host https://files.example.com --id root
./target/release/serve-cli info --host https://files.example.com \
    --id 01ARZ3NDEKTSV4RRFFQ69G5FAV
./target/release/serve-cli download --host https://files.example.com \
    --id 01ARZ3NDEKTSV4RRFFQ69G5FAV --out archive.tar
./target/release/serve-cli download --host https://files.example.com \
    --id root --recursive --out backups
./target/release/serve-cli upload --host https://files.example.com \
    --file ./archive.tar --token Inipassword_ --parent-id root
./target/release/serve-cli delete --host https://files.example.com \
    --id 01ARZ3NDEKTSV4RRFFQ69G5FAV --token Inipassword_
```

Install via `make build` / `make install` to populate `dist/serve-cli` and `/usr/local/bin/serve-cli`.

Commands operate on catalog IDs (e.g. `root`, entries returned by `serve-cli list` or `serve-cli info`). The server emits JSON directory listings when clients send the header `X-Serve-Client: serve-cli` (used by the helper); browsers still receive the HTML view by default.

## Upload API

```bash
POST /upload?dir=<catalog_id>
Headers:
  X-Upload-Token: <token>
  X-Upload-Dir: optional catalog ID (fallback to query ?dir)
  X-Allow-No-Ext: true|1|yes to bypass extension check
Form:
  file=@path/to/upload
  dir=optional catalog ID (defaults to root)
```

Response JSON includes `powered_by`, `view`, `download` URL.

## Delete API

```bash
DELETE /delete?id=<catalog_id>
Headers:
  X-Upload-Token: <token>
```

Successful responses include the catalog ID, normalized path, entry type, and `"status": "deleted"`. The CLI helper wraps this via `serve-cli delete`.

## Logging

The server uses `tracing` with `RUST_LOG=info` by default. Upload and download handlers log the IP, file path, and user-agent for auditing.

## License

This project is licensed under the [MIT License](LICENSE).
