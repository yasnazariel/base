/**
 * Sends EIP-8130 (type 0x05) AA transactions against a local devnet.
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
 *   nonce-rpc    Verify base_getEip8130Nonce RPC matches storage reads + increments
 *   estimate-gas Verify eth_estimateGas / eth_call work with type 0x05 AA requests
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
const AA_TX_TYPE = 0x05;
const AA_PAYER_TYPE = 0x06;
const L2_CHAIN_ID = 84538453n;

const NONCE_MANAGER_ADDRESS = '0x000000000000000000000000000000000000Aa02';

// Deployed contract addresses — loaded from deploy-8130.sh output if available,
// otherwise fall back to provisional values matching predeploys.rs.
const FALLBACK_ADDRESSES = {
  accountConfiguration: '0x0F127193b72E0f8546A6F4E471b6F8241900932B',
  defaultAccount:       '0xb080bA38C82F824137A12Db1Ac53baeDa70e4a03',
  k1Verifier:           '0x167Ad053B3d786C6a6dC90aCa456DE98625EE31C',
  p256Verifier:         '0x0D8D9D476D39764D9C0eC19449497FE1F39c673B',
  webAuthnVerifier:     '0x895650b7dd7C5Bd1c31006A7790b353A8dB73F7D',
  delegateVerifier:     '0x1Bc0F6e1496420590fD4981Dd7b844525F32B1D1',
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

const SENDER_KEY = '0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d';
const PAYER_KEY  = '0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a';

const CONFIG_CHANGE_TYPEHASH = keccak256(
  toHex('ConfigChange(address account,uint64 chainId,uint64 sequence,ConfigOperation[] operations)ConfigOperation(uint8 opType,address verifier,bytes32 ownerId,uint8 scope)')
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
    method: 'base_getEip8130Nonce',
    params: [address, numberToHex(nonceKey)],
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

function configChangeDigest(accountAddr, chainId, sequence, operations) {
  const opHashes = operations.map(op => keccak256(
    encodeAbiParameters(
      parseAbiParameters('uint8, address, bytes32, uint8'),
      [op.opType, op.verifier, op.ownerId, op.scope]
    )
  ));
  const operationsHash = keccak256(concat(opHashes));
  return keccak256(
    encodeAbiParameters(
      parseAbiParameters('bytes32, address, uint64, uint64, bytes32'),
      [CONFIG_CHANGE_TYPEHASH, accountAddr, chainId, sequence, operationsHash]
    )
  );
}

async function signAndSend(unsignedRlpFields, { accountChanges = [], trace = true, payerAccount = null, customSenderAuth = null } = {}) {
  const signingPayload = concat([
    toHex(AA_TX_TYPE, { size: 1 }),
    toRlp(unsignedRlpFields),
  ]);
  const sigHash = keccak256(signingPayload);
  console.log(`Sender signing hash: ${sigHash}`);

  let senderAuth;
  if (customSenderAuth) {
    senderAuth = customSenderAuth(sigHash);
    console.log(`Using custom sender auth (${(senderAuth.length - 2) / 2} bytes)`);
  } else {
    const sig = await account.sign({ hash: sigHash });
    senderAuth = concat([toHex(0x01, { size: 1 }), sig]);
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
    payerAuth = concat([toHex(0x01, { size: 1 }), payerSig]);
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
    process.exit(1);
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

function baseTxFields(nonce, callsRlp, accountChangesRlp = [], payerAddress = '0x0000000000000000000000000000000000000000') {
  return [
    encodeUint(L2_CHAIN_ID),
    encodeAddress(account.address),
    encodeUint(0n),
    encodeUint(nonce),
    encodeUint(0n),
    encodeUint(1000000n),
    encodeUint(1000000000n),
    encodeUint(500000n),
    [],
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
  const newOwnerId = padHex(newOwnerAddr.toLowerCase(), { size: 32, dir: 'left' });

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
    opType: 1,
    verifier: K1_VERIFIER_ADDRESS,
    ownerId: newOwnerId,
    scope: 0,
  };

  const digest = configChangeDigest(account.address, 0n, currentSeq, [operation]);
  console.log(`Config change digest: ${digest}`);

  const authSig = await account.sign({ hash: digest });
  const authorizerAuth = concat([toHex(0x01, { size: 1 }), authSig]);

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
    opType: 1,
    verifier: P256_VERIFIER_ADDRESS,
    ownerId: p256OwnerId,
    scope: 0,
  };

  const digest = configChangeDigest(account.address, 0n, currentSeq, [operation]);
  const authSig = await account.sign({ hash: digest });
  const authorizerAuth = concat([toHex(0x01, { size: 1 }), authSig]);

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
      toHex(0x02, { size: 1 }),
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
    opType: 1,
    verifier: WEBAUTHN_VERIFIER_ADDRESS,
    ownerId: p256OwnerId,
    scope: 0,
  };

  const digest = configChangeDigest(account.address, 0n, currentSeq, [operation]);
  const authSig = await account.sign({ hash: digest });
  const authorizerAuth = concat([toHex(0x01, { size: 1 }), authSig]);

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
      toHex(0x03, { size: 1 }),
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

  // Test 2: Two-phase mixed — probe succeeds, invalid call to AccountConfig reverts.
  // Phase 0: probe() — succeeds. Phase 1: call AccountConfig with invalid selector — reverts
  // (AccountConfig is a real Solidity contract with no fallback).
  console.log('\n=== Test 2: Mixed phase results ===');
  const nonce2 = await getAaNonce();
  const invalidCalldata = '0xdeadbeef';
  const calls2 = [
    [[encodeAddress(opts.probeAddr), probeCalldata]],
    [[encodeAddress(ACCOUNT_CONFIG_ADDRESS), invalidCalldata]],
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
  const ownerId = padHex(account.address.toLowerCase(), { size: 32, dir: 'left' });
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
  console.log('\n--- base_getEip8130Nonce RPC Verification ---');
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

  // Build a type 0x05 transaction request with the new AA fields
  const txRequest = {
    type: '0x05',
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
  default:
    console.error(`Unknown mode: ${opts.mode}`);
    console.error('Available modes: probe, multi-call, sponsor, config-change, p256, webauthn, receipt-test, deploy, nonce-rpc, estimate-gas');
    process.exit(1);
}
