import { useQuery } from "@tanstack/react-query";
import { usePublicClient } from "wagmi";
import type { Address, Hex } from "viem";
import { getOwnerConfig } from "@base-org/eip8130-viem";

export interface UseAccountConfigParams {
  account: Address | undefined;
  ownerId: Hex | undefined;
  enabled?: boolean;
}

export interface OwnerConfigResult {
  verifier: Address;
  scope: number;
}

export interface UseAccountConfigResult {
  data: OwnerConfigResult | undefined;
  isLoading: boolean;
  isError: boolean;
  error: Error | null;
  refetch: () => void;
}

/**
 * React hook to read an owner's configuration from the AccountConfig system contract.
 */
export function useAccountConfig(
  params: UseAccountConfigParams,
): UseAccountConfigResult {
  const publicClient = usePublicClient();

  const query = useQuery({
    queryKey: ["account-config", params.account, params.ownerId],
    queryFn: async () => {
      if (!publicClient || !params.account || !params.ownerId) {
        throw new Error("Client not available or params missing");
      }
      return getOwnerConfig(publicClient, {
        account: params.account,
        ownerId: params.ownerId,
      });
    },
    enabled:
      (params.enabled ?? true) &&
      !!params.account &&
      !!params.ownerId &&
      !!publicClient,
  });

  return {
    data: query.data,
    isLoading: query.isLoading,
    isError: query.isError,
    error: query.error,
    refetch: query.refetch,
  };
}
