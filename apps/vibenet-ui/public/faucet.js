// vibenet faucet page.
//
// Calls the faucet service behind nginx at /faucet/status and /faucet/drip.

async function loadStatus() {
  const statusEl = document.getElementById("faucet-status");
  try {
    const res = await fetch("/faucet/status", { cache: "no-store" });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const s = await res.json();
    const eth = (Number(s.balance_wei || 0) / 1e18).toFixed(4);
    const dripEth = (Number(s.drip_wei || 0) / 1e18).toFixed(4);
    statusEl.textContent =
      `Faucet ${s.address} holds ${eth} ETH. ` +
      `Drips ${dripEth} ETH per request.`;
  } catch (err) {
    statusEl.textContent = `Could not load faucet status: ${err.message}`;
  }
}

document.getElementById("drip-form").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const addr = document.getElementById("addr").value.trim();
  const btn = ev.target.querySelector("button");
  const resultEl = document.getElementById("drip-result");
  btn.disabled = true;
  resultEl.textContent = "Dripping...";
  try {
    const res = await fetch("/faucet/drip", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ address: addr }),
    });
    const body = await res.json().catch(() => ({}));
    if (!res.ok) throw new Error(body.error || `HTTP ${res.status}`);
    resultEl.textContent = JSON.stringify(body, null, 2);
  } catch (err) {
    resultEl.textContent = `Drip failed: ${err.message}`;
  } finally {
    btn.disabled = false;
    loadStatus();
  }
});

loadStatus();
