# Serve — Yet another HTTP serve file server

This repository contains two Rust binaries for a simple file server:

- `serve/` — Axum-based HTTP file server
- `serve-cli/` — companion CLI for interacting with the server

## Features

- Directory listing with HTML template
- File download with proper `Content-Length`, `Accept-Ranges` and optional `view=true`
- Authenticated file uploads (`X-Upload-Token`)
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
./target/release/serve-cli list --host https://files.example.com --path dir/
./target/release/serve-cli upload --host https://files.example.com \
    --file ./archive.tar --token Inipassword_ --upload-path backups/
./target/release/serve-cli download --host https://files.example.com \
    --path dir/archive.tar --out archive.tar
./target/release/serve-cli download --host https://files.example.com \
    --path dir/ --recursive --out backups
```

Install via `make build` / `make install` to populate `dist/serve-cli` and `/usr/local/bin/serve-cli`.

The server emits JSON directory listings when clients send the header `X-Serve-Client: serve-cli` (used by the helper); browsers still receive the HTML view by default.

## Upload API

```bash
POST /upload
Headers:
  X-Upload-Token: <token>
  X-Upload-Path: optional path
  X-Allow-No-Ext: true|1|yes to bypass extension check
Form:
  file=@path/to/upload
  path=optional directory
```

Response JSON includes `powered_by`, `view`, `download` URL.

## Logging

The server uses `tracing` with `RUST_LOG=info` by default. Upload and download handlers log the IP, file path, and user-agent for auditing.

## License

This project is licensed under the [MIT License](LICENSE).
