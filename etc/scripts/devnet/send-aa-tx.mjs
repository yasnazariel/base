/**
 * Sends an EIP-8130 (type 0x05) AA transaction that calls OwnerIdProbe.probe(),
 * then verifies that owner_id was propagated through TxContext.
 *
 * Usage: node etc/scripts/devnet/send-aa-tx.mjs [--probe <address>]
 *
 * Prerequisites:
 *   - Devnet running with BASE_V1 active (L2_BASE_V1_BLOCK=0)
 *   - npm install viem in this directory (or globally)
 *   - OwnerIdProbe contract deployed (deploy via forge or pass address)
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
  encodeFunctionData,
  decodeFunctionResult,
} from 'viem';
import { privateKeyToAccount } from 'viem/accounts';

const AA_TX_TYPE = 0x05;
const L2_CHAIN_ID = 84538453n;
const RPC_URL = process.env.L2_RPC || 'http://localhost:7545';

// Anvil Account 1 (has 10k ETH on L2)
const SENDER_KEY = '0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d';
const account = privateKeyToAccount(SENDER_KEY);

// OwnerIdProbe contract address (pass via env or CLI arg)
const PROBE_ADDR = process.env.PROBE_ADDR
  || process.argv.find((_, i, a) => a[i - 1] === '--probe')
  || '0x712516e61C8B383dF4A63CFe83d7701Bce54B03e';

const PROBE_ABI = [
  { type: 'function', name: 'probe', inputs: [], outputs: [{ type: 'bytes32' }], stateMutability: 'nonpayable' },
  { type: 'function', name: 'lastOwnerId', inputs: [], outputs: [{ type: 'bytes32' }], stateMutability: 'view' },
];

console.log(`Sender: ${account.address}`);
console.log(`RPC:    ${RPC_URL}`);
console.log(`Chain:  ${L2_CHAIN_ID}`);
console.log(`Probe:  ${PROBE_ADDR}`);

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

// probe() calldata
const probeCalldata = encodeFunctionData({ abi: PROBE_ABI, functionName: 'probe' });
console.log(`\nprobe() calldata: ${probeCalldata}`);

const txFields = {
  chainId: L2_CHAIN_ID,
  from: account.address,
  nonceKey: 0n,
  nonceSequence: currentNonce,
  expiry: 0n,
  maxPriorityFeePerGas: 1000000n,
  maxFeePerGas: 1000000000n,
  gasLimit: 500000n,
};

// calls: one phase with one call to OwnerIdProbe.probe()
const callsRlp = [
  [
    [encodeAddress(PROBE_ADDR), probeCalldata],
  ],
];

// Unsigned field list (for signing hash — includes all fields except sender_auth and payer_auth)
const unsignedRlpFields = [
  encodeUint(txFields.chainId),
  encodeAddress(txFields.from),
  encodeUint(txFields.nonceKey),
  encodeUint(txFields.nonceSequence),
  encodeUint(txFields.expiry),
  encodeUint(txFields.maxPriorityFeePerGas),
  encodeUint(txFields.maxFeePerGas),
  encodeUint(txFields.gasLimit),
  [],              // authorization_list (empty)
  [],              // account_changes (empty)
  callsRlp,        // calls: one phase calling probe()
  encodeAddress('0x0000000000000000000000000000000000000000'),
];

// Compute sender signing hash: keccak256(0x05 || rlp(unsignedFields))
const signingPayload = concat([
  toHex(AA_TX_TYPE, { size: 1 }),
  toRlp(unsignedRlpFields),
]);
const sigHash = keccak256(signingPayload);
console.log(`Sender signing hash: ${sigHash}`);

// Sign with the EOA private key (raw hash signing, not EIP-191 message)
const sig = await account.sign({ hash: sigHash });
console.log(`Signature: ${sig}`);
console.log(`Sig length: ${(sig.length - 2) / 2} bytes`);

// sender_auth: verifier_type (0x01 = K1) || 65-byte ECDSA signature
const senderAuth = concat([toHex(0x01, { size: 1 }), sig]);

// Full EIP-2718 encoded transaction: 0x05 || rlp([...unsignedFields, senderAuth, payerAuth])
const signedRlpFields = [
  ...unsignedRlpFields,
  senderAuth,
  '0x',
];

const encodedTx = concat([
  toHex(AA_TX_TYPE, { size: 1 }),
  toRlp(signedRlpFields),
]);

const txHash = keccak256(encodedTx);
console.log(`\nEncoded tx: ${encodedTx.slice(0, 60)}...`);
console.log(`Encoded length: ${(encodedTx.length - 2) / 2} bytes`);
console.log(`TX hash: ${txHash}`);

// Submit via eth_sendRawTransaction
console.log('\n--- Submitting to L2 RPC ---');
let nodeTxHash;
try {
  nodeTxHash = await client.request({
    method: 'eth_sendRawTransaction',
    params: [encodedTx],
  });
  console.log(`SUCCESS! TX hash from node: ${nodeTxHash}`);
} catch (err) {
  console.log(`\nRPC error: ${err.shortMessage || err.message}`);
  if (err.details) console.log(`Details: ${err.details}`);
  process.exit(1);
}

// Wait for receipt
console.log('Waiting for receipt (5s)...');
await new Promise(r => setTimeout(r, 5000));

const receipt = await client.request({
  method: 'eth_getTransactionReceipt',
  params: [nodeTxHash],
});
if (receipt) {
  console.log(`\n--- Receipt ---`);
  console.log(`Status:       ${receipt.status}`);
  console.log(`Block number: ${receipt.blockNumber}`);
  console.log(`Gas used:     ${receipt.gasUsed}`);
  if (receipt.payer) console.log(`Payer:        ${receipt.payer}`);
  if (receipt.phaseStatuses) console.log(`Phase status: ${JSON.stringify(receipt.phaseStatuses)}`);
} else {
  console.log('Receipt not available yet (tx may still be pending)');
}

// Trace the transaction to check for INVALID opcode issues
console.log('\n--- Call Trace ---');
try {
  const trace = await client.request({
    method: 'debug_traceTransaction',
    params: [nodeTxHash, { tracer: 'callTracer', tracerConfig: { onlyTopCall: false } }],
  });
  console.log(JSON.stringify(trace, null, 2));
} catch (err) {
  console.log(`Trace error: ${err.shortMessage || err.message}`);
}

// Read lastOwnerId() from the probe contract
console.log('\n--- Checking owner_id ---');
try {
  const result = await client.readContract({
    address: PROBE_ADDR,
    abi: PROBE_ABI,
    functionName: 'lastOwnerId',
  });
  console.log(`lastOwnerId: ${result}`);
  if (result === '0x0000000000000000000000000000000000000000000000000000000000000000') {
    console.log('WARNING: owner_id is zero — TxContext.getOwnerId() was not populated');
  } else {
    console.log('SUCCESS: owner_id is non-zero — TxContext precompile is working!');
  }
} catch (err) {
  console.log(`Read error: ${err.shortMessage || err.message}`);
}
