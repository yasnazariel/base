# vibenet

A public, ephemeral devnet for showing off in-flight Base features.

- Single L1 (anvil) + single L2 sequencer (same as `just up-single`)
- Public HTTP gateway served through Cloudflare Tunnel (no open ports)
- Per-IP rate limiting at the nginx layer and per-method rate limiting at
  proxyd
- Open RPC (no API key); abuse mitigated by the per-IP limits above
- One prefunded faucet address; standard anvil accounts are drained
- Test contracts (`USDV` - public-mint ERC-20, `NFV` - public-mint ERC-721) auto-deployed on boot
- A static landing page + a faucet UI + a Grafana admin panel
- `vibescan` block explorer (in-house; indexes address -> activity in
  sqlite, reads block/tx bodies directly from the node)

## Quick links

- Deployment guide: [`deploy/README.md`](./deploy/README.md)
- Host env template: [`vibenet-env.example`](./vibenet-env.example)
- UI content (editable per branch): [`config/vibenet.yaml`](./config/vibenet.yaml)
- Contract list (editable per branch): [`setup/contracts.yaml`](./setup/contracts.yaml)

## Running locally

Vibenet is designed for deployment on a host with a Cloudflare Tunnel, but you
can run the entire stack on your laptop for iteration:

```bash
# One-time: copy the example env and fill in values. FAUCET_PRIVATE_KEY /
# FAUCET_ADDR are required; TUNNEL_TOKEN can be any placeholder when testing
# locally (cloudflared will fail to connect but nothing else depends on it).
cp etc/vibenet/vibenet-env.example etc/vibenet/vibenet-env
${EDITOR} etc/vibenet/vibenet-env

just -f etc/docker/Justfile vibe
```

`nginx-gateway` publishes two loopback-only host ports so you can hit it
directly without `/etc/hosts` entries or `Host` header spoofing:

| URL                                            | Service                                  |
| ---------------------------------------------- | ---------------------------------------- |
| `http://localhost:18080/`                      | Landing page                             |
| `http://localhost:18080/faucet`                | Faucet UI                                |
| `http://localhost:18080/admin/`                | Grafana (basic auth via `ADMIN_HTPASSWD`)|
| `http://localhost:18080/config.json`           | Rendered UI config                       |
| `http://localhost:18080/contracts.json`        | Deployed contract addresses              |
| `http://localhost:18081/rpc`                   | JSON-RPC (proxyd -> base-client)         |
| `ws://localhost:18081/ws`                      | WebSocket RPC                            |
| `http://localhost:18082/`                      | vibescan block explorer                  |

Override the bindings with `VIBENET_HOST_PORT` / `VIBENET_RPC_HOST_PORT` /
`VIBENET_EXPLORER_HOST_PORT` in `vibenet-env` if those collide with something
else on your machine. The landing page's copy-pasteable RPC/explorer URLs
automatically rewrite to the local ports when served from `localhost`, so the
UI stays accurate in both modes.

Quick smoke test once `just vibe` is up:

```bash
curl -s http://localhost:18080/config.json | jq .title

curl -s -X POST -H 'Content-Type: application/json' \
  --data '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}' \
  http://localhost:18081/rpc
```

## Customizing what appears on the landing page

Edit [`config/vibenet.yaml`](./config/vibenet.yaml). The `vibenet-config-renderer`
container reads it on startup, converts to JSON, and writes it to a shared
volume that the UI fetches at `https://vibenet.base.org/config.json`. No
rebuild is needed; `docker compose restart vibenet-config-renderer nginx-gateway`
picks up changes.

Fields:

- `title`, `subtitle` - page header
- `instructions_markdown` - left column, rendered as markdown client-side
- `features` - right column, array of `{title, description, link?}`
- `branch`, `commit` - auto-overwritten by `just vibe` from `git rev-parse`

## Customizing deployed contracts

Edit [`setup/contracts.yaml`](./setup/contracts.yaml) and drop any new Solidity
sources into [`setup/contracts/src/`](./setup/contracts/). Each entry is:

