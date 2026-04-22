// base vibenet landing page bootstrap.
//
// Loads /config.json (UI content from etc/vibenet/config/vibenet.yaml) and
// /contracts.json (written by vibenet-setup), renders them, and wires up
// the chain-card utilities (Add to wallet, Copy RPC URL, Copy terminal
// snippet). No build step, no bundler - one module, one stylesheet.
//
// The RPC is currently open (no API key path), so URL building is
// deterministic from the page's hostname.

import { createWalletClient, custom } from "https://esm.sh/viem@2.21.0";

// Hard-coded vibenet L2 chain id - matches L2_CHAIN_ID in vibenet-env and
// the markdown instructions. We surface it for display and for the
// wallet_addEthereumChain call below.
const VIBENET_CHAIN_ID = 84538453;

async function loadJson(url) {
  const res = await fetch(url, { cache: "no-store" });
  if (!res.ok) throw new Error(`${url} -> ${res.status}`);
  return res.json();
}

function isLocalHost(host) {
  return host === "localhost" || host === "127.0.0.1" || host === "0.0.0.0";
}

// RPC / WS / explorer URL builders. They branch on hostname because local
// dev publishes the gateway on three sibling ports (ui:18080, rpc:18081,
// explorer:18082) whereas prod uses three Cloudflare hostnames.
function buildRpcUrl() {
  const host = location.hostname;
  if (isLocalHost(host)) {
    const uiPort = parseInt(location.port || "80", 10);
    const rpcPort = uiPort + 1;
    return `${location.protocol}//${host}:${rpcPort}/rpc`;
  }
  const rpcHost = host.startsWith("vibenet.")
    ? host.replace(/^vibenet\./, "vibenet-rpc.")
    : "vibenet-rpc.base.org";
  return `https://${rpcHost}/rpc`;
}

function buildWsUrl() {
  const host = location.hostname;
  if (isLocalHost(host)) {
    const uiPort = parseInt(location.port || "80", 10);
    const rpcPort = uiPort + 1;
    const scheme = location.protocol === "https:" ? "wss:" : "ws:";
    return `${scheme}//${host}:${rpcPort}/ws`;
  }
  const wsHost = host.startsWith("vibenet.")
    ? host.replace(/^vibenet\./, "vibenet-rpc.")
    : "vibenet-rpc.base.org";
  return `wss://${wsHost}/ws`;
}

function buildExplorerUrl() {
  const host = location.hostname;
  if (isLocalHost(host)) {
    const uiPort = parseInt(location.port || "80", 10);
    const explorerPort = uiPort + 2;
    return `${location.protocol}//${host}:${explorerPort}`;
  }
  if (host.startsWith("vibenet.")) {
    return `https://${host.replace(/^vibenet\./, "vibenet-explorer.")}`;
  }
  return "https://vibenet-explorer.base.org";
}

// Rewrite the prod hostnames in any rendered markdown so copy-pasteable
// URLs actually work when served from localhost. Prod keeps the markdown
// verbatim.
function localizeUrls(html) {
  const host = location.hostname;
  if (!isLocalHost(host)) return html;
  const uiPort = parseInt(location.port || "80", 10);
  const rpcPort = uiPort + 1;
  const explorerPort = uiPort + 2;
  const httpBase = `${location.protocol}//${host}:${rpcPort}`;
  const wsScheme = location.protocol === "https:" ? "wss:" : "ws:";
  const wsBase = `${wsScheme}//${host}:${rpcPort}`;
  const explorerBase = `${location.protocol}//${host}:${explorerPort}`;
  return html
    .replaceAll("https://vibenet-rpc.base.org", httpBase)
    .replaceAll("wss://vibenet-rpc.base.org", wsBase)
    .replaceAll("https://vibenet-explorer.base.org", explorerBase);
}

