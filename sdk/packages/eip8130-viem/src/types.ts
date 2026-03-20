import type { Address, Hex } from "viem";

export interface Call {
  to: Address;
  data: Hex;
}

export interface Owner {
  verifier: Address;
  ownerId: Hex;
  scope: number;
}

export interface ConfigOperation {
  opType: number;
  verifier: Address;
  ownerId: Hex;
  scope: number;
}

export const OP_AUTHORIZE_OWNER = 0x01 as const;
export const OP_REVOKE_OWNER = 0x02 as const;

export interface CreateEntry {
  type: "create";
  userSalt: Hex;
  bytecode: Hex;
  initialOwners: Owner[];
}

export interface ConfigChangeEntry {
  type: "configChange";
  chainId: bigint;
  sequence: bigint;
  operations: ConfigOperation[];
  authorizerAuth: Hex;
}

export type AccountChangeEntry = CreateEntry | ConfigChangeEntry;

export interface SignedAuthorization {
  chainId: bigint;
  address: Address;
  nonce: bigint;
  yParity: number;
  r: Hex;
  s: Hex;
}

/**
 * EIP-8130 Account Abstraction transaction parameters.
 *
 * RLP field order:
 * [chain_id, from, nonce_key, nonce_sequence, expiry,
 *  max_priority_fee_per_gas, max_fee_per_gas, gas_limit,
 *  authorization_list, account_changes, calls, payer,
 *  sender_auth, payer_auth]
 */
export interface TxAa {
  chainId: bigint;
  from: Address;
  nonceKey: bigint;
  nonceSequence: bigint;
  expiry: bigint;
  maxPriorityFeePerGas: bigint;
  maxFeePerGas: bigint;
  gasLimit: bigint;
  authorizationList: SignedAuthorization[];
  accountChanges: AccountChangeEntry[];
  calls: Call[][];
  payer: Address;
  senderAuth: Hex;
  payerAuth: Hex;
}

export function isEoa(tx: TxAa): boolean {
  return tx.from === "0x0000000000000000000000000000000000000000";
}

export function isSelfPay(tx: TxAa): boolean {
  return tx.payer === "0x0000000000000000000000000000000000000000";
}

export function effectivePayer(tx: TxAa): Address {
  return isSelfPay(tx) ? tx.from : tx.payer;
}
