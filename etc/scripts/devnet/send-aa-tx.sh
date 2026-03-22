#!/bin/bash
# Sends a minimal EIP-8130 AA transaction to the local devnet.
#
# Usage: ./etc/scripts/devnet/send-aa-tx.sh
#
# Prerequisites:
#   - Devnet running with BASE_V1 active (L2_BASE_V1_BLOCK=0)
#   - cast (foundry) installed
set -euo pipefail

L2_RPC="${L2_BUILDER_RPC_URL:-http://localhost:7545}"
SENDER_KEY="${ANVIL_ACCOUNT_0_KEY:-0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80}"
SENDER_ADDR="${ANVIL_ACCOUNT_0_ADDR:-0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266}"

echo "=== EIP-8130 AA Transaction Test ==="
echo "RPC: $L2_RPC"
echo "Sender: $SENDER_ADDR"

# Check devnet is reachable
echo ""
echo "--- Checking devnet ---"
BLOCK=$(cast block-number --rpc-url "$L2_RPC" 2>/dev/null || echo "FAIL")
if [ "$BLOCK" = "FAIL" ]; then
  echo "ERROR: Cannot reach L2 RPC at $L2_RPC"
  echo "Is the devnet running? Try: just devnet up"
  exit 1
fi
echo "Current L2 block: $BLOCK"

# Check sender balance
BALANCE=$(cast balance "$SENDER_ADDR" --rpc-url "$L2_RPC")
echo "Sender balance: $BALANCE"

# Build the raw AA transaction (type 0x05)
#
# EIP-8130 RLP field order:
# [chain_id, from, nonce_key, nonce_sequence, expiry,
#  max_priority_fee_per_gas, max_fee_per_gas, gas_limit,
#  authorization_list, account_changes, calls, payer,
#  sender_auth, payer_auth]
#
# For a minimal self-pay EOA transaction:
# - from = 0x00...00 (EOA mode, sender derived from ecrecover)
# - nonce_key = 0, nonce_sequence = 0
# - expiry = 0 (no expiry)
# - payer = 0x00...00 (self-pay)
# - calls = [[{to: 0xdead, data: 0x}]] (single no-op call)
# - sender_auth = 65-byte K1 ECDSA signature
# - payer_auth = empty

echo ""
echo "--- Building AA transaction ---"

# We'll use a small node script to construct the RLP-encoded AA tx
# since cast doesn't natively support type 0x05 yet.

# First check if node is available
if ! command -v node &>/dev/null; then
  echo "ERROR: node is required to build the AA transaction"
  exit 1
fi

# Create a temporary script that uses viem to build and sign the AA tx
TMPDIR=$(mktemp -d)
cat > "$TMPDIR/send-aa.mjs" << 'SCRIPT'
import { createPublicClient, createWalletClient, http, toHex, toRlp, keccak256, concat, hexToBytes, numberToHex, privateKeyToAccount } from 'viem';

const AA_TX_TYPE_ID = 0x05;
const AA_PAYER_TYPE = 0x06;
const L2_CHAIN_ID = BigInt(process.env.L2_CHAIN_ID || '84538453');
const rpcUrl = process.env.L2_RPC || 'http://localhost:7545';
const privKey = process.env.SENDER_KEY;

function bigintToHex(value) {
  if (value === 0n) return '0x';
  return numberToHex(value);
}

const account = privateKeyToAccount(privKey);
console.log(`Signing as: ${account.address}`);

// Build minimal AA transaction (EOA mode: from = 0x00...00)
const tx = {
  chainId: L2_CHAIN_ID,
  from: '0x0000000000000000000000000000000000000000',
  nonceKey: 0n,
  nonceSequence: 0n,
  expiry: 0n,
  maxPriorityFeePerGas: 1000000000n,
  maxFeePerGas: 2000000000n,
  gasLimit: 100000n,
  authorizationList: [],
  accountChanges: [],
  calls: [[]], // empty phase = no-op
  payer: '0x0000000000000000000000000000000000000000',
};

