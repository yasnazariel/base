import { describe, it, expect } from "vitest";
import { zeroAddress, keccak256 } from "viem";
import type { TxAa } from "../types.js";
import {
  rlpEncodeTxAa,
  encode2718,
  txHash,
  senderSignatureHash,
  payerSignatureHash,
} from "../rlp.js";
import { AA_TX_TYPE_ID } from "../constants.js";

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

describe("RLP encoding", () => {
  it("rlpEncodeTxAa produces a hex string", () => {
    const result = rlpEncodeTxAa(simpleTx());
    expect(result).toMatch(/^0x/);
    expect(result.length).toBeGreaterThan(10);
  });

  it("encode2718 prepends the type byte", () => {
    const result = encode2718(simpleTx());
    expect(result.startsWith("0x05")).toBe(true);
  });

  it("txHash is deterministic", () => {
    const tx = simpleTx();
    const h1 = txHash(tx);
    const h2 = txHash(tx);
    expect(h1).toBe(h2);
    expect(h1).toMatch(/^0x[0-9a-f]{64}$/);
  });

  it("different transactions produce different hashes", () => {
    const h1 = txHash(simpleTx({ nonceSequence: 0n }));
    const h2 = txHash(simpleTx({ nonceSequence: 1n }));
    expect(h1).not.toBe(h2);
  });
});

describe("signature hashes", () => {
  it("sender and payer hashes differ (domain separation)", () => {
    const tx = simpleTx();
    const senderHash = senderSignatureHash(tx);
    const payerHash = payerSignatureHash(tx);
    expect(senderHash).not.toBe(payerHash);
  });

  it("sender hash is deterministic", () => {
    const tx = simpleTx();
    expect(senderSignatureHash(tx)).toBe(senderSignatureHash(tx));
  });

  it("payer hash is deterministic", () => {
    const tx = simpleTx();
    expect(payerSignatureHash(tx)).toBe(payerSignatureHash(tx));
  });

  it("changing payer changes sender hash but not payer hash structure", () => {
    const tx1 = simpleTx({ payer: zeroAddress });
    const tx2 = simpleTx({
      payer: "0xcccccccccccccccccccccccccccccccccccccccc",
    });
    expect(senderSignatureHash(tx1)).not.toBe(senderSignatureHash(tx2));
  });
});
