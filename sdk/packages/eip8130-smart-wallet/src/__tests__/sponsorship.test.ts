import { describe, it, expect } from "vitest";
import { zeroAddress } from "viem";
import type { TxAa } from "@base-org/eip8130-viem";
import { applySponsorshipToTx } from "../sponsorship.js";

function simpleTx(): TxAa {
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
  };
}

describe("applySponsorshipToTx", () => {
  it("sets payer and payerAuth on the transaction", () => {
    const tx = simpleTx();
    const payer = "0xcccccccccccccccccccccccccccccccccccccccc" as const;
    const payerAuth = "0xdeadbeef" as const;

    const result = applySponsorshipToTx(tx, { payer, payerAuth });

    expect(result.payer).toBe(payer);
    expect(result.payerAuth).toBe(payerAuth);
    expect(result.from).toBe(tx.from);
    expect(result.senderAuth).toBe(tx.senderAuth);
  });

  it("does not mutate the original transaction", () => {
    const tx = simpleTx();
    const originalPayer = tx.payer;
    applySponsorshipToTx(tx, {
      payer: "0xcccccccccccccccccccccccccccccccccccccccc",
      payerAuth: "0xdeadbeef",
    });
    expect(tx.payer).toBe(originalPayer);
  });
});
