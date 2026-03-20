import type { Chain, Client, PublicClient, Transport } from "viem";

const CHAINS_WITH_8130_SUPPORT = new Set([
  8453,
  84532,
]);

/**
 * Detects whether a chain supports EIP-8130 native account abstraction.
 *
 * When supported, smart wallet operations should use the native EIP-8130
 * transaction path instead of the ERC-4337 fallback.
 */
export function supportsEip8130(chainId: number): boolean {
  return CHAINS_WITH_8130_SUPPORT.has(chainId);
}

/**
 * Detects EIP-8130 support by querying the connected chain.
 */
export async function detectEip8130Support(
  client: PublicClient<Transport, Chain>,
): Promise<boolean> {
  const chainId = await client.getChainId();
  return supportsEip8130(chainId);
}

export type TransactionPathKind = "eip8130" | "erc4337";

/**
 * Returns the optimal transaction path for the given chain.
 */
export function resolveTransactionPath(chainId: number): TransactionPathKind {
  return supportsEip8130(chainId) ? "eip8130" : "erc4337";
}
