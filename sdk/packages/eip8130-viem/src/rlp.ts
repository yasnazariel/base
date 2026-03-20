import {
  type Address,
  type Hex,
  concat,
  hexToBytes,
  bytesToHex,
  toHex,
  toBytes,
  toRlp,
  keccak256,
  pad,
  size,
} from "viem";
import type {
  TxAa,
  Call,
  AccountChangeEntry,
  SignedAuthorization,
  Owner,
  ConfigOperation,
} from "./types.js";
import { AA_TX_TYPE_ID, AA_PAYER_TYPE } from "./constants.js";

function bigintToHex(value: bigint): Hex {
  if (value === 0n) return "0x";
  return toHex(value);
}

function addressToHex(address: Address): Hex {
  return address.toLowerCase() as Hex;
}

function encodeCall(call: Call): Hex {
  return toRlp([addressToHex(call.to), call.data]);
}

function encodeOwner(owner: Owner): Hex {
  return toRlp([
    addressToHex(owner.verifier),
    owner.ownerId,
    bigintToHex(BigInt(owner.scope)),
  ]);
}

function encodeConfigOp(op: ConfigOperation): Hex {
  return toRlp([
    bigintToHex(BigInt(op.opType)),
    addressToHex(op.verifier),
    op.ownerId,
    bigintToHex(BigInt(op.scope)),
  ]);
}

function encodeAccountChange(entry: AccountChangeEntry): Hex {
  if (entry.type === "create") {
    return toRlp([
      "0x",
      entry.userSalt,
      entry.bytecode,
      toRlp(entry.initialOwners.map(encodeOwner)) as Hex,
    ]);
  }
  return toRlp([
    bigintToHex(1n),
    bigintToHex(entry.chainId),
    bigintToHex(entry.sequence),
    toRlp(entry.operations.map(encodeConfigOp)) as Hex,
    entry.authorizerAuth,
  ]);
}

function encodeSignedAuth(auth: SignedAuthorization): Hex {
  return toRlp([
    bigintToHex(auth.chainId),
    addressToHex(auth.address),
    bigintToHex(auth.nonce),
    bigintToHex(BigInt(auth.yParity)),
    auth.r,
    auth.s,
  ]);
}

function encodeCalls(calls: Call[][]): Hex {
  return toRlp(calls.map((phase) => toRlp(phase.map(encodeCall)))) as Hex;
}

/**
 * RLP-encodes the full EIP-8130 transaction fields (without the type prefix).
 */
export function rlpEncodeTxAa(tx: TxAa): Hex {
  return toRlp([
    bigintToHex(tx.chainId),
    addressToHex(tx.from),
    bigintToHex(tx.nonceKey),
    bigintToHex(tx.nonceSequence),
    bigintToHex(tx.expiry),
    bigintToHex(tx.maxPriorityFeePerGas),
    bigintToHex(tx.maxFeePerGas),
    bigintToHex(tx.gasLimit),
    toRlp(tx.authorizationList.map(encodeSignedAuth)) as Hex,
    toRlp(tx.accountChanges.map(encodeAccountChange)) as Hex,
    encodeCalls(tx.calls),
    addressToHex(tx.payer),
    tx.senderAuth,
    tx.payerAuth,
  ]);
}

/**
 * EIP-2718 encoding: type_byte || RLP(fields).
 */
export function encode2718(tx: TxAa): Hex {
  const rlp = rlpEncodeTxAa(tx);
  return concat([toHex(AA_TX_TYPE_ID, { size: 1 }), rlp]);
}

/**
 * Computes the transaction hash: keccak256(0x05 || RLP(fields)).
 */
export function txHash(tx: TxAa): Hex {
  return keccak256(encode2718(tx));
}

/**
 * Encodes the fields for the **sender** signature hash.
 *
 * `keccak256(AA_TX_TYPE || rlp([chain_id, from, nonce_key, nonce_sequence, expiry,
 *   max_priority_fee_per_gas, max_fee_per_gas, gas_limit,
 *   authorization_list, account_changes, calls, payer]))`
 */
export function senderSigningPayload(tx: TxAa): Hex {
  const rlp = toRlp([
    bigintToHex(tx.chainId),
    addressToHex(tx.from),
    bigintToHex(tx.nonceKey),
    bigintToHex(tx.nonceSequence),
    bigintToHex(tx.expiry),
    bigintToHex(tx.maxPriorityFeePerGas),
    bigintToHex(tx.maxFeePerGas),
    bigintToHex(tx.gasLimit),
    toRlp(tx.authorizationList.map(encodeSignedAuth)) as Hex,
    toRlp(tx.accountChanges.map(encodeAccountChange)) as Hex,
    encodeCalls(tx.calls),
    addressToHex(tx.payer),
  ]);
  return concat([toHex(AA_TX_TYPE_ID, { size: 1 }), rlp]);
}

/**
 * Encodes the fields for the **payer** signature hash.
 *
 * `keccak256(AA_PAYER_TYPE || rlp([chain_id, from, nonce_key, nonce_sequence, expiry,
 *   max_priority_fee_per_gas, max_fee_per_gas, gas_limit,
 *   authorization_list, account_changes, calls]))`
 */
export function payerSigningPayload(tx: TxAa): Hex {
  const rlp = toRlp([
    bigintToHex(tx.chainId),
    addressToHex(tx.from),
    bigintToHex(tx.nonceKey),
    bigintToHex(tx.nonceSequence),
    bigintToHex(tx.expiry),
    bigintToHex(tx.maxPriorityFeePerGas),
    bigintToHex(tx.maxFeePerGas),
    bigintToHex(tx.gasLimit),
    toRlp(tx.authorizationList.map(encodeSignedAuth)) as Hex,
    toRlp(tx.accountChanges.map(encodeAccountChange)) as Hex,
    encodeCalls(tx.calls),
  ]);
  return concat([toHex(AA_PAYER_TYPE, { size: 1 }), rlp]);
}

/**
 * Computes the sender signature hash.
 */
export function senderSignatureHash(tx: TxAa): Hex {
  return keccak256(senderSigningPayload(tx));
}

/**
 * Computes the payer signature hash.
 */
export function payerSignatureHash(tx: TxAa): Hex {
  return keccak256(payerSigningPayload(tx));
}
