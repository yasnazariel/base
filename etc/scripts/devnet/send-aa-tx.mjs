/**
 * Sends EIP-8130 (type 0x7B) AA transactions against a local devnet.
 *
 * Usage:
 *   node send-aa-tx.mjs [mode] [options]
 *
 * Modes:
 *   probe        (default) Single-phase call to OwnerIdProbe.probe(), checks owner_id
 *   multi-call   Two-phase tx: phase 0 calls probe(), phase 1 sends ETH transfer
 *   sponsor      Sponsored tx: separate payer signs with AA_PAYER_TYPE, verifies gas billing
 *   config-change  Authorize a new owner via ConfigChangeEntry, verify storage + sequence
 *   p256         Register P256 owner + send P256-signed tx (two-step secp256r1 flow)
 *   webauthn     Register WebAuthn owner + send WebAuthn-signed tx (P256 + assertion envelope)
 *   receipt-test Verify receipt fields (status, payer, phaseStatuses) across scenarios
 *   deploy       Creates a new smart account via account_changes (CREATE entry)
 *   nonce-rpc    Verify eth_getTransactionCount(nonceKey) RPC matches storage reads + increments
 *   estimate-gas Verify eth_estimateGas / eth_call work with type 0x7B AA requests
 *   custom-verifier  Deploy + register AlwaysValidVerifier as custom EVM verifier (scope=SENDER+PAYER)
 *   delegate-native  Delegate verifier with K1 inner (native path, no on-chain call)
 *   delegate-p256    Delegate verifier with P256 inner (native delegation + P256 signature)
 *   owner-change-signing  Revoke EOA + add P256 in owner_changes; revoked signer fails, new P256 signer passes
 *   nonceless        Send a nonce-free tx (NONCE_KEY_MAX + expiry), verify no replay
 *   delegation       Set/change EIP-7702 code delegation via account_changes entry (type 0x02)
 *   locked-config    Lock an account then verify config changes are rejected (run last — locks account for 600s)
 *
 * Options:
 *   --probe <addr>    OwnerIdProbe contract address
 *   --rpc <url>       L2 RPC endpoint (default: http://localhost:7545)
 *   --no-trace        Skip debug_traceTransaction
 *
 * Prerequisites:
 *   - Devnet running with BASE_V1 active (L2_BASE_V1_BLOCK=0)
 *   - npm install viem in this directory (or globally)
 *   - OwnerIdProbe contract deployed (for probe / multi-call modes)
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
  encodeAbiParameters,
  parseAbiParameters,
} from 'viem';
import { privateKeyToAccount } from 'viem/accounts';
import { p256 as p256curve } from '@noble/curves/p256';

import { readFileSync } from 'fs';
import { createHash } from 'crypto';
import { resolve, dirname } from 'path';
import { fileURLToPath } from 'url';

// ─────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────
const AA_TX_TYPE = 0x7B;
const AA_PAYER_TYPE = 0x7C;
const L2_CHAIN_ID = 84538453n;

const NONCE_MANAGER_ADDRESS = '0x000000000000000000000000000000000000Aa02';

// Deployed contract addresses — loaded from deploy-8130.sh output if available,
// otherwise fall back to provisional values matching predeploys.rs.
const FALLBACK_ADDRESSES = {
  accountConfiguration: '0x47B8020ea35AbeBD959cEEf7a0D1bEae19d8cA21',
  defaultAccount:       '0x19E994e7Fe4a114A3E40a989Cc5F5f2324E7E21d',
  k1Verifier:           '0x6E03196230De715554734a73058dA27AdfE2A7A9',
  p256Verifier:         '0x75E9779603e826f2D8d4dD7Edee3F0a737e4228d',
  webAuthnVerifier:     '0xb2c8b7ec119882fBcc32FDe1be1341e19a5Bd53E',
  delegateVerifier:     '0x149A439e8ea89541d8A1d2Ab046E39b0A91D0843',
  alwaysValidVerifier:  '0x6812F1aab1dd53e3f6705de05b96D3b93f3503D8',
};

function loadDeployedAddresses() {
  const __dirname = dirname(fileURLToPath(import.meta.url));
  const addrFile = resolve(__dirname, '../../../.devnet/l2/8130-addresses.json');
  try {
    const json = JSON.parse(readFileSync(addrFile, 'utf-8'));
    console.log(`Loaded 8130 addresses from ${addrFile}`);
    return json;
  } catch {
    console.log('No 8130-addresses.json found, using provisional addresses.');
    console.log('Run deploy-8130.sh after devnet start to deploy system contracts.');
    return FALLBACK_ADDRESSES;
  }
}

const DEPLOYED = loadDeployedAddresses();
const ACCOUNT_CONFIG_ADDRESS  = DEPLOYED.accountConfiguration;
const DEFAULT_ACCOUNT_ADDR    = DEPLOYED.defaultAccount;
const K1_VERIFIER_ADDRESS     = DEPLOYED.k1Verifier;
const P256_VERIFIER_ADDRESS   = DEPLOYED.p256Verifier;
const WEBAUTHN_VERIFIER_ADDRESS = DEPLOYED.webAuthnVerifier;
const DELEGATE_VERIFIER_ADDRESS = DEPLOYED.delegateVerifier;
const ALWAYS_VALID_VERIFIER_ADDRESS = DEPLOYED.alwaysValidVerifier;

// keccak256("ALWAYS_VALID") — fixed owner ID returned by AlwaysValidVerifier
const ALWAYS_VALID_OWNER_ID = keccak256(toHex(new TextEncoder().encode('ALWAYS_VALID')));

const SENDER_KEY = '0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d';
const PAYER_KEY  = '0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a';
const DELEGATE_KEY = '0x47e179ec197488593b187f80a00eb0da91f1b9d0b13f8733639f19c30a34926a';

const CONFIG_CHANGE_TYPEHASH = keccak256(
  toHex('SignedOwnerChanges(address account,uint64 chainId,uint64 sequence,OwnerChange[] ownerChanges)OwnerChange(uint8 changeType,address verifier,bytes32 ownerId,uint8 scope)')
);

const PROBE_ABI = [
  { type: 'function', name: 'probe', inputs: [], outputs: [{ type: 'bytes32' }], stateMutability: 'nonpayable' },
  { type: 'function', name: 'lastOwnerId', inputs: [], outputs: [{ type: 'bytes32' }], stateMutability: 'view' },
];

// ─────────────────────────────────────────────────
// CLI Parsing
// ─────────────────────────────────────────────────
function parseArgs() {
  const args = process.argv.slice(2);
  const opts = {
    mode: 'probe',
    probeAddr: process.env.PROBE_ADDR || '0x8464135c8F25Da09e49BC8782676a84730C318bC',
    rpc: process.env.L2_RPC || 'http://localhost:7545',
    trace: true,
  };

  for (let i = 0; i < args.length; i++) {
    const arg = args[i];
    if (arg === '--probe')     { opts.probeAddr = args[++i]; continue; }
    if (arg === '--rpc')       { opts.rpc = args[++i]; continue; }
    if (arg === '--no-trace')  { opts.trace = false; continue; }
    if (!arg.startsWith('-'))  { opts.mode = arg; }
  }

  return opts;
}

const opts = parseArgs();
const account = privateKeyToAccount(SENDER_KEY);
const client = createPublicClient({ transport: http(opts.rpc) });

console.log(`Mode:   ${opts.mode}`);
console.log(`Sender: ${account.address}`);
console.log(`RPC:    ${opts.rpc}`);
console.log(`Chain:  ${L2_CHAIN_ID}`);

// ─────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────
function encodeUint(n) {
  if (n === 0n) return '0x';
  return numberToHex(n);
}

function encodeAddress(addr) {
  return addr.toLowerCase();
}

async function getAaNonce() {
  const innerHash = keccak256(
    concat([
      padHex(account.address.toLowerCase(), { size: 32, dir: 'left' }),
      padHex('0x1', { size: 32, dir: 'left' }),
    ])
  );
  const nonceSlot = keccak256(
    concat([
      padHex('0x0', { size: 32, dir: 'left' }),
      innerHash,
    ])
  );
  const hex = await client.getStorageAt({
    address: NONCE_MANAGER_ADDRESS,
    slot: nonceSlot,
  });
  return BigInt(hex);
}

async function getAaNonceViaRpc(address, nonceKey = 0n) {
  const result = await client.request({
    method: 'eth_getTransactionCount',
    params: [address, 'latest', numberToHex(nonceKey)],
  });
  return BigInt(result);
}

function ownerConfigSlot(accountAddr, ownerId) {
  const inner = keccak256(concat([
    padHex(accountAddr.toLowerCase(), { size: 32, dir: 'left' }),
    padHex('0x0', { size: 32, dir: 'left' }),
  ]));
  return keccak256(concat([ownerId, inner]));
}

function sequenceSlot(accountAddr) {
  return keccak256(concat([
    padHex(accountAddr.toLowerCase(), { size: 32, dir: 'left' }),
    padHex('0x2', { size: 32, dir: 'left' }),
  ]));
}

function configChangeDigest(accountAddr, chainId, sequence, ownerChanges) {
  const changeHashes = ownerChanges.map(oc => keccak256(
    encodeAbiParameters(
      parseAbiParameters('uint8, address, bytes32, uint8'),
      [oc.changeType, oc.verifier, oc.ownerId, oc.scope]
    )
  ));
  const ownerChangesHash = keccak256(concat(changeHashes));
  return keccak256(
    encodeAbiParameters(
      parseAbiParameters('bytes32, address, uint64, uint64, bytes32'),
      [CONFIG_CHANGE_TYPEHASH, accountAddr, chainId, sequence, ownerChangesHash]
    )
  );
}

async function signAndSend(unsignedRlpFields, {
  trace = true,
  payerAccount = null,
  customSenderAuth = null,
  exitOnError = true,
} = {}) {
  const signingPayload = concat([
    toHex(AA_TX_TYPE, { size: 1 }),
    toRlp(unsignedRlpFields),
  ]);
  const sigHash = keccak256(signingPayload);
  console.log(`Sender signing hash: ${sigHash}`);

  let senderAuth;
  if (customSenderAuth) {
    senderAuth = await Promise.resolve(customSenderAuth(sigHash));
    console.log(`Using custom sender auth (${(senderAuth.length - 2) / 2} bytes)`);
  } else {
    const sig = await account.sign({ hash: sigHash });
    senderAuth = concat([K1_VERIFIER_ADDRESS, sig]);
  }

  let payerAuth = '0x';
  if (payerAccount) {
    const payerSigningFields = unsignedRlpFields.slice(0, -1);
    const payerPayload = concat([
      toHex(AA_PAYER_TYPE, { size: 1 }),
      toRlp(payerSigningFields),
    ]);
    const payerSigHash = keccak256(payerPayload);
    console.log(`Payer signing hash:  ${payerSigHash}`);

    const payerSig = await payerAccount.sign({ hash: payerSigHash });
    payerAuth = concat([K1_VERIFIER_ADDRESS, payerSig]);
  }

  const signedRlpFields = [
    ...unsignedRlpFields,
    senderAuth,
    payerAuth,
  ];

  const encodedTx = concat([
    toHex(AA_TX_TYPE, { size: 1 }),
    toRlp(signedRlpFields),
  ]);

  const txHash = keccak256(encodedTx);
  console.log(`\nEncoded tx: ${encodedTx.slice(0, 80)}...`);
  console.log(`Encoded length: ${(encodedTx.length - 2) / 2} bytes`);
  console.log(`TX hash: ${txHash}`);

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
    if (exitOnError) {
      process.exit(1);
    }
    throw err;
  }

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
    console.log(`Logs:         ${receipt.logs?.length || 0}`);
    if (receipt.payer) console.log(`Payer:        ${receipt.payer}`);
    if (receipt.phaseStatuses) console.log(`Phase status: ${JSON.stringify(receipt.phaseStatuses)}`);
  } else {
    console.log('Receipt not available yet (tx may still be pending)');
  }

  if (trace) {
    console.log('\n--- Call Trace ---');
    try {
      const traceResult = await client.request({
        method: 'debug_traceTransaction',
        params: [nodeTxHash, { tracer: 'callTracer', tracerConfig: { onlyTopCall: false } }],
      });
      console.log(JSON.stringify(traceResult, null, 2));
    } catch (err) {
      console.log(`Trace error: ${err.shortMessage || err.message}`);
    }
  }

  return { nodeTxHash, receipt };
}

function baseTxFields(nonce, callsRlp, accountChangesRlp = [], payerAddress = '0x0000000000000000000000000000000000000000', { nonceKey = 0n, expiry = 0n } = {}) {
  return [
    encodeUint(L2_CHAIN_ID),
    encodeAddress(account.address),
    encodeUint(nonceKey),
    encodeUint(nonce),
    encodeUint(expiry),
    encodeUint(1000000n),
    encodeUint(1000000000n),
    encodeUint(500000n),
    accountChangesRlp,
    callsRlp,
    encodeAddress(payerAddress),
  ];
}

// ─────────────────────────────────────────────────
// Mode: probe (default)
// ─────────────────────────────────────────────────
async function runProbe() {
  console.log(`\nProbe:  ${opts.probeAddr}`);

  const nonce = await getAaNonce();
  console.log(`AA nonce (key=0): ${nonce}`);

  const probeCalldata = encodeFunctionData({ abi: PROBE_ABI, functionName: 'probe' });
  console.log(`probe() calldata: ${probeCalldata}`);

  const callsRlp = [
    [[encodeAddress(opts.probeAddr), probeCalldata]],
  ];

  const unsigned = baseTxFields(nonce, callsRlp);
  const { receipt } = await signAndSend(unsigned, { trace: opts.trace });

  console.log('\n--- Checking owner_id ---');
  try {
    const result = await client.readContract({
      address: opts.probeAddr,
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
}

// ─────────────────────────────────────────────────
// Mode: multi-call
// ─────────────────────────────────────────────────
async function runMultiCall() {
  console.log(`\nProbe:  ${opts.probeAddr}`);

  const nonce = await getAaNonce();
  console.log(`AA nonce (key=0): ${nonce}`);

  const probeCalldata = encodeFunctionData({ abi: PROBE_ABI, functionName: 'probe' });

  // ETH transfer target (Anvil Account 2)
  const ethTarget = '0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC';
  const ethValue = 1000000000000000n; // 0.001 ETH

  // Two phases: phase 0 = probe, phase 1 = ETH transfer (empty calldata)
  const callsRlp = [
    [[encodeAddress(opts.probeAddr), probeCalldata]],
    [[encodeAddress(ethTarget), '0x']],
  ];

  console.log(`Phase 0: call probe() on ${opts.probeAddr}`);
  console.log(`Phase 1: transfer ${ethValue} wei to ${ethTarget}`);

  const unsigned = baseTxFields(nonce, callsRlp);
  const { receipt } = await signAndSend(unsigned, { trace: opts.trace });

  if (receipt?.phaseStatuses) {
    console.log(`\n--- Phase Statuses ---`);
    receipt.phaseStatuses.forEach((s, i) => {
      console.log(`  Phase ${i}: ${s ? 'SUCCESS' : 'REVERTED'}`);
    });
  }

  console.log('\n--- Checking owner_id ---');
  try {
    const result = await client.readContract({
      address: opts.probeAddr,
      abi: PROBE_ABI,
      functionName: 'lastOwnerId',
    });
    console.log(`lastOwnerId: ${result}`);
    if (result !== '0x0000000000000000000000000000000000000000000000000000000000000000') {
      console.log('SUCCESS: owner_id set in multi-call tx!');
    }
  } catch (err) {
    console.log(`Read error: ${err.shortMessage || err.message}`);
  }

  console.log('\n--- ETH Transfer Check ---');
  try {
    const bal = await client.getBalance({ address: ethTarget });
    console.log(`${ethTarget} balance: ${bal} wei`);
  } catch (err) {
    console.log(`Balance read error: ${err.shortMessage || err.message}`);
  }
}

// ─────────────────────────────────────────────────
// Mode: sponsor
// ─────────────────────────────────────────────────
async function runSponsor() {
  const payerAcct = privateKeyToAccount(PAYER_KEY);
  console.log(`\nPayer:  ${payerAcct.address}`);
  console.log(`Probe:  ${opts.probeAddr}`);

  const nonce = await getAaNonce();
  console.log(`AA nonce (key=0): ${nonce}`);

  const senderBalBefore = await client.getBalance({ address: account.address });
  const payerBalBefore = await client.getBalance({ address: payerAcct.address });
  console.log(`\nSender balance before: ${senderBalBefore} wei`);
  console.log(`Payer balance before:  ${payerBalBefore} wei`);

  const probeCalldata = encodeFunctionData({ abi: PROBE_ABI, functionName: 'probe' });
  const callsRlp = [
    [[encodeAddress(opts.probeAddr), probeCalldata]],
  ];

  const unsigned = baseTxFields(nonce, callsRlp, [], payerAcct.address);
  const { receipt } = await signAndSend(unsigned, { trace: opts.trace, payerAccount: payerAcct });

  const senderBalAfter = await client.getBalance({ address: account.address });
  const payerBalAfter = await client.getBalance({ address: payerAcct.address });

  console.log(`\n--- Sponsorship Verification ---`);
  console.log(`Sender balance after:  ${senderBalAfter} wei`);
  console.log(`Payer balance after:   ${payerBalAfter} wei`);

  const senderDelta = senderBalBefore - senderBalAfter;
  const payerDelta = payerBalBefore - payerBalAfter;
  console.log(`Sender delta: ${senderDelta} wei`);
  console.log(`Payer delta:  ${payerDelta} wei`);

  if (senderDelta === 0n && payerDelta > 0n) {
    console.log('SUCCESS: Payer covered gas — sender balance unchanged!');
  } else if (senderDelta === 0n) {
    console.log('WARNING: Sender unchanged but payer also unchanged — check receipt');
  } else {
    console.log('UNEXPECTED: Sender balance changed — gas may not be sponsored correctly');
  }

  if (receipt?.payer) {
    console.log(`\nReceipt payer: ${receipt.payer}`);
    if (receipt.payer.toLowerCase() === payerAcct.address.toLowerCase()) {
      console.log('SUCCESS: Receipt payer matches sponsor address!');
    } else {
      console.log(`MISMATCH: Expected ${payerAcct.address}, got ${receipt.payer}`);
    }
  } else {
    console.log('\nWARNING: Receipt has no payer field');
  }

  console.log('\n--- Checking owner_id ---');
  try {
    const result = await client.readContract({
      address: opts.probeAddr,
      abi: PROBE_ABI,
      functionName: 'lastOwnerId',
    });
    console.log(`lastOwnerId: ${result}`);
    if (result !== '0x0000000000000000000000000000000000000000000000000000000000000000') {
      console.log('SUCCESS: owner_id set in sponsored tx!');
    } else {
      console.log('WARNING: owner_id is zero — TxContext.getOwnerId() was not populated');
    }
  } catch (err) {
    console.log(`Read error: ${err.shortMessage || err.message}`);
  }
}

// ─────────────────────────────────────────────────
// Mode: config-change
// ─────────────────────────────────────────────────
async function runConfigChange() {
  const newOwnerAddr = '0x90F79bf6EB2c4f870365E785982E1f101E93b906';
  const newOwnerId = padHex(newOwnerAddr.toLowerCase(), { size: 32, dir: 'right' });

  console.log('\n--- Config Change: Authorize New Owner ---');
  console.log(`Account:     ${account.address}`);
  console.log(`New owner:   ${newOwnerAddr}`);
  console.log(`Owner ID:    ${newOwnerId}`);
  console.log(`Verifier:    ${K1_VERIFIER_ADDRESS} (K1)`);

  const nonce = await getAaNonce();
  console.log(`AA nonce (key=0): ${nonce}`);

  const seqSlotHash = sequenceSlot(account.address);
  const packedSeq = await client.getStorageAt({
    address: ACCOUNT_CONFIG_ADDRESS,
    slot: seqSlotHash,
  });
  const currentSeq = BigInt(packedSeq || '0x0') & ((1n << 64n) - 1n);
  console.log(`Multichain sequence: ${currentSeq}`);

  const operation = {
    changeType: 1,
    verifier: K1_VERIFIER_ADDRESS,
    ownerId: newOwnerId,
    scope: 0,
  };

  const digest = configChangeDigest(account.address, 0n, currentSeq, [operation]);
  console.log(`Config change digest: ${digest}`);

  const authSig = await account.sign({ hash: digest });
  const authorizerAuth = concat([K1_VERIFIER_ADDRESS, authSig]);

  const configChangeRlp = [
    toHex(0x01, { size: 1 }),
    encodeUint(0n),
    encodeUint(currentSeq),
    [
      [
        toHex(0x01, { size: 1 }),
        encodeAddress(K1_VERIFIER_ADDRESS),
        newOwnerId,
        '0x',
      ],
    ],
    authorizerAuth,
  ];

  const probeCalldata = encodeFunctionData({ abi: PROBE_ABI, functionName: 'probe' });
  const callsRlp = [
    [[encodeAddress(opts.probeAddr), probeCalldata]],
  ];

  const unsigned = baseTxFields(nonce, callsRlp, [configChangeRlp]);
  const { receipt } = await signAndSend(unsigned, { trace: opts.trace });

  console.log('\n--- Verification: Owner Registration ---');
  const ownerSlotHash = ownerConfigSlot(account.address, newOwnerId);
  try {
    const ownerConfig = await client.getStorageAt({
      address: ACCOUNT_CONFIG_ADDRESS,
      slot: ownerSlotHash,
    });
    console.log(`Owner config slot:  ${ownerSlotHash}`);
    console.log(`Owner config value: ${ownerConfig}`);

    if (ownerConfig && ownerConfig !== '0x0000000000000000000000000000000000000000000000000000000000000000') {
      const verifierHex = '0x' + ownerConfig.slice(-40);
      const scopeByte = parseInt(ownerConfig.slice(24, 26), 16);
      console.log(`Verifier: ${verifierHex}`);
      console.log(`Scope:    0x${scopeByte.toString(16).padStart(2, '0')}`);
      if (verifierHex.toLowerCase() === K1_VERIFIER_ADDRESS.toLowerCase()) {
        console.log('SUCCESS: New K1 owner registered via config change!');
      } else {
        console.log(`MISMATCH: Expected verifier ${K1_VERIFIER_ADDRESS}`);
      }
    } else {
      console.log('FAILED: Owner config slot is empty — registration did not persist');
    }
  } catch (err) {
    console.log(`Owner check error: ${err.shortMessage || err.message}`);
  }

  console.log('\n--- Verification: Sequence Bump ---');
  try {
    const packedSeqAfter = await client.getStorageAt({
      address: ACCOUNT_CONFIG_ADDRESS,
      slot: seqSlotHash,
    });
    const seqAfter = BigInt(packedSeqAfter || '0x0') & ((1n << 64n) - 1n);
    console.log(`Multichain sequence after: ${seqAfter}`);
    if (seqAfter === currentSeq + 1n) {
      console.log('SUCCESS: Sequence bumped correctly!');
    } else {
      console.log(`UNEXPECTED: Expected ${currentSeq + 1n}, got ${seqAfter}`);
    }
  } catch (err) {
    console.log(`Sequence check error: ${err.shortMessage || err.message}`);
  }
}

// ─────────────────────────────────────────────────
// Mode: p256
// ─────────────────────────────────────────────────
async function runP256() {
  console.log('\n--- P256 Verification E2E ---');

  const p256PrivateKey = p256curve.utils.randomPrivateKey();
  const p256PubUncompressed = p256curve.getPublicKey(p256PrivateKey, false);
  const p256PubRaw = p256PubUncompressed.slice(1);
  const p256OwnerId = keccak256(toHex(p256PubRaw));

  console.log(`P256 public key: ${toHex(p256PubRaw).slice(0, 40)}...`);
  console.log(`P256 owner ID:   ${p256OwnerId}`);
  console.log(`P256 verifier:   ${P256_VERIFIER_ADDRESS}`);

  // Step 1: Register P256 owner via config change (K1-signed)
  console.log('\n--- Step 1: Register P256 owner ---');

  const nonce1 = await getAaNonce();
  console.log(`AA nonce (key=0): ${nonce1}`);

  const seqSlotHash = sequenceSlot(account.address);
  const packedSeq = await client.getStorageAt({
    address: ACCOUNT_CONFIG_ADDRESS,
    slot: seqSlotHash,
  });
  const currentSeq = BigInt(packedSeq || '0x0') & ((1n << 64n) - 1n);
  console.log(`Multichain sequence: ${currentSeq}`);

  const operation = {
    changeType: 1,
    verifier: P256_VERIFIER_ADDRESS,
    ownerId: p256OwnerId,
    scope: 0,
  };

  const digest = configChangeDigest(account.address, 0n, currentSeq, [operation]);
  const authSig = await account.sign({ hash: digest });
  const authorizerAuth = concat([K1_VERIFIER_ADDRESS, authSig]);

  const configChangeRlp = [
    toHex(0x01, { size: 1 }),
    encodeUint(0n),
    encodeUint(currentSeq),
    [
      [
        toHex(0x01, { size: 1 }),
        encodeAddress(P256_VERIFIER_ADDRESS),
        p256OwnerId,
        '0x',
      ],
    ],
    authorizerAuth,
  ];

  const setupCalldata = encodeFunctionData({ abi: PROBE_ABI, functionName: 'probe' });
  const setupCallsRlp = [
    [[encodeAddress(opts.probeAddr), setupCalldata]],
  ];

  const unsigned1 = baseTxFields(nonce1, setupCallsRlp, [configChangeRlp]);
  const { receipt: receipt1 } = await signAndSend(unsigned1, { trace: false });
  console.log(`Config change tx status: ${receipt1?.status}`);

  const ownerSlotHash = ownerConfigSlot(account.address, p256OwnerId);
  const ownerConfig = await client.getStorageAt({
    address: ACCOUNT_CONFIG_ADDRESS,
    slot: ownerSlotHash,
  });
  const verifierHex = '0x' + ownerConfig.slice(-40);
  console.log(`Owner config verifier: ${verifierHex}`);

  if (verifierHex.toLowerCase() !== P256_VERIFIER_ADDRESS.toLowerCase()) {
    console.log('FAILED: P256 verifier not registered correctly');
    return;
  }
  console.log('SUCCESS: P256 owner registered in AccountConfig');

  // Step 2: Send AA tx signed with P256
  console.log('\n--- Step 2: Send P256-signed AA tx ---');

  const nonce2 = await getAaNonce();
  console.log(`AA nonce (key=0): ${nonce2}`);

  const probeCalldata = encodeFunctionData({ abi: PROBE_ABI, functionName: 'probe' });
  const callsRlp = [
    [[encodeAddress(opts.probeAddr), probeCalldata]],
  ];

  const p256SenderAuth = (sigHash) => {
    const hashBytes = sigHash.slice(2);
    const hashArr = new Uint8Array(hashBytes.match(/.{2}/g).map(b => parseInt(b, 16)));
    const sig = p256curve.sign(hashArr, p256PrivateKey, { lowS: true });
    const rBytes = sig.r.toString(16).padStart(64, '0');
    const sBytes = sig.s.toString(16).padStart(64, '0');
    return concat([
      P256_VERIFIER_ADDRESS,
      toHex(p256PubRaw),
      '0x' + rBytes + sBytes,
    ]);
  };

  const unsigned2 = baseTxFields(nonce2, callsRlp);
  const { receipt: receipt2 } = await signAndSend(unsigned2, {
    trace: opts.trace,
    customSenderAuth: p256SenderAuth,
  });

  console.log('\n--- P256 Verification Results ---');
  if (receipt2?.status === '0x1') {
    console.log('SUCCESS: P256-signed AA transaction executed!');
  } else {
    console.log(`Status: ${receipt2?.status || 'unknown'}`);
  }
}

// ─────────────────────────────────────────────────
// Mode: webauthn
// ─────────────────────────────────────────────────

function sha256(data) {
  return createHash('sha256').update(data).digest();
}

function base64UrlEncode(buf) {
  return Buffer.from(buf)
    .toString('base64')
    .replace(/\+/g, '-')
    .replace(/\//g, '_')
    .replace(/=+$/, '');
}

async function runWebAuthn() {
  console.log('\n--- WebAuthn (P256) Verification E2E ---');

  const p256PrivateKey = p256curve.utils.randomPrivateKey();
  const p256PubUncompressed = p256curve.getPublicKey(p256PrivateKey, false);
  const p256PubRaw = p256PubUncompressed.slice(1);
  const p256OwnerId = keccak256(toHex(p256PubRaw));

  console.log(`P256 public key:     ${toHex(p256PubRaw).slice(0, 40)}...`);
  console.log(`P256 owner ID:       ${p256OwnerId}`);
  console.log(`WebAuthn verifier:   ${WEBAUTHN_VERIFIER_ADDRESS}`);

  // Step 1: Register WebAuthn owner via config change (K1-signed)
  console.log('\n--- Step 1: Register WebAuthn owner ---');

  const nonce1 = await getAaNonce();
  const seqSlotHash = sequenceSlot(account.address);
  const packedSeq = await client.getStorageAt({
    address: ACCOUNT_CONFIG_ADDRESS,
    slot: seqSlotHash,
  });
  const currentSeq = BigInt(packedSeq || '0x0') & ((1n << 64n) - 1n);

  const operation = {
    changeType: 1,
    verifier: WEBAUTHN_VERIFIER_ADDRESS,
    ownerId: p256OwnerId,
    scope: 0,
  };

  const digest = configChangeDigest(account.address, 0n, currentSeq, [operation]);
  const authSig = await account.sign({ hash: digest });
  const authorizerAuth = concat([K1_VERIFIER_ADDRESS, authSig]);

  const configChangeRlp = [
    toHex(0x01, { size: 1 }),
    encodeUint(0n),
    encodeUint(currentSeq),
    [
      [
        toHex(0x01, { size: 1 }),
        encodeAddress(WEBAUTHN_VERIFIER_ADDRESS),
        p256OwnerId,
        '0x',
      ],
    ],
    authorizerAuth,
  ];

  const setupCalldata = encodeFunctionData({ abi: PROBE_ABI, functionName: 'probe' });
  const setupCallsRlp = [
    [[encodeAddress(opts.probeAddr), setupCalldata]],
  ];

  const unsigned1 = baseTxFields(nonce1, setupCallsRlp, [configChangeRlp]);
  const { receipt: receipt1 } = await signAndSend(unsigned1, { trace: false });
  console.log(`Config change tx status: ${receipt1?.status}`);

  const ownerSlotHash = ownerConfigSlot(account.address, p256OwnerId);
  const ownerConfig = await client.getStorageAt({
    address: ACCOUNT_CONFIG_ADDRESS,
    slot: ownerSlotHash,
  });
  const verifierHex = '0x' + ownerConfig.slice(-40);
  console.log(`Owner config verifier: ${verifierHex}`);

  if (verifierHex.toLowerCase() !== WEBAUTHN_VERIFIER_ADDRESS.toLowerCase()) {
    console.log('FAILED: WebAuthn verifier not registered correctly');
    return;
  }
  console.log('SUCCESS: WebAuthn owner registered in AccountConfig');

  // Step 2: Send AA tx signed with WebAuthn P256 assertion
  console.log('\n--- Step 2: Send WebAuthn-signed AA tx ---');

  const nonce2 = await getAaNonce();
  const probeCalldata = encodeFunctionData({ abi: PROBE_ABI, functionName: 'probe' });
  const callsRlp = [
    [[encodeAddress(opts.probeAddr), probeCalldata]],
  ];

  const webauthnSenderAuth = (sigHash) => {
    const challengeBytes = Buffer.from(sigHash.slice(2), 'hex');
    const challenge = base64UrlEncode(challengeBytes);

    const rpIdHash = sha256(Buffer.from('localhost'));
    const flags = Buffer.from([0x05]); // UP + UV
    const signCount = Buffer.alloc(4);
    const authenticatorData = Buffer.concat([rpIdHash, flags, signCount]);

    const clientDataJSON = JSON.stringify({
      type: 'webauthn.get',
      challenge,
      origin: 'http://localhost:3000',
      crossOrigin: false,
    });
    const clientDataBytes = Buffer.from(clientDataJSON, 'utf-8');

    const clientDataHash = sha256(clientDataBytes);
    const signedData = Buffer.concat([authenticatorData, clientDataHash]);
    const signedDataHash = sha256(signedData);

    const sig = p256curve.sign(signedDataHash, p256PrivateKey, { lowS: true });
    const rBytes = sig.r.toString(16).padStart(64, '0');
    const sBytes = sig.s.toString(16).padStart(64, '0');

    const clientDataLenBuf = Buffer.alloc(4);
    clientDataLenBuf.writeUInt32BE(clientDataBytes.length);

    // Envelope: pubKey(64) || authenticatorData(37) || clientDataJSONLen(4, BE) || clientDataJSON || sig(64)
    const envelope = Buffer.concat([
      Buffer.from(p256PubRaw),
      authenticatorData,
      clientDataLenBuf,
      clientDataBytes,
      Buffer.from(rBytes + sBytes, 'hex'),
    ]);

    return concat([
      WEBAUTHN_VERIFIER_ADDRESS,
      toHex(envelope),
    ]);
  };

  const unsigned2 = baseTxFields(nonce2, callsRlp);
  const { receipt: receipt2 } = await signAndSend(unsigned2, {
    trace: opts.trace,
    customSenderAuth: webauthnSenderAuth,
  });

  console.log('\n--- WebAuthn Verification Results ---');
  if (receipt2?.status === '0x1') {
    console.log('SUCCESS: WebAuthn-signed AA transaction executed!');
  } else {
    console.log(`FAILED: status=${receipt2?.status || 'unknown'}`);
  }
}

// ─────────────────────────────────────────────────
// Mode: receipt-test
// ─────────────────────────────────────────────────
async function runReceiptTest() {
  console.log('\n--- Receipt Field Verification ---');
  const senderAddr = account.address;

  // Test 1: Single-phase success — phaseStatuses should be [true]
  console.log('\n=== Test 1: Single-phase success ===');
  const nonce1 = await getAaNonce();
  const probeCalldata = encodeFunctionData({ abi: PROBE_ABI, functionName: 'probe' });
  const calls1 = [[[encodeAddress(opts.probeAddr), probeCalldata]]];
  const unsigned1 = baseTxFields(nonce1, calls1);
  const { receipt: r1 } = await signAndSend(unsigned1, { trace: false });

  let pass = true;
  if (r1?.status !== '0x1') { console.log(`FAIL: status=${r1?.status}, expected 0x1`); pass = false; }
  if (r1?.payer?.toLowerCase() !== senderAddr.toLowerCase()) { console.log(`FAIL: payer=${r1?.payer}, expected ${senderAddr}`); pass = false; }
  if (!r1?.phaseStatuses || r1.phaseStatuses.length !== 1 || !r1.phaseStatuses[0]) {
    console.log(`FAIL: phaseStatuses=${JSON.stringify(r1?.phaseStatuses)}, expected [true]`); pass = false;
  }
  if (pass) console.log('PASS: status=1, payer=sender, phaseStatuses=[true]');

  // Test 2: Two-phase mixed — probe succeeds, call to NonceManager reverts.
  // Phase 0: probe() — succeeds. Phase 1: call NonceManager (0xfe INVALID opcode) — reverts.
  // Note: AccountConfig has no code (pure storage), so calls to it succeed silently.
  console.log('\n=== Test 2: Mixed phase results ===');
  const nonce2 = await getAaNonce();
  const invalidCalldata = '0xdeadbeef';
  const calls2 = [
    [[encodeAddress(opts.probeAddr), probeCalldata]],
    [[encodeAddress(NONCE_MANAGER_ADDRESS), invalidCalldata]],
  ];
  const unsigned2 = baseTxFields(nonce2, calls2);
  const { receipt: r2 } = await signAndSend(unsigned2, { trace: false });

  pass = true;
  if (r2?.status !== '0x1') { console.log(`FAIL: status=${r2?.status}, expected 0x1 (any phase succeeded)`); pass = false; }
  if (!r2?.phaseStatuses || r2.phaseStatuses.length !== 2) {
    console.log(`FAIL: phaseStatuses length=${r2?.phaseStatuses?.length}, expected 2`); pass = false;
  } else {
    if (!r2.phaseStatuses[0]) { console.log(`FAIL: phase 0 should be true (probe succeeded)`); pass = false; }
    if (r2.phaseStatuses[1]) { console.log(`FAIL: phase 1 should be false (invalid selector reverts)`); pass = false; }
  }
  if (pass) console.log('PASS: status=1, phaseStatuses=[true, false]');

  // Test 3: Sponsored receipt — verify payer field
  console.log('\n=== Test 3: Sponsored payer field ===');
  const payerAcct = privateKeyToAccount(PAYER_KEY);
  const nonce3 = await getAaNonce();
  const calls3 = [[[encodeAddress(opts.probeAddr), probeCalldata]]];
  const unsigned3 = baseTxFields(nonce3, calls3, [], payerAcct.address);
  const { receipt: r3 } = await signAndSend(unsigned3, { trace: false, payerAccount: payerAcct });

  pass = true;
  if (r3?.status !== '0x1') { console.log(`FAIL: status=${r3?.status}, expected 0x1`); pass = false; }
  if (r3?.payer?.toLowerCase() !== payerAcct.address.toLowerCase()) {
    console.log(`FAIL: payer=${r3?.payer}, expected ${payerAcct.address}`); pass = false;
  }
  if (!r3?.phaseStatuses || r3.phaseStatuses.length !== 1 || !r3.phaseStatuses[0]) {
    console.log(`FAIL: phaseStatuses=${JSON.stringify(r3?.phaseStatuses)}, expected [true]`); pass = false;
  }
  if (pass) console.log('PASS: payer matches sponsored address, phaseStatuses=[true]');

  // Test 4: Empty calls (deploy-like) — phaseStatuses should be []
  console.log('\n=== Test 4: Empty calls receipt ===');
  const nonce4 = await getAaNonce();
  const unsigned4 = baseTxFields(nonce4, []);
  const { receipt: r4 } = await signAndSend(unsigned4, { trace: false });

  pass = true;
  if (!r4?.phaseStatuses || r4.phaseStatuses.length !== 0) {
    console.log(`FAIL: phaseStatuses=${JSON.stringify(r4?.phaseStatuses)}, expected []`); pass = false;
  }
  if (r4?.payer?.toLowerCase() !== senderAddr.toLowerCase()) {
    console.log(`FAIL: payer=${r4?.payer}, expected ${senderAddr}`); pass = false;
  }
  if (pass) console.log('PASS: phaseStatuses=[], payer=sender');

  console.log('\n--- Receipt Verification Complete ---');
}

// ─────────────────────────────────────────────────
// Mode: deploy
// ─────────────────────────────────────────────────
async function runDeploy() {
  console.log('\n--- Account Deployment via EIP-8130 ---');

  const nonce = await getAaNonce();
  console.log(`AA nonce (key=0): ${nonce}`);

  // Derive owner_id for the sender (implicit EOA owner: bytes32(bytes20(sender)))
  // Left-aligned per Solidity: bytes32(bytes20(address)) — right-padded with zeros.
  const ownerId = padHex(account.address.toLowerCase(), { size: 32, dir: 'right' });
  console.log(`Owner ID:        ${ownerId}`);
  console.log(`K1 Verifier:     ${K1_VERIFIER_ADDRESS}`);
  console.log(`AccountConfig:   ${ACCOUNT_CONFIG_ADDRESS}`);
  console.log(`DefaultAccount:  ${DEFAULT_ACCOUNT_ADDR}`);

  // ERC-1167 minimal proxy bytecode pointing to DefaultAccount
  const implAddr = DEFAULT_ACCOUNT_ADDR.slice(2).toLowerCase();
  const erc1167 = `0x363d3d373d3d3d363d73${implAddr}5af43d82803e903d91602b57fd5bf3`;
  console.log(`ERC-1167 proxy:  ${erc1167} (${(erc1167.length - 2) / 2} bytes)`);

  // Random user_salt
  const userSalt = keccak256(concat([
    padHex(account.address, { size: 32, dir: 'left' }),
    padHex(toHex(BigInt(Date.now())), { size: 32, dir: 'left' }),
  ]));
  console.log(`User salt:       ${userSalt}`);

  // Build account_changes RLP:
  //   [[type=0x00, user_salt, bytecode, [[verifier, owner_id, scope]]]]
  const createEntryRlp = [
    '0x',              // type 0x00 (Create)
    userSalt,          // user_salt (32 bytes)
    erc1167,           // bytecode (ERC-1167 proxy)
    [                  // initial_owners list
      [
        encodeAddress(K1_VERIFIER_ADDRESS),
        ownerId,
        '0x',          // scope 0x00 (unrestricted)
      ],
    ],
  ];

  // Compute CREATE2 address for verification:
  // effectiveSalt = keccak256(userSalt || ownersCommitment)
  // ownersCommitment = keccak256(ownerId || verifier || scope)
  const ownersCommitment = keccak256(concat([
    ownerId,
    padHex(K1_VERIFIER_ADDRESS.toLowerCase(), { size: 20, dir: 'left' }),
    toHex(0, { size: 1 }),
  ]));
  const effectiveSalt = keccak256(concat([userSalt, ownersCommitment]));

  // Build deployment code: 14-byte EVM loader header + bytecode.
  // Must match crates/alloy/consensus/src/transaction/eip8130/address.rs
  const bytecodeBytes = erc1167.slice(2);
  const n = bytecodeBytes.length / 2;
  const deployHeader = [
    0x61, (n >> 8) & 0xff, n & 0xff, // PUSH2 len
    0x80,                              // DUP1
    0x60, 0x0e,                        // PUSH1 14  (header size)
    0x60, 0x00,                        // PUSH1 0
    0x39,                              // CODECOPY
    0x60, 0x00,                        // PUSH1 0
    0xf3,                              // RETURN
    0x00, 0x00,                        // padding
  ];
  const deploymentCode = concat([
    toHex(new Uint8Array(deployHeader)),
    erc1167,
  ]);
  const codeHash = keccak256(deploymentCode);

  const create2Addr = `0x${keccak256(concat([
    '0xff',
    padHex(ACCOUNT_CONFIG_ADDRESS, { size: 20, dir: 'left' }),
    effectiveSalt,
    codeHash,
  ])).slice(26)}`;
  console.log(`Predicted addr:  ${create2Addr}`);

  // Empty calls (no execution phases in deploy-only tx)
  const callsRlp = [];

  const unsigned = baseTxFields(nonce, callsRlp, [createEntryRlp]);
  const { receipt } = await signAndSend(unsigned, { trace: opts.trace });

  // Verify deployment
  console.log('\n--- Deployment Verification ---');
  try {
    const code = await client.getCode({ address: create2Addr });
    if (code && code !== '0x') {
      console.log(`SUCCESS: Account deployed at ${create2Addr} (code: ${code.slice(0, 40)}...)`);
    } else {
      console.log(`Account at ${create2Addr} has no code — deployment may have failed`);
    }
  } catch (err) {
    console.log(`Code check error: ${err.shortMessage || err.message}`);
  }

  // Check owner registration
  try {
    const ownerSlot = keccak256(
      concat([
        ownerId,
        keccak256(concat([
          padHex(create2Addr, { size: 32, dir: 'left' }),
          padHex('0x0', { size: 32, dir: 'left' }),
        ])),
      ])
    );
    const ownerConfig = await client.getStorageAt({
      address: ACCOUNT_CONFIG_ADDRESS,
      slot: ownerSlot,
    });
    console.log(`Owner config slot: ${ownerConfig}`);
    if (ownerConfig && ownerConfig !== '0x0000000000000000000000000000000000000000000000000000000000000000') {
      console.log(`SUCCESS: Owner registered for account ${create2Addr}`);
    }
  } catch (err) {
    console.log(`Owner check error: ${err.shortMessage || err.message}`);
  }
}

// ─────────────────────────────────────────────────
// Mode: nonce-rpc
// ─────────────────────────────────────────────────
async function runNonceRpc() {
  console.log('\n--- eth_getTransactionCount(nonceKey) RPC Verification ---');
  const senderAddr = account.address;

  const storageBefore = await getAaNonce();
  const rpcBefore = await getAaNonceViaRpc(senderAddr, 0n);
  console.log(`Before tx — storage nonce: ${storageBefore}, RPC nonce: ${rpcBefore}`);

  let pass = true;
  if (storageBefore !== rpcBefore) {
    console.log(`FAIL: storage (${storageBefore}) != RPC (${rpcBefore}) before tx`);
    pass = false;
  } else {
    console.log('PASS: storage == RPC before tx');
  }

  const probeCalldata = encodeFunctionData({ abi: PROBE_ABI, functionName: 'probe' });
  const callsRlp = [[[encodeAddress(opts.probeAddr), probeCalldata]]];
  const unsigned = baseTxFields(storageBefore, callsRlp);
  const { receipt } = await signAndSend(unsigned, { trace: false });

  if (receipt?.status !== '0x1') {
    console.log(`FAIL: tx status=${receipt?.status}, expected 0x1`);
    pass = false;
  }

  const storageAfter = await getAaNonce();
  const rpcAfter = await getAaNonceViaRpc(senderAddr, 0n);
  console.log(`After tx  — storage nonce: ${storageAfter}, RPC nonce: ${rpcAfter}`);

  if (storageAfter !== rpcAfter) {
    console.log(`FAIL: storage (${storageAfter}) != RPC (${rpcAfter}) after tx`);
    pass = false;
  } else {
    console.log('PASS: storage == RPC after tx');
  }

  if (storageAfter !== storageBefore + 1n) {
    console.log(`FAIL: nonce did not increment (was ${storageBefore}, now ${storageAfter})`);
    pass = false;
  } else {
    console.log('PASS: nonce incremented by 1');
  }

  // Also test non-zero nonce_key returns 0 (unused lane)
  const otherKeyNonce = await getAaNonceViaRpc(senderAddr, 42n);
  if (otherKeyNonce !== 0n) {
    console.log(`FAIL: nonce_key=42 should be 0, got ${otherKeyNonce}`);
    pass = false;
  } else {
    console.log('PASS: nonce_key=42 returns 0 (unused lane)');
  }

  console.log(pass ? '\n--- All nonce-rpc checks PASSED ---' : '\n--- Some nonce-rpc checks FAILED ---');
  if (!pass) process.exit(1);
}

// ─────────────────────────────────────────────────
// Mode: estimate-gas
// ─────────────────────────────────────────────────
async function runEstimateGas() {
  console.log('\n--- eth_estimateGas for EIP-8130 AA Transactions ---');
  const senderAddr = account.address;
  const nonce = await getAaNonce();

  const probeCalldata = encodeFunctionData({ abi: PROBE_ABI, functionName: 'probe' });

  // Build a type 0x7B transaction request with the new AA fields
  const txRequest = {
    type: '0x7b',
    from: senderAddr,
    nonce: numberToHex(nonce),
    nonceKey: '0x0',
    maxFeePerGas: numberToHex(1000000000n),
    maxPriorityFeePerGas: numberToHex(1000000n),
    gas: numberToHex(500000n),
    calls: [[{ to: opts.probeAddr, data: probeCalldata }]],
    accountChanges: [],
    senderAuth: '0x',
    payerAuth: '0x',
  };

  let pass = true;

  // 1. Call eth_estimateGas with the AA request
  let estimated;
  try {
    const result = await client.request({
      method: 'eth_estimateGas',
      params: [txRequest],
    });
    estimated = BigInt(result);
    console.log(`eth_estimateGas returned: ${estimated}`);
    if (estimated > 0n) {
      console.log('PASS: got non-zero gas estimate');
    } else {
      console.log('FAIL: gas estimate is zero');
      pass = false;
    }
  } catch (err) {
    console.log(`FAIL: eth_estimateGas error: ${err.shortMessage || err.message}`);
    if (err.details) console.log(`  Details: ${err.details}`);
    pass = false;
  }

  // 2. Submit the actual transaction and compare gas used
  const callsRlp = [[[encodeAddress(opts.probeAddr), probeCalldata]]];
  const unsigned = baseTxFields(nonce, callsRlp);
  const { receipt } = await signAndSend(unsigned, { trace: false });

  if (receipt?.status !== '0x1') {
    console.log(`FAIL: tx status=${receipt?.status}, expected 0x1`);
    pass = false;
  }

  if (receipt && estimated) {
    const gasUsed = BigInt(receipt.gasUsed);
    console.log(`Actual gas used:     ${gasUsed}`);
    console.log(`Estimated gas:       ${estimated}`);

    if (estimated >= gasUsed) {
      console.log('PASS: estimate >= actual gas used');
    } else {
      console.log('WARN: estimate < actual gas used (underestimate)');
    }

    const ratio = Number(estimated * 100n / gasUsed);
    console.log(`Ratio (estimate/actual): ${ratio}%`);
  }

  // 3. Test eth_call with the AA request
  try {
    const callResult = await client.request({
      method: 'eth_call',
      params: [txRequest, 'latest'],
    });
    console.log(`eth_call returned: ${callResult.slice(0, 66)}...`);
    console.log('PASS: eth_call succeeded');
  } catch (err) {
    console.log(`INFO: eth_call error (may be expected for AA): ${err.shortMessage || err.message}`);
  }

  console.log(pass ? '\n--- All estimate-gas checks PASSED ---' : '\n--- Some estimate-gas checks FAILED ---');
  if (!pass) process.exit(1);
}

// ─────────────────────────────────────────────────
// Mode: custom-verifier (AlwaysValid EVM verifier)
// ─────────────────────────────────────────────────
async function runCustomVerifier() {
  console.log('\n--- Custom EVM Verifier (AlwaysValid) E2E ---');
  console.log(`AlwaysValid verifier: ${ALWAYS_VALID_VERIFIER_ADDRESS}`);
  console.log(`AlwaysValid owner ID: ${ALWAYS_VALID_OWNER_ID}`);

  const SCOPE_SENDER_PAYER = 0x03; // SENDER (0x01) | PAYER (0x02)

  // Step 1: Register AlwaysValid owner with SENDER+PAYER scope via config change
  console.log('\n--- Step 1: Register AlwaysValid owner (scope=SENDER+PAYER) ---');

  const nonce1 = await getAaNonce();
  const seqSlotHash = sequenceSlot(account.address);
  const packedSeq = await client.getStorageAt({ address: ACCOUNT_CONFIG_ADDRESS, slot: seqSlotHash });
  const currentSeq = BigInt(packedSeq || '0x0') & ((1n << 64n) - 1n);

  const operation = {
    changeType: 1,
    verifier: ALWAYS_VALID_VERIFIER_ADDRESS,
    ownerId: ALWAYS_VALID_OWNER_ID,
    scope: SCOPE_SENDER_PAYER,
  };

  const digest = configChangeDigest(account.address, 0n, currentSeq, [operation]);
  const authSig = await account.sign({ hash: digest });
  const authorizerAuth = concat([K1_VERIFIER_ADDRESS, authSig]);

  const configChangeRlp = [
    toHex(0x01, { size: 1 }),
    encodeUint(0n),
    encodeUint(currentSeq),
    [[
      toHex(0x01, { size: 1 }),
      encodeAddress(ALWAYS_VALID_VERIFIER_ADDRESS),
      ALWAYS_VALID_OWNER_ID,
      toHex(SCOPE_SENDER_PAYER, { size: 1 }),
    ]],
    authorizerAuth,
  ];

  const setupCallsRlp = [[[encodeAddress(opts.probeAddr), encodeFunctionData({ abi: PROBE_ABI, functionName: 'probe' })]]];
  const unsigned1 = baseTxFields(nonce1, setupCallsRlp, [configChangeRlp]);
  const { receipt: receipt1 } = await signAndSend(unsigned1, { trace: false });

  const ownerSlotHash = ownerConfigSlot(account.address, ALWAYS_VALID_OWNER_ID);
  const ownerConfig = await client.getStorageAt({ address: ACCOUNT_CONFIG_ADDRESS, slot: ownerSlotHash });
  const verifierHex = '0x' + ownerConfig.slice(-40);

  if (verifierHex.toLowerCase() !== ALWAYS_VALID_VERIFIER_ADDRESS.toLowerCase()) {
    console.log(`FAILED: AlwaysValid verifier not registered (got ${verifierHex})`);
    process.exit(1);
  }
  console.log('SUCCESS: AlwaysValid owner registered with SENDER+PAYER scope');

  // Step 2: Send AA tx using AlwaysValid as sender (type 0x00 custom)
  console.log('\n--- Step 2: Send AA tx with AlwaysValid sender auth ---');

  const nonce2 = await getAaNonce();
  const callsRlp = [[[encodeAddress(opts.probeAddr), encodeFunctionData({ abi: PROBE_ABI, functionName: 'probe' })]]];
  const unsigned2 = baseTxFields(nonce2, callsRlp);

  const customSenderAuth = (_sigHash) => {
    // verifier_address(20) || data (empty for AlwaysValid)
    return ALWAYS_VALID_VERIFIER_ADDRESS;
  };

  const { receipt: receipt2 } = await signAndSend(unsigned2, { trace: opts.trace, customSenderAuth });

  if (receipt2?.status === '0x1') {
    console.log('SUCCESS: AlwaysValid custom-verifier AA tx executed!');
  } else {
    console.log(`FAILED: status ${receipt2?.status || 'unknown'}`);
    process.exit(1);
  }
}

// ─────────────────────────────────────────────────
// Mode: delegate-native (delegate with K1 inner)
// ─────────────────────────────────────────────────
async function runDelegateNative() {
  const delegateAccount = privateKeyToAccount(DELEGATE_KEY);
  const delegateOwnerId = padHex(delegateAccount.address.toLowerCase(), { size: 32, dir: 'right' });

  console.log('\n--- Delegate Verifier (K1 inner) E2E ---');
  console.log(`Sender:    ${account.address}`);
  console.log(`Delegate:  ${delegateAccount.address}`);
  console.log(`Owner ID:  ${delegateOwnerId}`);
  console.log(`Verifier:  ${DELEGATE_VERIFIER_ADDRESS} (Delegate)`);

  // Step 1: Register delegate owner on sender's account
  console.log('\n--- Step 1: Register delegate owner on sender ---');

  const nonce1 = await getAaNonce();
  const seqSlotHash = sequenceSlot(account.address);
  const packedSeq = await client.getStorageAt({ address: ACCOUNT_CONFIG_ADDRESS, slot: seqSlotHash });
  const currentSeq = BigInt(packedSeq || '0x0') & ((1n << 64n) - 1n);

  const operation = {
    changeType: 1,
    verifier: DELEGATE_VERIFIER_ADDRESS,
    ownerId: delegateOwnerId,
    scope: 0, // unrestricted
  };

  const digest = configChangeDigest(account.address, 0n, currentSeq, [operation]);
  const authSig = await account.sign({ hash: digest });
  const authorizerAuth = concat([K1_VERIFIER_ADDRESS, authSig]);

  const configChangeRlp = [
    toHex(0x01, { size: 1 }),
    encodeUint(0n),
    encodeUint(currentSeq),
    [[
      toHex(0x01, { size: 1 }),
      encodeAddress(DELEGATE_VERIFIER_ADDRESS),
      delegateOwnerId,
      '0x',
    ]],
    authorizerAuth,
  ];

  const setupCallsRlp = [[[encodeAddress(opts.probeAddr), encodeFunctionData({ abi: PROBE_ABI, functionName: 'probe' })]]];
  const unsigned1 = baseTxFields(nonce1, setupCallsRlp, [configChangeRlp]);
  const { receipt: receipt1 } = await signAndSend(unsigned1, { trace: false });

  const ownerSlotHash = ownerConfigSlot(account.address, delegateOwnerId);
  const ownerConfig = await client.getStorageAt({ address: ACCOUNT_CONFIG_ADDRESS, slot: ownerSlotHash });
  const verifierHex = '0x' + ownerConfig.slice(-40);

  if (verifierHex.toLowerCase() !== DELEGATE_VERIFIER_ADDRESS.toLowerCase()) {
    console.log(`FAILED: Delegate verifier not registered (got ${verifierHex})`);
    process.exit(1);
  }
  console.log('SUCCESS: Delegate owner registered on sender account');

  // Step 2: Send AA tx where delegate signs with K1 (type 0x04 || 0x01 || K1 sig)
  console.log('\n--- Step 2: Send AA tx with delegate K1 auth ---');

  const nonce2 = await getAaNonce();
  const callsRlp = [[[encodeAddress(opts.probeAddr), encodeFunctionData({ abi: PROBE_ABI, functionName: 'probe' })]]];
  const unsigned2 = baseTxFields(nonce2, callsRlp);

  const delegateSenderAuth = (sigHash) => {
    const sig = delegateAccount.sign({ hash: sigHash });
    // DELEGATE_VERIFIER(20) || K1_VERIFIER(20) || K1 signature
    return sig.then(s => concat([DELEGATE_VERIFIER_ADDRESS, K1_VERIFIER_ADDRESS, s]));
  };

  // signAndSend expects sync or we handle async in customSenderAuth
  const signingPayload = concat([toHex(AA_TX_TYPE, { size: 1 }), toRlp(unsigned2)]);
  const sigHash = keccak256(signingPayload);
  const delegateSig = await delegateAccount.sign({ hash: sigHash });
  const senderAuth = concat([DELEGATE_VERIFIER_ADDRESS, K1_VERIFIER_ADDRESS, delegateSig]);

  const { receipt: receipt2 } = await signAndSend(unsigned2, {
    trace: opts.trace,
    customSenderAuth: () => senderAuth,
  });

  if (receipt2?.status === '0x1') {
    console.log('SUCCESS: Delegate-native (K1 inner) AA tx executed!');
  } else {
    console.log(`FAILED: status ${receipt2?.status || 'unknown'}`);
    process.exit(1);
  }
}

// ─────────────────────────────────────────────────
// Mode: delegate-p256 (delegate with P256 inner)
// ─────────────────────────────────────────────────
async function runDelegateP256() {
  console.log('\n--- Delegate Verifier (P256 inner) E2E ---');

  const p256PrivateKey = p256curve.utils.randomPrivateKey();
  const p256PubUncompressed = p256curve.getPublicKey(p256PrivateKey, false);
  const p256PubRaw = p256PubUncompressed.slice(1);
  const p256OwnerId = keccak256(toHex(p256PubRaw));

  console.log(`Sender:     ${account.address}`);
  console.log(`P256 owner: ${p256OwnerId}`);
  console.log(`P256 verifier:    ${P256_VERIFIER_ADDRESS}`);
  console.log(`Delegate verifier: ${DELEGATE_VERIFIER_ADDRESS}`);

  // Step 1: Register P256 owner on the sender's account (via config change)
  // so the delegate inner verification can resolve the P256 owner_id.
  console.log('\n--- Step 1: Register P256 owner on sender (for inner resolution) ---');

  const nonce1 = await getAaNonce();
  const seqSlotHash = sequenceSlot(account.address);
  const packedSeq1 = await client.getStorageAt({ address: ACCOUNT_CONFIG_ADDRESS, slot: seqSlotHash });
  const seq1 = BigInt(packedSeq1 || '0x0') & ((1n << 64n) - 1n);

  const p256Op = {
    changeType: 1,
    verifier: DELEGATE_VERIFIER_ADDRESS,
    ownerId: p256OwnerId,
    scope: 0, // unrestricted
  };

  const digest1 = configChangeDigest(account.address, 0n, seq1, [p256Op]);
  const authSig1 = await account.sign({ hash: digest1 });
  const authorizerAuth1 = concat([K1_VERIFIER_ADDRESS, authSig1]);

  const configChangeRlp1 = [
    toHex(0x01, { size: 1 }),
    encodeUint(0n),
    encodeUint(seq1),
    [[
      toHex(0x01, { size: 1 }),
      encodeAddress(DELEGATE_VERIFIER_ADDRESS),
      p256OwnerId,
      '0x',
    ]],
    authorizerAuth1,
  ];

  const setupCallsRlp = [[[encodeAddress(opts.probeAddr), encodeFunctionData({ abi: PROBE_ABI, functionName: 'probe' })]]];
  const unsigned1 = baseTxFields(nonce1, setupCallsRlp, [configChangeRlp1]);
  const { receipt: receipt1 } = await signAndSend(unsigned1, { trace: false });

  const ownerSlotHash = ownerConfigSlot(account.address, p256OwnerId);
  const ownerConfig = await client.getStorageAt({ address: ACCOUNT_CONFIG_ADDRESS, slot: ownerSlotHash });
  const verifierHex = '0x' + ownerConfig.slice(-40);

  if (verifierHex.toLowerCase() !== DELEGATE_VERIFIER_ADDRESS.toLowerCase()) {
    console.log(`FAILED: Delegate+P256 owner not registered (got ${verifierHex})`);
    process.exit(1);
  }
  console.log('SUCCESS: P256 owner registered with DelegateVerifier on sender');

  // Step 2: Send AA tx with delegate + P256 inner auth
  // Auth: 0x04 (delegate) || 0x02 (P256 inner) || pubkey(64) || sig(64)
  console.log('\n--- Step 2: Send AA tx with delegate P256 auth ---');

  const nonce2 = await getAaNonce();
  const callsRlp = [[[encodeAddress(opts.probeAddr), encodeFunctionData({ abi: PROBE_ABI, functionName: 'probe' })]]];
  const unsigned2 = baseTxFields(nonce2, callsRlp);

  const signingPayload = concat([toHex(AA_TX_TYPE, { size: 1 }), toRlp(unsigned2)]);
  const sigHash = keccak256(signingPayload);
  const hashArr = new Uint8Array(sigHash.slice(2).match(/.{2}/g).map(b => parseInt(b, 16)));
  const sig = p256curve.sign(hashArr, p256PrivateKey, { lowS: true });
  const rBytes = sig.r.toString(16).padStart(64, '0');
  const sBytes = sig.s.toString(16).padStart(64, '0');

  const senderAuth = concat([
    DELEGATE_VERIFIER_ADDRESS,  // delegate verifier (20 bytes)
    P256_VERIFIER_ADDRESS,      // P256 inner verifier (20 bytes)
    toHex(p256PubRaw),          // P256 public key (64 bytes)
    '0x' + rBytes + sBytes,     // P256 signature (64 bytes)
  ]);

  const { receipt: receipt2 } = await signAndSend(unsigned2, {
    trace: opts.trace,
    customSenderAuth: () => senderAuth,
  });

  if (receipt2?.status === '0x1') {
    console.log('SUCCESS: Delegate-P256 (P256 inner via native delegation) AA tx executed!');
  } else {
    console.log(`FAILED: status ${receipt2?.status || 'unknown'}`);
    process.exit(1);
  }
}

// ─────────────────────────────────────────────────
// Mode: owner-change-signing
// ─────────────────────────────────────────────────
async function runOwnerChangeSigning() {
  const ZERO_ADDRESS = '0x0000000000000000000000000000000000000000';
  const REVOKED_VERIFIER_ADDRESS = '0x0000000000000000000000000000000000000001';
  const AUTHORIZE_OWNER = 1;
  const REVOKE_OWNER = 2;

  const eoaOwnerId = padHex(account.address.toLowerCase(), { size: 32, dir: 'right' });
  // Keep this deterministic so repeated runs can always clean up prior state.
  const p256PrivateKey = createHash('sha256')
    .update('owner-change-signing-fixed-key')
    .digest();
  const p256PubUncompressed = p256curve.getPublicKey(p256PrivateKey, false);
  const p256PubRaw = p256PubUncompressed.slice(1);
  const p256OwnerId = keccak256(toHex(p256PubRaw));

  console.log('\n--- Owner Change Signing Test (revoke EOA + add P256) ---');
  console.log(`Sender: ${account.address}`);
  console.log(`EOA owner_id:  ${eoaOwnerId}`);
  console.log(`P256 owner_id: ${p256OwnerId}`);
  console.log('This mode restores K1 ownership at the end.');

  const probeCalldata = encodeFunctionData({ abi: PROBE_ABI, functionName: 'probe' });
  const callsRlp = [[[encodeAddress(opts.probeAddr), probeCalldata]]];
  const seqSlotHash = sequenceSlot(account.address);

  const readSequence = async () => {
    const packedSeq = await client.getStorageAt({
      address: ACCOUNT_CONFIG_ADDRESS,
      slot: seqSlotHash,
    });
    return BigInt(packedSeq || '0x0') & ((1n << 64n) - 1n);
  };

  const toVerifierAddress = (storageWord) => {
    if (!storageWord || storageWord === '0x') return ZERO_ADDRESS;
    const clean = storageWord.slice(2).padStart(64, '0');
    return `0x${clean.slice(-40)}`;
  };

  const aaSigHash = (unsignedRlpFields) =>
    keccak256(concat([toHex(AA_TX_TYPE, { size: 1 }), toRlp(unsignedRlpFields)]));

  const encodeSignedTx = (unsignedRlpFields, senderAuth, payerAuth = '0x') =>
    concat([toHex(AA_TX_TYPE, { size: 1 }), toRlp([...unsignedRlpFields, senderAuth, payerAuth])]);

  const k1AuthForHash = async (hash) => {
    const sig = await account.sign({ hash });
    return concat([K1_VERIFIER_ADDRESS, sig]);
  };

  const p256AuthForHash = (hash) => {
    const hashArr = new Uint8Array(hash.slice(2).match(/.{2}/g).map((b) => parseInt(b, 16)));
    const sig = p256curve.sign(hashArr, p256PrivateKey, { lowS: true });
    const rBytes = sig.r.toString(16).padStart(64, '0');
    const sBytes = sig.s.toString(16).padStart(64, '0');
    return concat([
      P256_VERIFIER_ADDRESS,
      toHex(p256PubRaw),
      `0x${rBytes}${sBytes}`,
    ]);
  };

  const ownerChangeToRlp = (change) => [
    toHex(change.changeType, { size: 1 }),
    encodeAddress(change.verifier),
    change.ownerId,
    '0x',
  ];

  const buildConfigChangeRlp = async (ownerChanges, authorizerAuthForDigest) => {
    const seq = await readSequence();
    const digest = configChangeDigest(account.address, 0n, seq, ownerChanges);
    const authorizerAuth = await Promise.resolve(authorizerAuthForDigest(digest));
    return [
      toHex(0x01, { size: 1 }),
      encodeUint(0n),
      encodeUint(seq),
      ownerChanges.map(ownerChangeToRlp),
      authorizerAuth,
    ];
  };

  const waitForReceipt = async (txHash, attempts = 12, delayMs = 1000) => {
    for (let i = 0; i < attempts; i++) {
      const receipt = await client.request({
        method: 'eth_getTransactionReceipt',
        params: [txHash],
      });
      if (receipt) return receipt;
      await new Promise((r) => setTimeout(r, delayMs));
    }
    return null;
  };

  const rotationChanges = [
    { changeType: AUTHORIZE_OWNER, verifier: P256_VERIFIER_ADDRESS, ownerId: p256OwnerId, scope: 0 },
    { changeType: REVOKE_OWNER, verifier: ZERO_ADDRESS, ownerId: eoaOwnerId, scope: 0 },
  ];
  const p256Slot = ownerConfigSlot(account.address, p256OwnerId);
  const eoaSlot = ownerConfigSlot(account.address, eoaOwnerId);
  const readOwnerVerifiers = async () => {
    const [p256Config, eoaConfig] = await Promise.all([
      client.getStorageAt({ address: ACCOUNT_CONFIG_ADDRESS, slot: p256Slot }),
      client.getStorageAt({ address: ACCOUNT_CONFIG_ADDRESS, slot: eoaSlot }),
    ]);
    return {
      p256Config,
      eoaConfig,
      p256Verifier: toVerifierAddress(p256Config).toLowerCase(),
      eoaVerifier: toVerifierAddress(eoaConfig).toLowerCase(),
    };
  };
  const isRotationState = ({ p256Verifier, eoaVerifier }) =>
    p256Verifier === P256_VERIFIER_ADDRESS.toLowerCase() &&
    eoaVerifier === REVOKED_VERIFIER_ADDRESS.toLowerCase();
  const isRestoredState = ({ p256Verifier, eoaVerifier }) =>
    p256Verifier === ZERO_ADDRESS.toLowerCase() &&
    eoaVerifier === K1_VERIFIER_ADDRESS.toLowerCase();
  const toErrorMessage = (err) => err?.details || err?.shortMessage || err?.message || String(err);

  let hadFailure = false;
  let rotationApplied = false;
  let revokedSignerRejected = false;
  const initialState = await readOwnerVerifiers();
  const trySignAndSend = async (unsignedFields, sendOpts, failurePrefix) => {
    try {
      return await signAndSend(unsignedFields, { ...sendOpts, exitOnError: false });
    } catch (err) {
      console.log(`FAILED: ${failurePrefix}: ${toErrorMessage(err)}`);
      hadFailure = true;
      return null;
    }
  };

  // Step 1: revoked signer should fail (submission reject or on-chain revert).
  console.log('\n--- Step 1: Tx signed by soon-to-be-revoked EOA should fail ---');
  const step1NonceKey = 1n;
  const nonce1 = await getAaNonceViaRpc(account.address, step1NonceKey);
  const blockBeforeStep1 = await client.getBlockNumber();
  console.log(`AA nonce (key=${step1NonceKey}): ${nonce1}`);
  const configChangeRlp1 = await buildConfigChangeRlp(rotationChanges, k1AuthForHash);
  const unsigned1 = baseTxFields(nonce1, callsRlp, [configChangeRlp1], ZERO_ADDRESS, { nonceKey: step1NonceKey });
  const senderAuth1 = await k1AuthForHash(aaSigHash(unsigned1));
  const encodedTx1 = encodeSignedTx(unsigned1, senderAuth1);

  try {
    const txHash1 = await client.request({
      method: 'eth_sendRawTransaction',
      params: [encodedTx1],
    });
    console.log(`Tx accepted for propagation (${txHash1}); waiting for receipt...`);
    const receipt1 = await waitForReceipt(txHash1);
    if (!receipt1) {
      const blockAfterStep1 = await client.getBlockNumber();
      const stateAfterNoReceipt = await readOwnerVerifiers();
      const stateUnchanged =
        stateAfterNoReceipt.p256Config === initialState.p256Config &&
        stateAfterNoReceipt.eoaConfig === initialState.eoaConfig;
      if (blockAfterStep1 > blockBeforeStep1 && stateUnchanged) {
        console.log('SUCCESS: Revoked-signer tx accepted by txpool but stayed unmined (invalid at inclusion)');
        revokedSignerRejected = true;
      } else {
        console.log('FAILED: Revoked-signer tx had no receipt and owner state changed unexpectedly');
        hadFailure = true;
      }
    } else if (receipt1.status === '0x1') {
      console.log('FAILED: Revoked-signer tx succeeded unexpectedly');
      hadFailure = true;
      rotationApplied = true;
    } else {
      console.log('SUCCESS: Revoked-signer tx reverted on-chain as expected');
      revokedSignerRejected = true;
    }
  } catch (err) {
    console.log(`SUCCESS: Revoked-signer tx rejected at submission: ${toErrorMessage(err)}`);
    revokedSignerRejected = true;
  }

  // Step 2: newly-added P256 signer should pass in the same tx that adds it.
  if (revokedSignerRejected) {
    console.log('\n--- Step 2: Tx signed by newly-added P256 owner should pass ---');
    const nonce2 = await getAaNonce();
    console.log(`AA nonce (key=0): ${nonce2}`);
    const configChangeRlp2 = await buildConfigChangeRlp(rotationChanges, k1AuthForHash);
    const unsigned2 = baseTxFields(nonce2, callsRlp, [configChangeRlp2]);
    const sent2 = await trySignAndSend(
      unsigned2,
      { trace: opts.trace, customSenderAuth: p256AuthForHash },
      'Newly-added P256 signer tx rejected',
    );
    const receipt2 = sent2?.receipt ?? null;

    if (receipt2) {
      if (receipt2.status !== '0x1') {
        console.log(`FAILED: Newly-added P256 signer tx failed (status ${receipt2.status || 'unknown'})`);
        hadFailure = true;
      } else {
        console.log('SUCCESS: Tx signed by owner added in owner_changes executed');
      }
    }

    const stateAfterRotation = await readOwnerVerifiers();

    if (stateAfterRotation.p256Verifier !== P256_VERIFIER_ADDRESS.toLowerCase()) {
      console.log(`FAILED: P256 owner verifier mismatch (got ${stateAfterRotation.p256Verifier})`);
      hadFailure = true;
    }
    if (stateAfterRotation.eoaVerifier !== REVOKED_VERIFIER_ADDRESS.toLowerCase()) {
      console.log(`FAILED: EOA owner not revoked (got ${stateAfterRotation.eoaVerifier})`);
      hadFailure = true;
    }
    rotationApplied = isRotationState(stateAfterRotation);
    if (rotationApplied) {
      console.log('SUCCESS: Post-state matches expected rotation (P256 added, EOA revoked)');
    }
  } else {
    console.log('\n--- Step 2: Skipped ---');
    console.log('Skipped because step 1 did not confirm revoked-signer rejection.');
  }

  // Step 3: cleanup to restore original sender for subsequent script modes.
  if (rotationApplied) {
    console.log('\n--- Step 3: Cleanup (restore EOA, remove temporary P256 owner) ---');
    const restoreChanges = [
      { changeType: AUTHORIZE_OWNER, verifier: K1_VERIFIER_ADDRESS, ownerId: eoaOwnerId, scope: 0 },
      { changeType: REVOKE_OWNER, verifier: ZERO_ADDRESS, ownerId: p256OwnerId, scope: 0 },
    ];
    const nonce3 = await getAaNonce();
    console.log(`AA nonce (key=0): ${nonce3}`);
    const configChangeRlp3 = await buildConfigChangeRlp(restoreChanges, p256AuthForHash);
    const unsigned3 = baseTxFields(nonce3, callsRlp, [configChangeRlp3]);
    const sent3 = await trySignAndSend(
      unsigned3,
      { trace: false, customSenderAuth: k1AuthForHash },
      'Cleanup tx rejected',
    );
    const receipt3 = sent3?.receipt ?? null;

    if (receipt3 && receipt3.status !== '0x1') {
      console.log(`FAILED: Cleanup tx failed (status ${receipt3.status || 'unknown'})`);
      hadFailure = true;
    }

    const stateAfterCleanup = await readOwnerVerifiers();

    if (stateAfterCleanup.eoaVerifier !== K1_VERIFIER_ADDRESS.toLowerCase()) {
      console.log(`FAILED: Cleanup did not restore EOA owner (got ${stateAfterCleanup.eoaVerifier})`);
      hadFailure = true;
    }
    if (stateAfterCleanup.p256Verifier !== ZERO_ADDRESS.toLowerCase()) {
      console.log(`FAILED: Cleanup did not remove temporary P256 owner (got ${stateAfterCleanup.p256Verifier})`);
      hadFailure = true;
    }

    if (isRestoredState(stateAfterCleanup)) {
      console.log('SUCCESS: Cleanup restored original K1 owner configuration');
    }
  } else {
    console.log('\n--- Step 3: Cleanup skipped ---');
    console.log('No owner rotation was applied, so no cleanup was required.');
  }

  if (hadFailure) {
    process.exit(1);
  }
}

// ─────────────────────────────────────────────────
// Mode: locked-config
// ─────────────────────────────────────────────────
async function runLockedConfig() {
  const LOCK_ABI = [
    { type: 'function', name: 'lock', inputs: [{ type: 'uint16', name: 'unlockDelay' }], outputs: [], stateMutability: 'nonpayable' },
  ];

  console.log('\n--- Locked Config Test: Verify locked accounts reject config changes ---');
  console.log(`Sender: ${account.address}`);

  // Step 1: Lock the account via a call to AccountConfiguration.lock(600)
  console.log('\n--- Step 1: Lock the account ---');

  const lockCalldata = encodeFunctionData({ abi: LOCK_ABI, functionName: 'lock', args: [600] });
  const nonce1 = await getAaNonce();
  console.log(`AA nonce (key=0): ${nonce1}`);

  const lockCalls = [[[encodeAddress(ACCOUNT_CONFIG_ADDRESS), lockCalldata]]];
  const unsigned1 = baseTxFields(nonce1, lockCalls);
  const { receipt: receipt1 } = await signAndSend(unsigned1, { trace: opts.trace });

  if (receipt1?.status !== '0x1') {
    console.log(`FAILED: Lock tx failed (status ${receipt1?.status || 'unknown'})`);
    process.exit(1);
  }
  console.log('Account locked successfully');

  // Verify lock state in storage: slot = keccak256(pad(account,32) || pad(1,32))
  const lockSlotHash = keccak256(concat([
    padHex(account.address.toLowerCase(), { size: 32, dir: 'left' }),
    padHex('0x1', { size: 32, dir: 'left' }),
  ]));
  const lockValue = await client.getStorageAt({
    address: ACCOUNT_CONFIG_ADDRESS,
    slot: lockSlotHash,
  });
  console.log(`Lock storage: ${lockValue}`);

  // Step 2: Try to send a config change while locked — expect rejection
  console.log('\n--- Step 2: Attempt config change while locked (should fail) ---');

  const newOwnerId = padHex('0xdeadbeef', { size: 32, dir: 'right' });
  const nonce2 = await getAaNonce();

  const seqSlotHash = sequenceSlot(account.address);
  const packedSeq = await client.getStorageAt({ address: ACCOUNT_CONFIG_ADDRESS, slot: seqSlotHash });
  const currentSeq = BigInt(packedSeq || '0x0') & ((1n << 64n) - 1n);

  const operation = { changeType: 1, verifier: K1_VERIFIER_ADDRESS, ownerId: newOwnerId, scope: 0 };
  const digest = configChangeDigest(account.address, 0n, currentSeq, [operation]);
  const authSig = await account.sign({ hash: digest });
  const authorizerAuth = concat([K1_VERIFIER_ADDRESS, authSig]);

  const configChangeRlp = [
    toHex(0x01, { size: 1 }),
    encodeUint(0n),
    encodeUint(currentSeq),
    [[toHex(0x01, { size: 1 }), encodeAddress(K1_VERIFIER_ADDRESS), newOwnerId, '0x']],
    authorizerAuth,
  ];

  const probeCalldata = encodeFunctionData({ abi: PROBE_ABI, functionName: 'probe' });
  const callsRlp2 = [[[encodeAddress(opts.probeAddr), probeCalldata]]];
  const unsigned2 = baseTxFields(nonce2, callsRlp2, [configChangeRlp]);

  // Sign manually — we expect submission to fail
  const sigPayload = concat([toHex(AA_TX_TYPE, { size: 1 }), toRlp(unsigned2)]);
  const sigHash = keccak256(sigPayload);
  const senderSig = await account.sign({ hash: sigHash });
  const senderAuth = concat([K1_VERIFIER_ADDRESS, senderSig]);
  const encodedTx = concat([
    toHex(AA_TX_TYPE, { size: 1 }),
    toRlp([...unsigned2, senderAuth, '0x']),
  ]);

  try {
    const txHash2 = await client.request({
      method: 'eth_sendRawTransaction',
      params: [encodedTx],
    });
    console.log(`TX was accepted (hash: ${txHash2}), checking receipt...`);
    await new Promise(r => setTimeout(r, 5000));
    const receipt2 = await client.request({
      method: 'eth_getTransactionReceipt',
      params: [txHash2],
    });
    if (!receipt2 || receipt2.status === '0x0') {
      console.log('SUCCESS: Config change reverted on-chain (account is locked)');
    } else {
      console.log('FAILED: Config change succeeded despite account being locked!');
      process.exit(1);
    }
  } catch (err) {
    const msg = err.details || err.shortMessage || err.message;
    console.log(`SUCCESS: Config change rejected while account locked: ${msg}`);
  }
}

// ─────────────────────────────────────────────────
// Mode: nonceless
// ─────────────────────────────────────────────────
async function runNonceless() {
  const NONCE_KEY_MAX = (1n << 256n) - 1n;

  console.log('\n--- Nonceless Transaction Test (NONCE_KEY_MAX) ---');
  console.log(`Sender: ${account.address}`);
  console.log(`NONCE_KEY_MAX: ${NONCE_KEY_MAX}`);

  // Compute expiry relative to latest block timestamp
  const block = await client.getBlock();
  const blockTimestamp = Number(block.timestamp);
  const expiry = BigInt(blockTimestamp + 20);
  console.log(`Block timestamp: ${blockTimestamp}`);
  console.log(`Expiry: ${expiry} (${Number(expiry) - blockTimestamp}s from now)`);

  const probeCalldata = encodeFunctionData({ abi: PROBE_ABI, functionName: 'probe' });
  const callsRlp = [[[encodeAddress(opts.probeAddr), probeCalldata]]];

  const unsigned = baseTxFields(0n, callsRlp, [], '0x0000000000000000000000000000000000000000', {
    nonceKey: NONCE_KEY_MAX,
    expiry,
  });

  // Step 1: Send nonceless AA tx
  console.log('\n--- Step 1: Send nonceless AA tx ---');
  const { receipt, nodeTxHash } = await signAndSend(unsigned, { trace: opts.trace });

  if (receipt?.status !== '0x1') {
    console.log(`FAILED: Nonceless tx failed (status ${receipt?.status || 'unknown'})`);
    process.exit(1);
  }
  console.log(`Nonceless tx landed! Hash: ${nodeTxHash}`);

  // Step 2: Resubmit the exact same signed transaction (same hash)
  console.log('\n--- Step 2: Resubmit same nonceless tx (should be rejected) ---');

  const sigPayload = concat([toHex(AA_TX_TYPE, { size: 1 }), toRlp(unsigned)]);
  const sigHash = keccak256(sigPayload);
  const sig = await account.sign({ hash: sigHash });
  const senderAuth = concat([K1_VERIFIER_ADDRESS, sig]);
  const encodedTx = concat([
    toHex(AA_TX_TYPE, { size: 1 }),
    toRlp([...unsigned, senderAuth, '0x']),
  ]);
  const dupTxHash = keccak256(encodedTx);
  console.log(`Duplicate TX hash: ${dupTxHash}`);

  try {
    await client.request({
      method: 'eth_sendRawTransaction',
      params: [encodedTx],
    });
    console.log('WARNING: Duplicate tx was accepted by RPC, checking if it lands...');
    await new Promise(r => setTimeout(r, 5000));
    const receipt2 = await client.request({
      method: 'eth_getTransactionReceipt',
      params: [dupTxHash],
    });
    if (!receipt2 || receipt2.status === '0x0') {
      console.log('SUCCESS: Duplicate nonceless tx did not land');
    } else {
      console.log('FAILED: Duplicate nonceless tx actually landed twice!');
      process.exit(1);
    }
  } catch (err) {
    const msg = err.details || err.shortMessage || err.message;
    console.log(`SUCCESS: Duplicate nonceless tx rejected: ${msg}`);
  }
}

// ─────────────────────────────────────────────────
// Mode: delegation
// ─────────────────────────────────────────────────
async function runDelegation() {
  console.log('\n--- Delegation Entry Test (account_changes type 0x02) ---');
  console.log(`Sender:          ${account.address}`);
  console.log(`Default account: ${DEFAULT_ACCOUNT_ADDR}`);
  console.log(`Account config:  ${ACCOUNT_CONFIG_ADDRESS}`);

  // Step 1: Verify current code is auto-delegation to DEFAULT_ACCOUNT
  const codeBefore = await client.getCode({ address: account.address });
  console.log(`\nCode before: ${codeBefore}`);
  const expectedDefault = ('0xef0100' + DEFAULT_ACCOUNT_ADDR.slice(2)).toLowerCase();
  if (codeBefore?.toLowerCase() === expectedDefault) {
    console.log('Current delegation: DEFAULT_ACCOUNT (as expected)');
  } else if (codeBefore && codeBefore !== '0x') {
    console.log(`Current code: ${codeBefore.slice(0, 50)}...`);
  } else {
    console.log('No code on sender (bare EOA), auto-delegation will fire first');
  }

  // Step 2: Send AA tx with delegation entry targeting ACCOUNT_CONFIG_ADDRESS
  console.log('\n--- Step 2: Delegate to ACCOUNT_CONFIG_ADDRESS ---');
  const nonce1 = await getAaNonce();
  console.log(`AA nonce (key=0): ${nonce1}`);

  const probeCalldata = encodeFunctionData({ abi: PROBE_ABI, functionName: 'probe' });
  const callsRlp1 = [[[encodeAddress(opts.probeAddr), probeCalldata]]];

  const delegationEntry1 = [
    toHex(0x02, { size: 1 }),
    encodeAddress(ACCOUNT_CONFIG_ADDRESS),
  ];
  const unsigned1 = baseTxFields(nonce1, callsRlp1, [delegationEntry1]);
  const { receipt: receipt1 } = await signAndSend(unsigned1, { trace: opts.trace });

  if (receipt1?.status !== '0x1') {
    console.log(`FAILED: Delegation tx failed (status ${receipt1?.status || 'unknown'})`);
    process.exit(1);
  }

  const codeAfter1 = await client.getCode({ address: account.address });
  const expectedConfig = ('0xef0100' + ACCOUNT_CONFIG_ADDRESS.slice(2)).toLowerCase();
  console.log(`Code after delegation: ${codeAfter1}`);
  if (codeAfter1?.toLowerCase() === expectedConfig) {
    console.log('SUCCESS: Delegation changed to ACCOUNT_CONFIG_ADDRESS');
  } else {
    console.log(`FAILED: Expected ${expectedConfig}, got ${codeAfter1}`);
    process.exit(1);
  }

  // Step 3: Restore delegation back to DEFAULT_ACCOUNT
  console.log('\n--- Step 3: Restore delegation to DEFAULT_ACCOUNT ---');
  const nonce2 = await getAaNonce();
  console.log(`AA nonce (key=0): ${nonce2}`);

  const callsRlp2 = [[[encodeAddress(opts.probeAddr), probeCalldata]]];
  const delegationEntry2 = [
    toHex(0x02, { size: 1 }),
    encodeAddress(DEFAULT_ACCOUNT_ADDR),
  ];
  const unsigned2 = baseTxFields(nonce2, callsRlp2, [delegationEntry2]);
  const { receipt: receipt2 } = await signAndSend(unsigned2, { trace: opts.trace });

  if (receipt2?.status !== '0x1') {
    console.log(`FAILED: Restore delegation tx failed (status ${receipt2?.status || 'unknown'})`);
    process.exit(1);
  }

  const codeAfter2 = await client.getCode({ address: account.address });
  if (codeAfter2?.toLowerCase() === expectedDefault) {
    console.log('SUCCESS: Delegation restored to DEFAULT_ACCOUNT');
  } else {
    console.log(`FAILED: Expected ${expectedDefault}, got ${codeAfter2}`);
    process.exit(1);
  }

  // Step 4: Clear delegation (target = address(0))
  console.log('\n--- Step 4: Clear delegation (target = address(0)) ---');
  const nonce3 = await getAaNonce();
  console.log(`AA nonce (key=0): ${nonce3}`);

  const callsRlp3 = [[[encodeAddress(opts.probeAddr), probeCalldata]]];
  const delegationEntry3 = [
    toHex(0x02, { size: 1 }),
    encodeAddress('0x0000000000000000000000000000000000000000'),
  ];
  const unsigned3 = baseTxFields(nonce3, callsRlp3, [delegationEntry3]);
  const { receipt: receipt3 } = await signAndSend(unsigned3, { trace: opts.trace });

  if (receipt3?.status !== '0x1') {
    console.log(`FAILED: Clear delegation tx failed (status ${receipt3?.status || 'unknown'})`);
    process.exit(1);
  }

  const codeAfter3 = await client.getCode({ address: account.address });
  if (!codeAfter3 || codeAfter3 === '0x') {
    console.log('SUCCESS: Delegation cleared — account is bare EOA again');
  } else {
    console.log(`FAILED: Expected empty code, got ${codeAfter3}`);
    process.exit(1);
  }

  // Step 5: Send a normal tx — auto-delegation should fire, restoring DEFAULT_ACCOUNT
  console.log('\n--- Step 5: Normal tx to verify auto-delegation restores ---');
  const nonce4 = await getAaNonce();
  console.log(`AA nonce (key=0): ${nonce4}`);

  const callsRlp4 = [[[encodeAddress(opts.probeAddr), probeCalldata]]];
  const unsigned4 = baseTxFields(nonce4, callsRlp4);
  const { receipt: receipt4 } = await signAndSend(unsigned4, { trace: opts.trace });

  if (receipt4?.status !== '0x1') {
    console.log(`FAILED: Auto-delegation restore tx failed (status ${receipt4?.status || 'unknown'})`);
    process.exit(1);
  }

  const codeAfter4 = await client.getCode({ address: account.address });
  if (codeAfter4?.toLowerCase() === expectedDefault) {
    console.log('SUCCESS: Auto-delegation restored DEFAULT_ACCOUNT after clear');
  } else {
    console.log(`FAILED: Expected ${expectedDefault}, got ${codeAfter4}`);
    process.exit(1);
  }

  console.log('\n--- All delegation tests passed! ---');
}

// ─────────────────────────────────────────────────
// Main
// ─────────────────────────────────────────────────
const blockNum = await client.getBlockNumber();
console.log(`Current block: ${blockNum}`);

const balance = await client.getBalance({ address: account.address });
console.log(`Sender balance: ${balance} wei (${Number(balance) / 1e18} ETH)\n`);

switch (opts.mode) {
  case 'probe':
    await runProbe();
    break;
  case 'multi-call':
    await runMultiCall();
    break;
  case 'sponsor':
    await runSponsor();
    break;
  case 'config-change':
    await runConfigChange();
    break;
  case 'p256':
    await runP256();
    break;
  case 'webauthn':
    await runWebAuthn();
    break;
  case 'receipt-test':
    await runReceiptTest();
    break;
  case 'deploy':
    await runDeploy();
    break;
  case 'nonce-rpc':
    await runNonceRpc();
    break;
  case 'estimate-gas':
    await runEstimateGas();
    break;
  case 'custom-verifier':
    await runCustomVerifier();
    break;
  case 'delegate-native':
    await runDelegateNative();
    break;
  case 'delegate-p256':
    await runDelegateP256();
    break;
  case 'owner-change-signing':
    await runOwnerChangeSigning();
    break;
  case 'nonceless':
    await runNonceless();
    break;
  case 'delegation':
    await runDelegation();
    break;
  case 'locked-config':
    await runLockedConfig();
    break;
  default:
    console.error(`Unknown mode: ${opts.mode}`);
    console.error('Available modes: probe, multi-call, sponsor, config-change, p256, webauthn, receipt-test, deploy, nonce-rpc, estimate-gas, custom-verifier, delegate-native, delegate-p256, owner-change-signing, nonceless, delegation, locked-config');
    process.exit(1);
}
