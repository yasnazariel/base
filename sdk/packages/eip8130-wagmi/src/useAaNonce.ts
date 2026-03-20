import { useQuery } from "@tanstack/react-query";
import { usePublicClient } from "wagmi";
import type { Address } from "viem";
import { getAaNonce } from "@base-org/eip8130-viem";

export interface UseAaNonceParams {
  address: Address | undefined;
  nonceKey?: bigint;
  enabled?: boolean;
}

export interface UseAaNonceResult {
  data: bigint | undefined;
  isLoading: boolean;
  isError: boolean;
  error: Error | null;
  refetch: () => void;
}

/**
 * React hook to read an AA account's 2D nonce from the NonceManager precompile.
 */
export function useAaNonce(params: UseAaNonceParams): UseAaNonceResult {
  const publicClient = usePublicClient();
  const nonceKey = params.nonceKey ?? 0n;

  const query = useQuery({
    queryKey: ["aa-nonce", params.address, nonceKey.toString()],
    queryFn: async () => {
      if (!publicClient || !params.address) {
        throw new Error("Client not available or address missing");
      }
      return getAaNonce(publicClient, {
        address: params.address,
        nonceKey,
      });
    },
    enabled: (params.enabled ?? true) && !!params.address && !!publicClient,
  });

  return {
    data: query.data,
    isLoading: query.isLoading,
    isError: query.isError,
    error: query.error,
    refetch: query.refetch,
  };
}
