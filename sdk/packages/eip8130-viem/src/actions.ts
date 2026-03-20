import {
  type Account,
  type Address,
  type Chain,
  type Client,
  type Hex,
  type PublicClient,
  type Transport,
  type WalletClient,
  encodeFunctionData,
  decodeFunctionResult,
} from "viem";
import type { TxAa } from "./types.js";
import { encode2718 } from "./rlp.js";
import {
  INonceManagerAbi,
  IAccountConfigAbi,
  NONCE_MANAGER_ADDRESS,
  ACCOUNT_CONFIG_ADDRESS,
} from "./constants.js";

/**
 * Sends a fully signed AA transaction via `eth_sendRawTransaction`.
 */
export async function sendAaTransaction(
  client: WalletClient<Transport, Chain, Account>,
  tx: TxAa,
): Promise<Hex> {
  const encoded = encode2718(tx);
  return client.request({
    method: "eth_sendRawTransaction",
    params: [encoded],
  }) as Promise<Hex>;
}

/**
 * Reads the current nonce for an AA account's 2D nonce key via the NonceManager precompile.
 */
export async function getAaNonce(
  client: PublicClient<Transport, Chain>,
  params: { address: Address; nonceKey: bigint },
): Promise<bigint> {
  const data = encodeFunctionData({
    abi: INonceManagerAbi,
    functionName: "getNonce",
    args: [params.address, params.nonceKey],
  });

  const result = await client.call({
    to: NONCE_MANAGER_ADDRESS,
    data,
  });

  if (!result.data) {
    throw new Error("NonceManager returned no data");
  }

  const decoded = decodeFunctionResult({
    abi: INonceManagerAbi,
    functionName: "getNonce",
    data: result.data,
  });

  return decoded as bigint;
}

/**
 * Reads the owner configuration for an account from the AccountConfig system contract.
 */
export async function getOwnerConfig(
  client: PublicClient<Transport, Chain>,
  params: { account: Address; ownerId: Hex },
): Promise<{ verifier: Address; scope: number }> {
  const data = encodeFunctionData({
    abi: IAccountConfigAbi,
    functionName: "getOwnerConfig",
    args: [params.account, params.ownerId],
  });

  const result = await client.call({
    to: ACCOUNT_CONFIG_ADDRESS,
    data,
  });

  if (!result.data) {
    throw new Error("AccountConfig returned no data");
  }

  const decoded = decodeFunctionResult({
    abi: IAccountConfigAbi,
    functionName: "getOwnerConfig",
    data: result.data,
  });

  const [verifier, scope] = decoded as [Address, number];
  return { verifier, scope };
}

/**
 * Queries the AA nonce via the custom `base_getAaNonce` RPC method.
 */
export async function getAaNonceViaRpc(
  client: Client<Transport, Chain>,
  params: { address: Address; nonceKey: bigint },
): Promise<bigint> {
  const result = await client.request({
    method: "base_getAaNonce" as any,
    params: [params.address, `0x${params.nonceKey.toString(16)}`] as any,
  });
  return BigInt(result as string);
}
