/**
 * Sends a minimal EIP-8130 (type 0x05) AA transaction to the local devnet.
 *
 * Usage: node etc/scripts/devnet/send-aa-tx.mjs
 *
 * Prerequisites:
 *   - Devnet running with BASE_V1 active (L2_BASE_V1_BLOCK=0)
 *   - npm install viem in this directory (or globally)
 */
import {
  createPublicClient,
  http,
  toHex,
  toRlp,
  keccak256,
  concat,
  numberToHex,
  padHex,
} from 'viem';
import { privateKeyToAccount } from 'viem/accounts';

const AA_TX_TYPE = 0x05;
const L2_CHAIN_ID = 84538453n;
const RPC_URL = process.env.L2_RPC || 'http://localhost:7545';

// Anvil Account 1 (has 10k ETH on L2)
const SENDER_KEY = '0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d';
const account = privateKeyToAccount(SENDER_KEY);
console.log(`Sender: ${account.address}`);
console.log(`RPC:    ${RPC_URL}`);
console.log(`Chain:  ${L2_CHAIN_ID}`);

const client = createPublicClient({ transport: http(RPC_URL) });

const blockNum = await client.getBlockNumber();
console.log(`\nCurrent block: ${blockNum}`);

const balance = await client.getBalance({ address: account.address });
console.log(`Sender balance: ${balance} wei (${Number(balance) / 1e18} ETH)`);

// Fetch the current AA nonce from NonceManager storage.
// Slot = keccak256(nonce_key . keccak256(account . NONCE_BASE_SLOT))
const NONCE_MANAGER = '0x000000000000000000000000000000000000Aa02';
const nonceKey = 0n;
const innerHash = keccak256(
  concat([
    padHex(account.address.toLowerCase(), { size: 32, dir: 'left' }),
    padHex('0x1', { size: 32, dir: 'left' }),
  ])
);
const nonceSlot = keccak256(
  concat([
    padHex(toHex(nonceKey), { size: 32, dir: 'left' }),
    innerHash,
  ])
);
const currentNonceHex = await client.getStorageAt({
  address: NONCE_MANAGER,
  slot: nonceSlot,
});
const currentNonce = BigInt(currentNonceHex);
console.log(`AA nonce (key=0): ${currentNonce} (slot: ${nonceSlot})`);

// RLP helpers
function encodeUint(n) {
  if (n === 0n) return '0x';
  return numberToHex(n);
}

function encodeAddress(addr) {
  return addr.toLowerCase();
}

// Build the AA transaction fields.
// `from` must be a funded address so the txpool considers the tx solvent and
// moves it to "pending" (rather than "queued"). For this test, use the same
// account that signs the transaction.
const txFields = {
  chainId: L2_CHAIN_ID,
  from: account.address,
  nonceKey: 0n,
  nonceSequence: currentNonce,
  expiry: 0n,
  maxPriorityFeePerGas: 1000000n,   // 1 gwei
  maxFeePerGas: 1000000000n,         // 1 gwei
  gasLimit: 50000n,
};

// Unsigned field list (for signing hash)
const unsignedRlpFields = [
  encodeUint(txFields.chainId),                // chain_id
  encodeAddress(txFields.from),                 // from (0x00...00 = EOA mode)
  encodeUint(txFields.nonceKey),                // nonce_key
  encodeUint(txFields.nonceSequence),           // nonce_sequence
  encodeUint(txFields.expiry),                  // expiry (0 = no expiry)
  encodeUint(txFields.maxPriorityFeePerGas),    // max_priority_fee_per_gas
  encodeUint(txFields.maxFeePerGas),            // max_fee_per_gas
  encodeUint(txFields.gasLimit),                // gas_limit
  [],                                           // authorization_list (empty)
  [],                                           // account_changes (empty)
  [[]],                                         // calls: one empty phase (no-op)
  encodeAddress('0x0000000000000000000000000000000000000000'), // payer (self-pay)
];

// Compute sender signing hash: keccak256(0x05 || rlp(unsignedFields))
const signingPayload = concat([
  toHex(AA_TX_TYPE, { size: 1 }),
  toRlp(unsignedRlpFields),
]);
const sigHash = keccak256(signingPayload);
console.log(`\nSender signing hash: ${sigHash}`);

// Sign with the EOA private key (raw hash signing, not EIP-191 message)
const sig = await account.sign({ hash: sigHash });
console.log(`Signature: ${sig}`);
console.log(`Sig length: ${(sig.length - 2) / 2} bytes`);

// In configured mode (from != 0x00), sender_auth = verifier_type || data.
// K1 verifier type = 0x01, data = 65-byte ECDSA signature.
const senderAuth = concat([toHex(0x01, { size: 1 }), sig]);

// Full EIP-2718 encoded transaction: 0x05 || rlp([...unsignedFields, senderAuth, payerAuth])
const signedRlpFields = [
  ...unsignedRlpFields,
  senderAuth,  // sender_auth: 0x01 (K1) || 65-byte ECDSA signature
  '0x',        // payer_auth: empty (self-pay)
];

const encodedTx = concat([
  toHex(AA_TX_TYPE, { size: 1 }),
  toRlp(signedRlpFields),
]);

const txHash = keccak256(encodedTx);
console.log(`\nEncoded tx: ${encodedTx.slice(0, 40)}...`);
console.log(`Encoded length: ${(encodedTx.length - 2) / 2} bytes`);
console.log(`TX hash: ${txHash}`);

// Debug: dump hex
console.log(`\nFull encoded tx hex:`);
console.log(encodedTx);

// Submit via eth_sendRawTransaction
console.log('\n--- Submitting to L2 RPC ---');
try {
  const result = await client.request({
    method: 'eth_sendRawTransaction',
    params: [encodedTx],
  });
  console.log(`\nSUCCESS! TX hash from node: ${result}`);

  console.log('Waiting for receipt (5s)...');
  await new Promise(r => setTimeout(r, 5000));

  const receipt = await client.request({
    method: 'eth_getTransactionReceipt',
    params: [result],
  });
  if (receipt) {
    console.log(`Receipt status: ${receipt.status}`);
    console.log(`Block number:   ${receipt.blockNumber}`);
    console.log(`Gas used:       ${receipt.gasUsed}`);
  } else {
    console.log('Receipt not available yet (tx may still be pending)');
  }
} catch (err) {
  console.log(`\nRPC error: ${err.shortMessage || err.message}`);
  if (err.details) console.log(`Details: ${err.details}`);
  process.exit(1);
}
