import { describe, it, expect } from "vitest";
import {
  AA_TX_TYPE_ID,
  AA_PAYER_TYPE,
  AA_BASE_COST,
  VERIFIER_K1,
  VERIFIER_P256_RAW,
  VERIFIER_P256_WEBAUTHN,
  VERIFIER_DELEGATE,
  VERIFIER_CUSTOM,
  ACCOUNT_CONFIG_ADDRESS,
  NONCE_MANAGER_ADDRESS,
  TX_CONTEXT_ADDRESS,
  DEFAULT_ACCOUNT_ADDRESS,
  K1_VERIFIER_ADDRESS,
  P256_RAW_VERIFIER_ADDRESS,
  P256_WEBAUTHN_VERIFIER_ADDRESS,
  DELEGATE_VERIFIER_ADDRESS,
  OwnerScope,
  hasScope,
} from "../constants.js";

describe("constants", () => {
  it("AA_TX_TYPE_ID matches spec", () => {
    expect(AA_TX_TYPE_ID).toBe(0x05);
  });

  it("AA_PAYER_TYPE provides domain separation", () => {
    expect(AA_PAYER_TYPE).toBe(0x06);
    expect(AA_PAYER_TYPE).not.toBe(AA_TX_TYPE_ID);
  });

  it("AA_BASE_COST is 15000", () => {
    expect(AA_BASE_COST).toBe(15_000n);
  });

  it("verifier type bytes are sequential", () => {
    expect(VERIFIER_CUSTOM).toBe(0x00);
    expect(VERIFIER_K1).toBe(0x01);
    expect(VERIFIER_P256_RAW).toBe(0x02);
    expect(VERIFIER_P256_WEBAUTHN).toBe(0x03);
    expect(VERIFIER_DELEGATE).toBe(0x04);
  });

  it("predeploy addresses are unique", () => {
    const addresses = [
      ACCOUNT_CONFIG_ADDRESS,
      NONCE_MANAGER_ADDRESS,
      TX_CONTEXT_ADDRESS,
      DEFAULT_ACCOUNT_ADDRESS,
      K1_VERIFIER_ADDRESS,
      P256_RAW_VERIFIER_ADDRESS,
      P256_WEBAUTHN_VERIFIER_ADDRESS,
      DELEGATE_VERIFIER_ADDRESS,
    ];
    expect(new Set(addresses).size).toBe(addresses.length);
  });
});

describe("OwnerScope", () => {
  it("unrestricted grants all permissions", () => {
    expect(hasScope(OwnerScope.UNRESTRICTED, OwnerScope.SENDER)).toBe(true);
    expect(hasScope(OwnerScope.UNRESTRICTED, OwnerScope.PAYER)).toBe(true);
    expect(hasScope(OwnerScope.UNRESTRICTED, OwnerScope.CONFIG)).toBe(true);
    expect(hasScope(OwnerScope.UNRESTRICTED, OwnerScope.SIGNATURE)).toBe(true);
  });

  it("specific scope grants only that permission", () => {
    expect(hasScope(OwnerScope.SENDER, OwnerScope.SENDER)).toBe(true);
    expect(hasScope(OwnerScope.SENDER, OwnerScope.PAYER)).toBe(false);
  });

  it("combined scope grants both permissions", () => {
    const combined = OwnerScope.SENDER | OwnerScope.PAYER;
    expect(hasScope(combined, OwnerScope.SENDER)).toBe(true);
    expect(hasScope(combined, OwnerScope.PAYER)).toBe(true);
    expect(hasScope(combined, OwnerScope.CONFIG)).toBe(false);
  });
});
