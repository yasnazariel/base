// vibenet landing page bootstrap.
//
// Loads /config.json (UI content from etc/vibenet/config/vibenet.yaml) and
// /contracts.json (written by vibenet-setup), and renders them.
//
// The API key for the RPC URL is read from localStorage under `vibenet:apiKey`.
// If it is not set, users are prompted; the key is never transmitted back to
// this origin, only used to build copy-pasteable RPC URLs.

const LS_API_KEY = "vibenet:apiKey";

function getApiKey() {
  let key = localStorage.getItem(LS_API_KEY);
  if (!key) {
    key = prompt("Enter your vibenet API key (shared by the operator):") || "";
    if (key) localStorage.setItem(LS_API_KEY, key);
  }
  return key;
}

function substituteKey(template, apiKey) {
  return (template || "").replaceAll("{apiKey}", apiKey || "<API_KEY>");
}

async function loadJson(url) {
  const res = await fetch(url, { cache: "no-store" });
  if (!res.ok) throw new Error(`${url} -> ${res.status}`);
  return res.json();
}

async function main() {
  const [config, contracts] = await Promise.all([
    loadJson("/config.json").catch(() => ({})),
    loadJson("/contracts.json").catch(() => null),
  ]);

  const apiKey = getApiKey();

  document.getElementById("title").textContent = config.title || "vibenet";
  document.getElementById("subtitle").textContent = config.subtitle || "";
  document.getElementById("branch").textContent = config.branch || "unknown";
  document.getElementById("commit").textContent = (config.commit || "unknown").slice(0, 12);

  const instructionsEl = document.getElementById("instructions");
  const rendered = window.marked
    ? window.marked.parse(config.instructions_markdown || "")
    : `<pre>${config.instructions_markdown || ""}</pre>`;
  instructionsEl.innerHTML = rendered.replaceAll("<API_KEY>", apiKey || "&lt;API_KEY&gt;");

  const rpcLink = document.getElementById("rpc-link");
  if (config.rpc_url_template) {
    rpcLink.href = substituteKey(config.rpc_url_template, apiKey);
    rpcLink.textContent = "RPC";
  }

  const featuresEl = document.getElementById("features");
  featuresEl.innerHTML = "";
  for (const f of config.features || []) {
    const li = document.createElement("li");
    const strong = document.createElement("strong");
    strong.textContent = f.title;
    li.appendChild(strong);
    li.appendChild(document.createTextNode(" — " + (f.description || "")));
    if (f.link) {
      li.appendChild(document.createTextNode(" "));
      const a = document.createElement("a");
      a.href = f.link;
      a.target = "_blank";
      a.rel = "noopener";
      a.textContent = "docs";
      li.appendChild(a);
    }
    featuresEl.appendChild(li);
  }

  const contractsEl = document.getElementById("contracts");
  if (contracts) {
    contractsEl.textContent = JSON.stringify(contracts, null, 2);
  } else {
    contractsEl.textContent = "Contracts not yet deployed. Refresh in a few seconds.";
  }

  const rpcHost = location.hostname.startsWith("vibenet.")
    ? location.hostname.replace(/^vibenet\./, "vibenet-rpc.")
    : "vibenet-rpc.base.org";
  const rpcUrl = `https://${rpcHost}/rpc/${apiKey || "<API_KEY>"}`;
  document.getElementById("curl-snippet").textContent =
    `curl -sX POST -H 'Content-Type: application/json' \\\n` +
    `  --data '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}' \\\n` +
    `  ${rpcUrl}`;
}

main().catch((err) => {
  document.body.insertAdjacentHTML(
    "afterbegin",
    `<pre style="color:#ff6b6b;padding:1rem;">Failed to load vibenet UI: ${err.message}</pre>`,
  );
});
