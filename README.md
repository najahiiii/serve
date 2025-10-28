# Serve — Yet another HTTP serve file server

This repository contains two implementations of a simple file server:

- `serve-go/` — Go HTTP server
- `serve-rs/` — Rust Axum-based server

## Features

- Directory listing with HTML template
- File download with proper `Content-Length`, `Accept-Ranges` and optional `view=true`
- Authenticated file uploads (`X-Upload-Token`)
- Optional upload path overrides via header, form field, query
- Configurable defaults via TOML/config/env/flags
- Gzip for text responses
- Logging for upload/download including IP + User-Agent

## Build

Install Go (>=1.25) and Rust toolchain.

Use Makefile:

```bash

make build    # builds both binaries into dist/
make install  # (sudo) installs to /usr/local/bin
make clean
```

## Configuration

Each implementation reads config with precedence: CLI overrides → env vars → TOML file (auto-located) → defaults.

Example configs provided:

- `serve-go/config.example.toml`
- `serve-rs/config.example.toml`

Upload token, root paths, extension whitelist, blacklist can be customized.

## Starting the servers

### Go

```bash
./dist/serve-go run --config /path/to/config.toml
```

### Rust

```bash
./dist/serve-rs run --config /path/to/config.toml
```

CLI supports `--root`, `--port`, `--upload-token`, `--max-file-size`, etc.  `init-config` subcommand writes `$HOME/.config/serve/config.toml` template.

## systemd deployment

Systemd unit examples in `deploy/systemd/`:

- `serve-go.service`
- `serve-rs.service`

Steps:

1. Copy binaries to `/usr/local/bin/`
2. Create config directory `/etc/serve/` and runtime dir `/var/lib/serve`
3. Place config files at `/etc/serve/serve-go.toml` and `/etc/serve/serve-rs.toml`
4. Copy unit files to `/etc/systemd/system/`
5. `sudo systemctl daemon-reload`
6. `sudo systemctl enable --now serve-go.service serve-rs.service`

Ensure user/group `serve` exists or adjust `User=`/`Group=` in unit files.

## Reverse proxy example

An OpenResty/Nginx v1.25+ server block example is available at `deploy/reverse-proxy/serve`. It demonstrates HTTP/2 + QUIC (HTTP/3) listeners, TLS, real-IP headers, and `proxy_set_header` values compatible with both backends. Adjust `server_name`, certificate paths, and upstream target before production use.

## CLI helper

`serve-cli/` provides a Rust-based helper tool:

```bash
cargo build --manifest-path serve-cli/Cargo.toml --release
./serve-cli/target/release/serve-cli list --host https://files.example.com --path dir/
./serve-cli/target/release/serve-cli upload --host https://files.example.com \
    --file ./archive.tar --token Inipassword_ --upload-path backups/
./serve-cli/target/release/serve-cli download --host https://files.example.com \
    --path dir/archive.tar --out archive.tar
./serve-cli/target/release/serve-cli download --host https://files.example.com \
    --path dir/ --recursive --out backups
```

Install via `make build` / `make install` to populate `dist/serve-cli` and `/usr/local/bin/serve-cli`.

Both servers emit JSON directory listings when clients send the header `X-Serve-Client: serve-cli` (used by the helper); browsers still receive the HTML view by default.

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

- Go uses `log.Printf`
- Rust uses `tracing` with `RUST_LOG=info`

Both log `[downloading]` and `[uploading]` lines with IP / file / path / user-agent.

## License

This project is licensed under the [MIT License](LICENSE).