// Compute sender signature hash:
// keccak256(0x05 || rlp([chainId, from, nonceKey, nonceSequence, expiry,
//   maxPriorityFeePerGas, maxFeePerGas, gasLimit,
//   authorizationList, accountChanges, calls, payer]))
const senderPayload = concat([
  toHex(AA_TX_TYPE_ID, { size: 1 }),
  toRlp([
    bigintToHex(tx.chainId),
    tx.from,
    bigintToHex(tx.nonceKey),
    bigintToHex(tx.nonceSequence),
    bigintToHex(tx.expiry),
    bigintToHex(tx.maxPriorityFeePerGas),
    bigintToHex(tx.maxFeePerGas),
    bigintToHex(tx.gasLimit),
    toRlp([]),  // empty authorization_list
    toRlp([]),  // empty account_changes
    toRlp([toRlp([])]),  // calls: one empty phase
    tx.payer,
  ]),
]);

const senderHash = keccak256(senderPayload);
console.log(`Sender signature hash: ${senderHash}`);

// Sign with K1 (EOA mode: raw 65-byte signature, no type prefix)
const signature = await account.signMessage({ message: { raw: hexToBytes(senderHash) } });
console.log(`Signature: ${signature}`);

// Full EIP-2718 encoding: 0x05 || rlp([...fields, senderAuth, payerAuth])
const encoded = concat([
  toHex(AA_TX_TYPE_ID, { size: 1 }),
  toRlp([
    bigintToHex(tx.chainId),
    tx.from,
    bigintToHex(tx.nonceKey),
    bigintToHex(tx.nonceSequence),
    bigintToHex(tx.expiry),
    bigintToHex(tx.maxPriorityFeePerGas),
    bigintToHex(tx.maxFeePerGas),
    bigintToHex(tx.gasLimit),
    toRlp([]),  // authorization_list
    toRlp([]),  // account_changes
    toRlp([toRlp([])]),  // calls
    tx.payer,
    signature,  // sender_auth (65-byte EOA sig)
    '0x',       // payer_auth (empty, self-pay)
  ]),
]);

const txHash = keccak256(encoded);
console.log(`TX hash: ${txHash}`);
console.log(`Encoded tx length: ${(encoded.length - 2) / 2} bytes`);

// Submit via eth_sendRawTransaction
console.log('');
console.log('--- Submitting to L2 ---');
const client = createPublicClient({ transport: http(rpcUrl) });

try {
  const result = await client.request({
    method: 'eth_sendRawTransaction',
    params: [encoded],
  });
  console.log(`SUCCESS! TX hash from node: ${result}`);
  
  // Wait a bit and check receipt
  console.log('Waiting for receipt...');
  await new Promise(r => setTimeout(r, 3000));
  
  const receipt = await client.request({
    method: 'eth_getTransactionReceipt',
    params: [result],
  });
  if (receipt) {
    console.log(`Receipt status: ${receipt.status}`);
    console.log(`Block number: ${receipt.blockNumber}`);
    console.log(`Gas used: ${receipt.gasUsed}`);
  } else {
    console.log('Receipt not available yet (tx may still be pending)');
  }
} catch (err) {
  console.log(`RPC error: ${err.message || err}`);
  if (err.details) console.log(`Details: ${err.details}`);
  if (err.cause) console.log(`Cause: ${JSON.stringify(err.cause)}`);
  process.exit(1);
}
SCRIPT

# Install viem in the temp dir
echo "Installing viem..."
cd "$TMPDIR"
npm init -y --silent >/dev/null 2>&1
npm install viem --silent 2>/dev/null

echo ""
echo "--- Sending AA transaction ---"
L2_CHAIN_ID="${L2_CHAIN_ID:-84538453}" \
L2_RPC="$L2_RPC" \
SENDER_KEY="$SENDER_KEY" \
node --experimental-vm-modules "$TMPDIR/send-aa.mjs"

# Cleanup
rm -rf "$TMPDIR"
echo ""
echo "=== Done ==="
