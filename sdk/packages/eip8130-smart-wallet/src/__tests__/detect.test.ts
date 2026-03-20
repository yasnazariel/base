import { describe, it, expect } from "vitest";
import {
  supportsEip8130,
  resolveTransactionPath,
} from "../detect.js";

describe("supportsEip8130", () => {
  it("returns true for Base mainnet (8453)", () => {
    expect(supportsEip8130(8453)).toBe(true);
  });

  it("returns true for Base Sepolia (84532)", () => {
    expect(supportsEip8130(84532)).toBe(true);
  });

  it("returns false for Ethereum mainnet", () => {
    expect(supportsEip8130(1)).toBe(false);
  });

  it("returns false for unknown chain", () => {
    expect(supportsEip8130(99999)).toBe(false);
  });
});

describe("resolveTransactionPath", () => {
  it("returns eip8130 for supported chains", () => {
    expect(resolveTransactionPath(8453)).toBe("eip8130");
  });

  it("returns erc4337 for unsupported chains", () => {
    expect(resolveTransactionPath(1)).toBe("erc4337");
  });
});