async function main() {
  const [config, contracts] = await Promise.all([
    loadJson("/config.json").catch(() => ({})),
    loadJson("/contracts.json").catch(() => null),
  ]);

  document.getElementById("title").textContent = config.title || "base vibenet";
  document.getElementById("subtitle").textContent = config.subtitle || "";
  document.getElementById("branch").textContent = config.branch || "unknown";
  document.getElementById("commit").textContent = (config.commit || "unknown").slice(0, 12);

  const rpcUrl = buildRpcUrl();
  const wsUrl = buildWsUrl();
  const explorerUrl = buildExplorerUrl();

  document.getElementById("chain-id").textContent = String(VIBENET_CHAIN_ID);
  document.getElementById("rpc-display").textContent = rpcUrl;
  document.getElementById("ws-display").textContent = wsUrl;

  const explorerEl = document.getElementById("explorer-link");
  explorerEl.href = explorerUrl;
  explorerEl.textContent = explorerUrl;

  const rpcLink = document.getElementById("rpc-link");
  if (rpcLink) rpcLink.href = rpcUrl;

  const instructionsEl = document.getElementById("instructions");
  const rendered = window.marked
    ? window.marked.parse(config.instructions_markdown || "")
    : `<pre>${config.instructions_markdown || ""}</pre>`;
  instructionsEl.innerHTML = localizeUrls(rendered);

  renderFeatures(config.features || []);
  renderContracts(contracts, explorerUrl);

  document.getElementById("curl-snippet").textContent =
    `curl -sX POST -H 'Content-Type: application/json' \\\n` +
    `  --data '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}' \\\n` +
    `  ${rpcUrl}`;

  wireAddToWallet(rpcUrl, explorerUrl);
  wireCopyButton("copy-rpc", () => document.getElementById("rpc-display").textContent);
  wireCopyButton("copy-curl", () => document.getElementById("curl-snippet").textContent);
}

function renderFeatures(features) {
  const host = document.getElementById("features");
  host.innerHTML = "";
  for (const f of features) {
    const card = document.createElement("div");
    card.className = "feature-card";

    const title = document.createElement("div");
    title.className = "feature-title";
    title.textContent = f.title || "";
    card.appendChild(title);

    if (f.description) {
      const desc = document.createElement("div");
      desc.className = "feature-desc";
      desc.textContent = f.description;
      card.appendChild(desc);
    }

    if (f.link) {
      const a = document.createElement("a");
      a.href = f.link;
      a.target = "_blank";
      a.rel = "noopener";
      a.textContent = "Learn more →";
      card.appendChild(a);
    }
    host.appendChild(card);
  }
}

// Friendly labels for the keys vibenet-setup writes into contracts.json.
// Anything not in the map falls back to the raw key. Keys starting with
// `_` are metadata and skipped entirely.
const CONTRACT_LABELS = {
  faucetAddress: "Faucet",
  usdv: "USDV (ERC-20)",
  nfv: "NFV (ERC-721)",
};

// Tokens we offer a wallet_watchAsset button for. Values here must match
// the on-chain `name()` / `symbol()` / `decimals()` so wallets that
// validate the metadata don't reject the prompt.
const WATCHABLE_TOKENS = {
  usdv: { type: "ERC20", symbol: "USDV", decimals: 6 },
};

