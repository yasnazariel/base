import {
  type Address,
  type Hex,
  concat,
  keccak256,
  toHex,
  hexToBytes,
  getAddress,
  slice,
} from "viem";
import type { Owner } from "./types.js";
import { DEPLOYMENT_HEADER_SIZE } from "./constants.js";

/**
 * Builds the 14-byte EVM deployment header that wraps bytecode for CREATE2.
 */
export function deploymentHeader(bytecodeLength: number): Hex {
  const hi = (bytecodeLength >> 8) & 0xff;
  const lo = bytecodeLength & 0xff;
  const bytes = new Uint8Array([
    0x61, hi, lo,
    0x80,
    0x60, 0x0e,
    0x60, 0x00,
    0x39,
    0x60, 0x00,
    0xf3,
    0x00, 0x00,
  ]);
  return toHex(bytes);
}

/**
 * Builds the full deployment code: header(len) || bytecode.
 */
export function deploymentCode(bytecode: Hex): Hex {
  const bytecodeBytes = hexToBytes(bytecode);
  const header = deploymentHeader(bytecodeBytes.length);
  return concat([header, bytecode]);
}

/**
 * Computes the effective_salt for CREATE2 address derivation.
 *
 * Owners are sorted by ownerId to make the address independent of ordering.
 * `effective_salt = keccak256(user_salt || keccak256(ownerId_0 || verifier_0 || scope_0 || ...))`
 */
export function effectiveSalt(userSalt: Hex, initialOwners: Owner[]): Hex {
  const sorted = [...initialOwners].sort((a, b) => {
    const aBytes = hexToBytes(a.ownerId as Hex);
    const bBytes = hexToBytes(b.ownerId as Hex);
    for (let i = 0; i < aBytes.length; i++) {
      if (aBytes[i] !== bBytes[i]) return aBytes[i] - bBytes[i];
    }
    return 0;
  });

  const commitmentParts: Hex[] = sorted.flatMap((owner) => [
    owner.ownerId as Hex,
    owner.verifier as Hex,
    toHex(owner.scope, { size: 1 }),
  ]);

  const ownersCommitment = keccak256(
    commitmentParts.length > 0 ? concat(commitmentParts) : "0x",
  );

  return keccak256(concat([userSalt, ownersCommitment]));
}

/**
 * Computes the CREATE2 address.
 *
 * `address = keccak256(0xff || deployer || salt || keccak256(initCode))[12:]`
 */
export function create2Address(
  deployer: Address,
  salt: Hex,
  initCode: Hex,
): Address {
  const initCodeHash = keccak256(initCode);
  const hash = keccak256(
    concat([
      "0xff",
      deployer as Hex,
      salt,
      initCodeHash,
    ]),
  );
  return getAddress(slice(hash, 12));
}

/**
 * Full address derivation for an account creation entry.
 */
export function deriveAccountAddress(
  deployer: Address,
  userSalt: Hex,
  bytecode: Hex,
  initialOwners: Owner[],
): Address {
  const salt = effectiveSalt(userSalt, initialOwners);
  const code = deploymentCode(bytecode);
  return create2Address(deployer, salt, code);
}
