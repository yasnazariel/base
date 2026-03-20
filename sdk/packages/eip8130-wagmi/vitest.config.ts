import { defineConfig } from "vitest/config";
import path from "path";

export default defineConfig({
  resolve: {
    alias: {
      "@base-org/eip8130-viem": path.resolve(
        __dirname,
        "../eip8130-viem/src/index.ts",
      ),
    },
  },
  test: {
    globals: true,
  },
});