function renderContracts(contracts, explorerBase) {
  const host = document.getElementById("contracts-list");
  if (!contracts) {
    host.innerHTML = `<p class="muted" style="padding: 0.75rem 1rem; margin: 0;">Contracts not yet deployed. Refresh in a few seconds.</p>`;
    return;
  }
  host.innerHTML = "";
  let rendered = 0;
  for (const [k, v] of Object.entries(contracts)) {
    if (k.startsWith("_")) continue;
    if (typeof v !== "string") continue;
    if (!/^0x[0-9a-fA-F]{40}$/.test(v)) continue;

    const row = document.createElement("div");
    row.className = "contract-row";

    const label = document.createElement("span");
    label.className = "contract-label";
    label.textContent = CONTRACT_LABELS[k] || k;

    const link = document.createElement("a");
    link.className = "contract-addr";
    link.href = `${explorerBase}/address/${v}`;
    link.target = "_blank";
    link.rel = "noopener";
    link.textContent = v;

    row.append(label, link);

    // If this contract is a known ERC-20 we can offer a "Watch in wallet"
    // button - clicking it calls wallet_watchAsset so the user sees their
    // token balance in their wallet UI without pasting addresses by hand.
    const meta = WATCHABLE_TOKENS[k];
    if (meta) {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "watch-asset secondary small";
      btn.textContent = `Add ${meta.symbol} to wallet`;
      btn.addEventListener("click", () => watchAsset(v, meta, btn));
      row.append(btn);
    }

    host.appendChild(row);
    rendered++;
  }
  if (!rendered) {
    host.innerHTML = `<p class="muted" style="padding: 0.75rem 1rem; margin: 0;">No deployed contracts yet.</p>`;
  }
}

// Fire wallet_watchAsset via viem so the user's wallet tracks the token.
// On success the wallet adds it to its token list; on user rejection /
// unsupported wallet we quietly surface the error on the button itself.
async function watchAsset(address, meta, btn) {
  const original = btn.textContent;
  const provider = window.ethereum;
  if (!provider) {
    btn.textContent = "No wallet detected";
    setTimeout(() => (btn.textContent = original), 1500);
    return;
  }
  try {
    const wallet = createWalletClient({ transport: custom(provider) });
    await wallet.watchAsset({
      type: meta.type,
      options: { address, symbol: meta.symbol, decimals: meta.decimals },
    });
    btn.textContent = `${meta.symbol} added`;
  } catch (err) {
    btn.textContent = err?.shortMessage || "Rejected";
  } finally {
    setTimeout(() => (btn.textContent = original), 1800);
  }
}

// Build a viem Chain object describing vibenet. Used by walletClient.addChain
// to trigger the wallet's native "Add network" prompt.
function vibenetChain(rpcUrl, explorerUrl) {
  return {
    id: VIBENET_CHAIN_ID,
    name: "base vibenet",
    nativeCurrency: { name: "Ether", symbol: "ETH", decimals: 18 },
    rpcUrls: { default: { http: [rpcUrl] } },
    blockExplorers: { default: { name: "vibescan", url: explorerUrl } },
  };
}

function wireAddToWallet(rpcUrl, explorerUrl) {
  const btn = document.getElementById("add-to-wallet");
  const status = document.getElementById("wallet-status");
  if (!btn) return;
  btn.addEventListener("click", async () => {
    const provider = window.ethereum;
    if (!provider) {
      status.textContent = "No browser wallet detected on this page.";
      return;
    }
    try {
      const wallet = createWalletClient({ transport: custom(provider) });
      await wallet.addChain({ chain: vibenetChain(rpcUrl, explorerUrl) });
      status.textContent = "Network added. Your wallet should now be on base vibenet.";
    } catch (err) {
      // User rejection is code 4001; surface everything else verbatim.
      status.textContent = `Wallet did not add the network: ${err?.shortMessage || err?.message || err}`;
    }
  });
}

function wireCopyButton(btnId, getText) {
  const btn = document.getElementById(btnId);
  if (!btn) return;
  btn.addEventListener("click", async () => {
    const original = btn.textContent;
    try {
      await navigator.clipboard.writeText(getText());
      btn.textContent = "Copied";
    } catch {
      btn.textContent = "Copy failed";
    }
    setTimeout(() => (btn.textContent = original), 1200);
  });
}

main().catch((err) => {
  document.body.insertAdjacentHTML(
    "afterbegin",
    `<pre style="color:#ff6b6b;padding:1rem;">Failed to load vibenet UI: ${err.message}</pre>`,
  );
});
