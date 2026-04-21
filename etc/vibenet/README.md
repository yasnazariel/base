# vibenet

A public, ephemeral devnet for showing off in-flight Base features.

- Single L1 (anvil) + single L2 sequencer (same as `just up-single`)
- Public HTTP gateway served through Cloudflare Tunnel (no open ports)
- Per-IP rate limiting at the nginx layer and per-method rate limiting at
  proxyd
- URL-path API key (`/rpc/<key>`) gating the RPC
- One prefunded faucet address; standard anvil accounts are drained
- Mock testnet contracts (`MockUSDC`, `MockNFT`) auto-deployed on boot
- A static landing page + a faucet UI + a Grafana admin panel

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

The gateway listens on `nginx-gateway:8080` inside the docker network. To hit
it from the host without Cloudflare, forward the port:

```bash
docker compose --env-file etc/docker/devnet-env --env-file etc/vibenet/vibenet-env \
  -f etc/docker/docker-compose.yml -f etc/vibenet/docker-compose.vibenet.yml \
  exec nginx-gateway nginx -T
# Or publish the port by adding a "ports:" override; intentionally not the
# default because on a real deploy only cloudflared should reach nginx.
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
  args: ["0x1234...", "{{ mockUsdc }}"]     # optional; {{ }} resolves from
                                            # previously-deployed entries
```

Deployed addresses are published at
`https://vibenet.base.org/contracts.json` (also mounted into the UI as a
client-side feature list).

## API key usage

Every RPC request must include the shared `VIBENET_API_KEY` in the path:

```bash
API=https://vibenet-rpc.base.org/rpc/$VIBENET_API_KEY
curl -s -X POST -H 'Content-Type: application/json' \
  --data '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}' "$API"
```

WebSocket:

```javascript
new WebSocket(`wss://vibenet-rpc.base.org/ws/${API_KEY}`);
```

The key is opaque to proxyd and base-client; nginx simply refuses to forward
any path that doesn't match, so an incorrect key returns 404.

Rotate the key by regenerating `VIBENET_API_KEY` in `vibenet-env` and
re-running `just vibe`. All existing clients are invalidated immediately.

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
| `nginx-gateway`        | `nginx:1.27-alpine`                | Host-routed HTTP gateway, per-IP rate limits, URL-path API key check |
| `cloudflared`          | `cloudflare/cloudflared:2025.10.1` | Outbound tunnel to Cloudflare, TLS terminated at the edge |
| `vibenet-faucet`       | `base-vibenet-faucet:local` (rust) | `/drip` + `/status`, per-IP + per-address cooldowns, alloy signer |
| `vibenet-setup`        | `vibenet-setup:local` (foundry)    | One-shot: waits for L2, sweeps anvil balances, deploys demo contracts |
| `vibenet-config-renderer` | `mikefarah/yq`                  | Converts `vibenet.yaml` to `config.json` |
| `vibenet-htpasswd`     | `alpine`                           | Materializes `ADMIN_HTPASSWD` into the htpasswd volume |
| `proxyd`               | `proxyd:local`                     | Per-method JSON-RPC rate limits on top of nginx per-IP |
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
  setup/Dockerfile                  build image for foundry-based deployer
  setup/contracts.yaml              list of contracts to deploy
  setup/contracts/                  foundry project: src/*.sol
  setup/deploy-contracts.sh         entrypoint for vibenet-setup
  deploy/bootstrap.sh               one-shot host bootstrap (ubuntu/debian)
  deploy/README.md                  production deployment guide

apps/vibenet-ui/public/             static HTML/JS served by nginx

crates/vibenet/faucet/              base-vibenet-faucet (axum + alloy)
```
