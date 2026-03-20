import { describe, it, expect } from "vitest";
import {
  useSendAaTransaction,
  useAaNonce,
  useAccountConfig,
} from "../index.js";

describe("wagmi hooks exports", () => {
  it("exports useSendAaTransaction", () => {
    expect(typeof useSendAaTransaction).toBe("function");
  });

  it("exports useAaNonce", () => {
    expect(typeof useAaNonce).toBe("function");
  });

  it("exports useAccountConfig", () => {
    expect(typeof useAccountConfig).toBe("function");
  });
});
