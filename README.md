# iroh-fm

`iroh-fm` is a personal music server built around `iroh`.

The core idea is simple: the music library lives behind an `iroh` endpoint, and client protocols are added as thin frontends on top.

## Why

Most music servers make the client protocol the center of the system. `iroh-fm` does the opposite.

- the backend is protocol-agnostic
- the transport is `iroh`
- Subsonic is one of multiple possible compatibility layers (e.g jellyfin, subsonic, plex)
- more frontends can be added without changing the server model

That means one music library, one backend, many possible client-facing APIs.

## What It Does

- scans a local music directory
- extracts tags and builds an indexed library
- caches metadata in SQLite for fast warm starts
- watches the library for real changes
- serves metadata, cover art, and audio over `iroh`
- exposes a Subsonic-compatible HTTP facade for existing players

## Workspace

```text
crates/
  client/
  protocol/
  server/
  subsonic/
```

- `client`: `iroh` RPC client for talking to the backend server
- `protocol`: shared backend request and response types
- `server`: the actual music server
- `subsonic`: Subsonic facade over the backend

## Usage

### Server

```sh
# Install iroh-fm binary
cargo install --git https://github.com/usagi-coffee/iroh-fm server

iroh-fm \
  --music-dir /path/to/music \        # required; root music directory
  --secret your-secret-key \          # optional; fixed iroh identity
  --relay https://relay.example.com \ # optional; advertised relay URL
  --peer peer-endpoint-id-1 \         # optional; allowlist peer, repeatable
  --peer peer-endpoint-id-2           # optional; omit all --peer flags for open access
```

### Subsonic frontend

```sh
# Install iroh-fm-subsonic binary
cargo install --git https://github.com/usagi-coffee/iroh-fm subsonic

iroh-fm-subsonic \
  --bind 127.0.0.1:4040 \                 # optional; default 127.0.0.1:4040
  --ticket your-endpoint-ticket \         # required unless --endpoint is used; backend ticket
  --endpoint your-backend-endpoint-id \   # required unless --ticket is used; backend endpoint id
  --relay https://relay.example.com \     # optional; overrides relay from ticket
  --secret your-secret-key \              # optional; fixed local iroh identity
  --username admin \                      # optional; default admin
  --password admin                        # optional; default admin
```
