// base vibenet faucet page.
//
// Calls the faucet service behind nginx at /faucet/status, /faucet/drip,
// and /faucet/drip-usdv. The drip form has two submit buttons ("Drip ETH"
// / "Mint USDV"); the clicked button's `value` picks which asset to drip.
// USDV drips go to a separate endpoint that reads the token address from
// the shared contracts.json and calls mint() on it.

// Mirror the explorer-URL logic from app.js so the USDV address in the
// status line can link directly to its explorer page. Kept inline (rather
// than imported) because these pages ship as plain <script> tags with no
// bundler.
function isLocalHost(host) {
  return host === "localhost" || host === "127.0.0.1" || host === "0.0.0.0";
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

function formatEth(wei) {
  return (Number(wei || 0) / 1e18).toFixed(4);
}

function formatUsdv(units) {
  // USDV has 6 decimals. Default faucet drip is a whole number of dollars
  // so two-decimal display is plenty.
  const n = Number(units || 0) / 1e6;
  return n.toLocaleString(undefined, { maximumFractionDigits: 2 });
}

async function loadStatus() {
  const el = document.getElementById("faucet-status");
  try {
    const res = await fetch("/faucet/status", { cache: "no-store" });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const s = await res.json();
    const explorer = buildExplorerUrl();
    const eth = formatEth(s.balance_wei);
    const dripEth = formatEth(s.drip_wei);
    el.innerHTML = "";

    // Line 1: faucet EOA balance + ETH drip amount.
    const line1 = document.createElement("div");
    line1.append("Faucet ");
    const faucetLink = document.createElement("a");
    faucetLink.href = `${explorer}/address/${s.address}`;
    faucetLink.target = "_blank";
    faucetLink.rel = "noopener";
    faucetLink.textContent = s.address;
    line1.appendChild(faucetLink);
    line1.append(` holds ${eth} ETH. Drips ${dripEth} ETH per request.`);
    el.appendChild(line1);

    // Line 2: USDV status.
    const line2 = document.createElement("div");
    if (s.usdv_address) {
      line2.append(`Mints ${formatUsdv(s.usdv_drip_units)} USDV per request at `);
      const usdvLink = document.createElement("a");
      usdvLink.href = `${explorer}/address/${s.usdv_address}`;
      usdvLink.target = "_blank";
      usdvLink.rel = "noopener";
      usdvLink.textContent = s.usdv_address;
      line2.appendChild(usdvLink);
      line2.append(".");
    } else {
      line2.textContent = "USDV not yet deployed.";
    }
    el.appendChild(line2);
  } catch (err) {
    el.textContent = `Could not load faucet status: ${err.message}`;
  }
}

const form = document.getElementById("drip-form");
form.addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const addr = document.getElementById("addr").value.trim();
  // submitter is set when a named submit button is clicked; default to ETH.
  const token = (ev.submitter && ev.submitter.value) || "eth";
  const buttons = form.querySelectorAll("button");
  const resultEl = document.getElementById("drip-result");
  buttons.forEach((b) => (b.disabled = true));
  resultEl.textContent = token === "usdv" ? "Minting USDV..." : "Dripping ETH...";
  try {
    const endpoint = token === "usdv" ? "/faucet/drip-usdv" : "/faucet/drip";
    const res = await fetch(endpoint, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ address: addr }),
    });
    const body = await res.json().catch(() => ({}));
    if (!res.ok) {
      // nginx-level 429s return an HTML body (not JSON), so `body.error`
      // is empty - fall back to a friendly message for that case.
      const reason =
        body.error ||
        (res.status === 429
          ? "rate limited - wait a minute and try again"
          : `HTTP ${res.status}`);
      throw new Error(reason);
    }
    resultEl.textContent = JSON.stringify(body, null, 2);
  } catch (err) {
    resultEl.textContent = `${token === "usdv" ? "USDV mint" : "Drip"} failed: ${err.message}`;
  } finally {
    buttons.forEach((b) => (b.disabled = false));
    loadStatus();
  }
});

loadStatus();
