# base-vibenet-faucet

HTTP faucet service used by public vibenet devnets.

Exposes:

- `POST /drip { "address": "0x..." }` — sends `VIBENET_FAUCET_DRIP_WEI` from
  the hot key to the supplied address. Rate-limited per client IP and per
  destination address.
- `POST /drip-usdv { "address": "0x..." }` — mints `VIBENET_FAUCET_USDV_DRIP_UNITS`
  USDV (in 6-decimal base units) to the supplied address. Same per-IP and
  per-address cooldown shape as `/drip`, but tracked independently — dripping
  ETH does not put the caller into cooldown for USDV, and vice versa.
- `GET /status` — returns the faucet address, current balance, drip size, and
  configured cooldowns. Safe to call unauthenticated; does not leak secrets.

## Configuration (env)

| Variable | Required | Default | Notes |
| --- | --- | --- | --- |
| `VIBENET_FAUCET_BIND` | no | `0.0.0.0:8080` | Listen address. |
| `VIBENET_FAUCET_RPC_URL` | yes | | Upstream L2 JSON-RPC URL. |
| `VIBENET_FAUCET_CHAIN_ID` | yes | | L2 chain id. |
| `VIBENET_FAUCET_PRIVATE_KEY` | yes | | 0x-prefixed hex private key. |
| `VIBENET_FAUCET_ADDR` | yes | | Public address; verified against the key. |
| `VIBENET_FAUCET_DRIP_WEI` | no | `100000000000000000` | Amount to drip (0.1 ETH). |
| `VIBENET_FAUCET_USDV_DRIP_UNITS` | no | `1000000000` | USDV minted per drip (1000 USDV, 6 decimals). |
| `VIBENET_FAUCET_IP_COOLDOWN_SECS` | no | `3600` | Per-IP cooldown, applied independently per asset. |
| `VIBENET_FAUCET_ADDR_COOLDOWN_SECS` | no | `3600` | Per-destination cooldown, applied independently per asset. |

## Security

- Real client IP is taken from `CF-Connecting-IP` (populated by Cloudflare +
  the nginx gateway). If absent, the connecting peer IP is used instead.
- The private key is only ever read from the environment. It is never logged,
  emitted in errors, or surfaced through `/status`.
- Cooldown state lives in memory only; restarting the service resets all
  cooldowns. This is acceptable for vibenet since restarts wipe chain state
  too.
