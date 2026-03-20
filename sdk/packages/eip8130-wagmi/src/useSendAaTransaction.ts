import { useMutation } from "@tanstack/react-query";
import { useWalletClient, usePublicClient } from "wagmi";
import type { Hex } from "viem";
import type { TxAa } from "@base-org/eip8130-viem";
import {
  signAaTransaction,
  sendAaTransaction,
} from "@base-org/eip8130-viem";

export interface UseSendAaTransactionResult {
  sendAaTransaction: (tx: TxAa) => void;
  sendAaTransactionAsync: (tx: TxAa) => Promise<Hex>;
  data: Hex | undefined;
  isPending: boolean;
  isSuccess: boolean;
  isError: boolean;
  error: Error | null;
  reset: () => void;
}

/**
 * React hook to sign and send an EIP-8130 AA transaction.
 *
 * Wraps the viem `signAaTransaction` and `sendAaTransaction` functions
 * with React Query state management.
 */
export function useSendAaTransaction(): UseSendAaTransactionResult {
  const { data: walletClient } = useWalletClient();

  const mutation = useMutation({
    mutationFn: async (tx: TxAa): Promise<Hex> => {
      if (!walletClient) {
        throw new Error("Wallet not connected");
      }

      const senderAuth = await signAaTransaction(walletClient, tx);
      const signedTx: TxAa = { ...tx, senderAuth };
      return sendAaTransaction(walletClient, signedTx);
    },
  });

  return {
    sendAaTransaction: mutation.mutate as (tx: TxAa) => void,
    sendAaTransactionAsync: mutation.mutateAsync,
    data: mutation.data,
    isPending: mutation.isPending,
    isSuccess: mutation.isSuccess,
    isError: mutation.isError,
    error: mutation.error,
    reset: mutation.reset,
  };
}