```yaml
- name: myDemo                              # key in contracts.json
  artifact: src/MyDemo.sol:MyDemo           # forge target
  args: ["0x1234...", "{{ usdv }}"]         # optional; {{ }} resolves from
                                            # previously-deployed entries
```

Deployed addresses are published at
`https://vibenet.base.org/contracts.json` (also mounted into the UI as a
client-side feature list).

## RPC access

The RPC is currently open; no API key is required. Clients hit:

```bash
curl -s -X POST -H 'Content-Type: application/json' \
  --data '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}' \
  https://vibenet-rpc.base.org/rpc
```

WebSocket:

```javascript
new WebSocket("wss://vibenet-rpc.base.org/ws");
```

Abuse is mitigated by nginx per-IP rate limits (keyed on the real client IP
from Cloudflare's `CF-Connecting-IP` header) plus proxyd's per-method limits.
If/when we need to gate the RPC again, add a URL-path prefix in
`nginx/vibenet.conf.template` and re-run `just vibe`.

## Admin panel

Grafana is published at `https://vibenet.base.org/admin/` behind HTTP basic
auth. Credentials come from `ADMIN_HTPASSWD` in `vibenet-env` (a single
`user:bcrypt-hash` line, generated with
`htpasswd -nbB admin '<password>'`).

Change the password by updating `ADMIN_HTPASSWD` and running
`docker compose restart vibenet-htpasswd nginx-gateway`.

## Components

| Container              | Image                              | Role |
| ---------------------- | ---------------------------------- | ---- |
| `nginx-gateway`        | `nginx:1.27-alpine`                | Host-routed HTTP gateway, per-IP rate limits, admin basic-auth |
| `cloudflared`          | `cloudflare/cloudflared:2025.10.1` | Outbound tunnel to Cloudflare, TLS terminated at the edge |
| `vibenet-faucet`       | `base-vibenet-faucet:local` (rust) | `/drip` + `/status`, per-IP + per-address cooldowns, alloy signer |
| `vibenet-setup`        | `vibenet-setup:local` (foundry)    | One-shot: waits for L2, sweeps anvil balances, deploys demo contracts |
| `vibenet-config-renderer` | `mikefarah/yq`                  | Converts `vibenet.yaml` to `config.json` |
| `vibenet-htpasswd`     | `alpine`                           | Materializes `ADMIN_HTPASSWD` into the htpasswd volume |
| `proxyd`               | `proxyd:local`                     | Per-method JSON-RPC rate limits on top of nginx per-IP |
| `vibescan`             | `vibescan:local` (rust)            | In-house block explorer: indexes address activity to sqlite, renders server-side HTML |
| `base-client/builder/...` | same as `just up-single`        | Core devnet |

## File map

```
etc/vibenet/
  README.md                         (this file)
  vibenet-env.example               host env template
  docker-compose.vibenet.yml        overlay on etc/docker/docker-compose.yml
  config/vibenet.yaml               editable UI content
  nginx/vibenet.conf.template       nginx config (envsubst'd at container start)
  nginx/cf-ips.conf                 cloudflare ip allow-list for real_ip
  cloudflared/config.yml.template   host-routed tunnel config
  proxyd/proxyd-ratelimit.toml      per-method rate limits
  faucet/Dockerfile                 build image for base-vibenet-faucet
  explorer/Dockerfile               build image for vibescan
  setup/Dockerfile                  build image for foundry-based deployer
  setup/contracts.yaml              list of contracts to deploy
  setup/contracts/                  foundry project: src/*.sol
  setup/deploy-contracts.sh         entrypoint for vibenet-setup
  deploy/bootstrap.sh               one-shot host bootstrap (ubuntu/debian)
  deploy/README.md                  production deployment guide

apps/vibenet-ui/public/             static HTML/JS served by nginx

crates/vibenet/faucet/              base-vibenet-faucet (axum + alloy)
crates/vibenet/explorer/            vibescan (axum + alloy + sqlite)
```
