import { describe, it, expect } from "vitest";
import { zeroAddress } from "viem";
import type { TxAa } from "../types.js";
import { isEoa, isSelfPay, effectivePayer } from "../types.js";

function simpleTx(overrides: Partial<TxAa> = {}): TxAa {
  return {
    chainId: 8453n,
    from: "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    nonceKey: 0n,
    nonceSequence: 0n,
    expiry: 0n,
    maxPriorityFeePerGas: 1_000_000_000n,
    maxFeePerGas: 2_000_000_000n,
    gasLimit: 100_000n,
    authorizationList: [],
    accountChanges: [],
    calls: [[{ to: "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", data: "0x0102" }]],
    payer: zeroAddress,
    senderAuth: "0x" + "00".repeat(65),
    payerAuth: "0x",
    ...overrides,
  };
}

describe("TxAa helpers", () => {
  it("isEoa returns true when from is zero address", () => {
    expect(isEoa(simpleTx({ from: zeroAddress }))).toBe(true);
  });

  it("isEoa returns false for non-zero from", () => {
    expect(isEoa(simpleTx())).toBe(false);
  });

  it("isSelfPay returns true when payer is zero", () => {
    expect(isSelfPay(simpleTx())).toBe(true);
  });

  it("isSelfPay returns false for non-zero payer", () => {
    expect(
      isSelfPay(simpleTx({ payer: "0xcccccccccccccccccccccccccccccccccccccccc" })),
    ).toBe(false);
  });

  it("effectivePayer returns payer when sponsored", () => {
    const payer = "0xcccccccccccccccccccccccccccccccccccccccc" as const;
    expect(effectivePayer(simpleTx({ payer }))).toBe(payer);
  });

  it("effectivePayer returns from when self-pay", () => {
    const tx = simpleTx();
    expect(effectivePayer(tx)).toBe(tx.from);
  });
});
