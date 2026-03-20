import type { Address, Hex } from "viem";

export const AA_TX_TYPE_ID = 0x05 as const;
export const AA_PAYER_TYPE = 0x06 as const;

export const AA_BASE_COST = 15_000n;
export const DEPLOYMENT_HEADER_SIZE = 14;
export const MAX_SIGNATURE_SIZE = 2048;

export const NONCE_KEY_COLD_GAS = 22_100n;
export const NONCE_KEY_WARM_GAS = 5_000n;
export const BYTECODE_BASE_GAS = 32_000n;
export const BYTECODE_PER_BYTE_GAS = 200n;
export const CONFIG_CHANGE_OP_GAS = 20_000n;
export const CONFIG_CHANGE_SKIP_GAS = 2_100n;
export const SLOAD_GAS = 2_100n;
export const EOA_AUTH_GAS = 6_000n;

export const VERIFIER_CUSTOM = 0x00 as const;
export const VERIFIER_K1 = 0x01 as const;
export const VERIFIER_P256_RAW = 0x02 as const;
export const VERIFIER_P256_WEBAUTHN = 0x03 as const;
export const VERIFIER_DELEGATE = 0x04 as const;

export const ACCOUNT_CONFIG_ADDRESS: Address =
  "0x000000000000000000000000000000000000aa01";
export const NONCE_MANAGER_ADDRESS: Address =
  "0x000000000000000000000000000000000000aa02";
export const TX_CONTEXT_ADDRESS: Address =
  "0x000000000000000000000000000000000000aa03";
export const DEFAULT_ACCOUNT_ADDRESS: Address =
  "0x000000000000000000000000000000000000aa04";
export const K1_VERIFIER_ADDRESS: Address =
  "0x000000000000000000000000000000000000aa10";
export const P256_RAW_VERIFIER_ADDRESS: Address =
  "0x000000000000000000000000000000000000aa11";
export const P256_WEBAUTHN_VERIFIER_ADDRESS: Address =
  "0x000000000000000000000000000000000000aa12";
export const DELEGATE_VERIFIER_ADDRESS: Address =
  "0x000000000000000000000000000000000000aa13";

export const INonceManagerAbi = [
  {
    type: "function",
    name: "getNonce",
    inputs: [
      { name: "account", type: "address" },
      { name: "nonceKey", type: "uint256" },
    ],
    outputs: [{ name: "", type: "uint64" }],
    stateMutability: "view",
  },
] as const;

export const IAccountConfigAbi = [
  {
    type: "function",
    name: "getOwnerConfig",
    inputs: [
      { name: "account", type: "address" },
      { name: "ownerId", type: "bytes32" },
    ],
    outputs: [
      { name: "verifier", type: "address" },
      { name: "scope", type: "uint8" },
    ],
    stateMutability: "view",
  },
] as const;

export const OwnerScope = {
  SIGNATURE: 0x01,
  SENDER: 0x02,
  PAYER: 0x04,
  CONFIG: 0x08,
  UNRESTRICTED: 0x00,
} as const;

export function hasScope(scope: number, permission: number): boolean {
  return scope === OwnerScope.UNRESTRICTED || (scope & permission) !== 0;
}
