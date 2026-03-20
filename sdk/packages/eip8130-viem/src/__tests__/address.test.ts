import { describe, it, expect } from "vitest";
import { zeroAddress } from "viem";
import type { Owner } from "../types.js";
import {
  deploymentHeader,
  deploymentCode,
  effectiveSalt,
  create2Address,
  deriveAccountAddress,
} from "../address.js";

describe("deploymentHeader", () => {
  it("produces a 14-byte hex (28 hex chars + 0x prefix)", () => {
    const header = deploymentHeader(256);
    expect(header).toMatch(/^0x/);
    expect((header.length - 2) / 2).toBe(14);
  });

  it("first byte is PUSH2 opcode (0x61)", () => {
    const header = deploymentHeader(256);
    expect(header.slice(2, 4)).toBe("61");
  });
});

describe("deploymentCode", () => {
  it("concatenates header and bytecode", () => {
    const code = deploymentCode("0x6000f3");
    const bytes = (code.length - 2) / 2;
    expect(bytes).toBe(14 + 3);
  });
});

describe("effectiveSalt", () => {
  it("is order-independent for owners", () => {
    const ownerA: Owner = {
      verifier: "0x0000000000000000000000000000000000000001",
      ownerId: "0x" + "01".repeat(32),
      scope: 0,
    };
    const ownerB: Owner = {
      verifier: "0x0000000000000000000000000000000000000002",
      ownerId: "0x" + "02".repeat(32),
      scope: 0,
    };
    const salt = "0x" + "aa".repeat(32);

    const s1 = effectiveSalt(salt, [ownerA, ownerB]);
    const s2 = effectiveSalt(salt, [ownerB, ownerA]);
    expect(s1).toBe(s2);
  });
});

describe("create2Address", () => {
  it("is deterministic", () => {
    const deployer = "0x" + "dd".repeat(20) as `0x${string}`;
    const salt = "0x" + "aa".repeat(32);
    const code = "0x6000f3";

    const a1 = create2Address(deployer, salt, code);
    const a2 = create2Address(deployer, salt, code);
    expect(a1).toBe(a2);
    expect(a1).not.toBe(zeroAddress);
  });

  it("different salts produce different addresses", () => {
    const deployer = "0x" + "dd".repeat(20) as `0x${string}`;
    const salt1 = "0x" + "aa".repeat(32);
    const salt2 = "0x" + "bb".repeat(32);
    const code = "0x6000f3";

    expect(create2Address(deployer, salt1, code)).not.toBe(
      create2Address(deployer, salt2, code),
    );
  });
});

describe("deriveAccountAddress", () => {
  it("different owners produce different addresses", () => {
    const deployer = "0x" + "dd".repeat(20) as `0x${string}`;
    const salt = "0x" + "aa".repeat(32);
    const bytecode = "0x6000f3";

    const ownersA: Owner[] = [{
      verifier: "0x0000000000000000000000000000000000000001",
      ownerId: "0x" + "01".repeat(32),
      scope: 0,
    }];
    const ownersB: Owner[] = [{
      verifier: "0x0000000000000000000000000000000000000002",
      ownerId: "0x" + "02".repeat(32),
      scope: 0,
    }];

    const a1 = deriveAccountAddress(deployer, salt, bytecode, ownersA);
    const a2 = deriveAccountAddress(deployer, salt, bytecode, ownersB);
    expect(a1).not.toBe(a2);
  });
});
