#!/bin/bash
set -euo pipefail

# Deploys the EIP-8130 system contracts (AccountConfiguration, verifiers,
# DefaultAccount) to the running devnet L2 via the existing Deploy.s.sol
# script from the contracts/eip-8130 submodule.
#
# Prerequisites:
#   - Devnet L2 running (docker-compose up)
#   - Foundry (forge) installed locally
#   - contracts/eip-8130 submodule initialized (git submodule update --init)
#
# Usage:
#   ./deploy-8130.sh [--rpc <url>]
#
# Outputs:
#   - Deployed addresses printed to stdout
#   - JSON written to .devnet/l2/8130-addresses.json

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
CONTRACTS_DIR="$REPO_ROOT/contracts/eip-8130"
OUTPUT_DIR="$REPO_ROOT/.devnet/l2"

L2_RPC="${L2_RPC:-http://localhost:7545}"

for arg in "$@"; do
  case "$arg" in
    --rpc) shift; L2_RPC="$1"; shift ;;
    *) ;;
  esac
done

# Anvil Account 1 (deployer) — prefunded with 10k ETH on devnet L2.
# Account 0 is used by op-deployer for L1 contracts and has no L2 balance.
DEPLOYER_KEY="${DEPLOYER_KEY:-0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d}"

echo "=== EIP-8130 Contract Deployment ==="
echo "RPC:        $L2_RPC"
echo "Contracts:  $CONTRACTS_DIR"
echo ""

if [ ! -f "$CONTRACTS_DIR/foundry.toml" ]; then
  echo "ERROR: contracts/eip-8130 submodule not initialized."
  echo "Run: git submodule update --init --recursive"
  exit 1
fi

if ! command -v forge &>/dev/null; then
  echo "ERROR: forge not found. Install Foundry: https://getfoundry.sh"
  exit 1
fi

# Wait for L2 RPC
echo "Waiting for L2 RPC..."
for i in $(seq 1 30); do
  if curl -s --max-time 2 -X POST -H "Content-Type: application/json" \
    --data '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
    "$L2_RPC" | grep -q result; then
    echo "L2 RPC ready."
    break
  fi
  if [ "$i" -eq 30 ]; then
    echo "ERROR: L2 RPC not reachable at $L2_RPC"
    exit 1
  fi
  sleep 1
done

echo ""
echo "--- Running forge script Deploy.s.sol ---"
cd "$CONTRACTS_DIR"

# Ensure dependencies are present
if [ ! -d "lib/openzeppelin-contracts" ] || [ ! -d "lib/solady" ]; then
  echo "Installing forge dependencies..."
  forge install --no-commit 2>/dev/null || true
fi

DEPLOY_OUTPUT=$(forge script script/Deploy.s.sol:Deploy \
  --rpc-url "$L2_RPC" \
  --private-key "$DEPLOYER_KEY" \
  --broadcast \
  --via-ir \
  2>&1) || {
  echo "ERROR: forge script failed"
  echo "$DEPLOY_OUTPUT"
  exit 1
}

echo "$DEPLOY_OUTPUT"

echo ""
echo "--- Extracting Addresses ---"

