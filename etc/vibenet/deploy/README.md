# Deploying vibenet to a bare-metal host

Vibenet is meant to run on one bare-metal (or VM) host behind Cloudflare. All
inbound traffic arrives over a Cloudflare Tunnel, so the host only needs SSH
(22) open to the public internet.

## Prerequisites

1. A linux host (Ubuntu 22.04+ or Debian 12+ recommended) with at least
   8 vCPU / 16 GB RAM / 200 GB disk.
2. Root SSH access.
3. A Cloudflare Zero Trust account and a Tunnel configured with two public
   hostnames pointing at `http://nginx-gateway:8080`:
   - `vibenet.base.org`
   - `vibenet-rpc.base.org`
4. DNS for both hostnames routed to the tunnel (Cloudflare will do this
   automatically when you add public hostnames in the Tunnel dashboard).

## 1. Bootstrap the host

As root on the target host:

```bash
curl -fsSL https://raw.githubusercontent.com/base/base/main/etc/vibenet/deploy/bootstrap.sh \
  | sudo VIBENET_REPO_BRANCH=<branch> bash
```

The script is idempotent and does the following:

- Installs Docker, Docker Compose, just, and Foundry
- Creates a `vibenet` unix user with docker group membership
- Clones the repo to `/opt/vibenet/base`
- Sets ufw to allow SSH only (Cloudflare Tunnel is outbound-only)
- Copies `vibenet-env.example` to `vibenet-env` (empty secrets)

## 2. Fill in secrets

```bash
su - vibenet
cd /opt/vibenet/base
${EDITOR} etc/vibenet/vibenet-env
```

Required values (see `etc/vibenet/vibenet-env.example` for details):

- `TUNNEL_TOKEN` - from the Cloudflare Tunnel dashboard.
- `FAUCET_ADDR` + `FAUCET_PRIVATE_KEY` - generate with `cast wallet new`. This
  address is the only account prefunded in vibenet genesis.
- `ADMIN_HTPASSWD` - bcrypt line from `htpasswd -nbB admin '<password>'`.

## 3. Launch

```bash
just -f etc/docker/Justfile vibe
```

This wipes any existing devnet data, rebuilds rust images, and brings up:

- L1 anvil + L2 sequencer + consensus + batcher
- `nginx-gateway`, `cloudflared`, `vibenet-faucet`, `vibenet-setup`,
  `vibenet-config-renderer`, `vibenet-htpasswd`, `proxyd`
- Jaeger / Prometheus / Grafana (the admin panel)

Give it ~2 minutes. Check progress with `just -f etc/docker/Justfile vibe-logs`.

## 4. Verify

- `https://vibenet.base.org/` - landing page with instructions + feature list
- `https://vibenet.base.org/faucet/status` - faucet JSON status
- `https://vibenet-rpc.base.org/rpc` - JSON-RPC endpoint (open)
- `https://vibenet.base.org/admin/` - Grafana (basic auth)

## Updating to a different branch

```bash
cd /opt/vibenet/base
git fetch && git checkout <branch> && git pull
just -f etc/docker/Justfile vibe
```

`just vibe` always wipes chain state, so the new branch starts from fresh
genesis with the faucet prefunded.

## Teardown

```bash
just -f etc/docker/Justfile vibe-down
```

This stops containers and wipes `.devnet/` state. It does not touch
`vibenet-env` or `contracts.json` history.