# Parse the forge broadcast JSON for reliable address extraction.
# Forge writes structured deploy artifacts to broadcast/ after --broadcast.
CHAIN_ID=$(python3 -c "
import json, urllib.request
resp = urllib.request.urlopen(urllib.request.Request(
    '$L2_RPC', method='POST',
    headers={'Content-Type': 'application/json'},
    data=b'{\"jsonrpc\":\"2.0\",\"method\":\"eth_chainId\",\"params\":[],\"id\":1}'))
print(int(json.load(resp)['result'], 16))
")
BROADCAST_JSON="$CONTRACTS_DIR/broadcast/Deploy.s.sol/$CHAIN_ID/run-latest.json"

if [ -f "$BROADCAST_JSON" ]; then
  echo "Reading from $BROADCAST_JSON"
  extract_addr() {
    python3 -c "
import json, sys
data = json.load(open('$BROADCAST_JSON'))
for tx in data.get('transactions', []):
    if tx.get('contractName') == sys.argv[1]:
        print(tx['contractAddress'])
        sys.exit(0)
print('')
" "$1"
  }
else
  echo "WARNING: Broadcast JSON not found, falling back to stdout parsing"
  extract_addr() {
    echo "$DEPLOY_OUTPUT" | sed 's/\x1b\[[0-9;]*m//g' | grep -i "$1:" | tail -1 | awk '{print $NF}'
  }
fi

# Deterministic CREATE2 deployments may skip emitting per-contract transactions
# when bytecode already exists (e.g., contracts deployed during BASE_V1 upgrade).
# Use the pure address preview as a fallback source of truth.
PREVIEW_OUTPUT=$(forge script script/Deploy.s.sol:Deploy --sig "addresses()" 2>&1 || true)
extract_preview_addr() {
  echo "$PREVIEW_OUTPUT" | sed 's/\x1b\[[0-9;]*m//g' | grep -i "$1:" | tail -1 | awk '{print $NF}'
}
resolve_addr() {
  local candidate="$1"
  local label="$2"
  if [ -n "$candidate" ] && [ "$candidate" != "null" ]; then
    echo "$candidate"
    return
  fi
  extract_preview_addr "$label"
}

ACCOUNT_CONFIG=$(resolve_addr "$(extract_addr "AccountConfiguration")" "AccountConfiguration")
DEFAULT_ACCOUNT=$(resolve_addr "$(extract_addr "DefaultAccount")" "DefaultAccount")
DEFAULT_HIGH_RATE=$(resolve_addr "$(extract_addr "DefaultHighRateAccount")" "DefaultHighRateAccount")

# Verifier namespace update:
#   - address(0): implicit EOA
#   - address(1): explicit native K1/ecrecover
#   - address(max): revoked sentinel
K1_VERIFIER="0x0000000000000000000000000000000000000001"
K1_VERIFIER_CONTRACT=$(resolve_addr "$(extract_addr "K1Verifier")" "K1Verifier")
P256_VERIFIER=$(resolve_addr "$(extract_addr "P256Verifier")" "P256Verifier")
WEBAUTHN_VERIFIER=$(resolve_addr "$(extract_addr "WebAuthnVerifier")" "WebAuthnVerifier")
DELEGATE_VERIFIER=$(resolve_addr "$(extract_addr "DelegateVerifier")" "DelegateVerifier")
ALWAYS_VALID_VERIFIER=$(resolve_addr "$(extract_addr "AlwaysValidVerifier")" "AlwaysValidVerifier")

echo ""
echo "=== Deployed Addresses ==="
echo "AccountConfiguration:   $ACCOUNT_CONFIG"
echo "DefaultAccount:         $DEFAULT_ACCOUNT"
echo "DefaultHighRateAccount: $DEFAULT_HIGH_RATE"
echo "K1Verifier:             $K1_VERIFIER"
echo "K1VerifierContract:     $K1_VERIFIER_CONTRACT"
echo "P256Verifier:           $P256_VERIFIER"
echo "WebAuthnVerifier:       $WEBAUTHN_VERIFIER"
echo "DelegateVerifier:       $DELEGATE_VERIFIER"
echo "AlwaysValidVerifier:    $ALWAYS_VALID_VERIFIER"

mkdir -p "$OUTPUT_DIR"
cat >"$OUTPUT_DIR/8130-addresses.json" <<ADDR_EOF
{
  "accountConfiguration": "$ACCOUNT_CONFIG",
  "defaultAccount": "$DEFAULT_ACCOUNT",
  "defaultHighRateAccount": "$DEFAULT_HIGH_RATE",
  "k1Verifier": "$K1_VERIFIER",
  "p256Verifier": "$P256_VERIFIER",
  "webAuthnVerifier": "$WEBAUTHN_VERIFIER",
  "delegateVerifier": "$DELEGATE_VERIFIER",
  "alwaysValidVerifier": "$ALWAYS_VALID_VERIFIER"
}
ADDR_EOF

echo ""
echo "Addresses written to $OUTPUT_DIR/8130-addresses.json"
echo ""
echo "=== Next Steps ==="
echo "Update the Rust predeploy constants in:"
echo "  crates/alloy/consensus/src/transaction/eip8130/predeploys.rs"
echo ""
echo "Specifically:"
echo "  ACCOUNT_CONFIG_ADDRESS   = $ACCOUNT_CONFIG"
echo "  DEFAULT_ACCOUNT_ADDRESS  = $DEFAULT_ACCOUNT"
echo "  K1_VERIFIER_ADDRESS      = $K1_VERIFIER"
echo "  P256_RAW_VERIFIER_ADDRESS = $P256_VERIFIER"
echo "  P256_WEBAUTHN_VERIFIER_ADDRESS = $WEBAUTHN_VERIFIER"
echo "  DELEGATE_VERIFIER_ADDRESS = $DELEGATE_VERIFIER"
