import type {
  Account,
  Address,
  Chain,
  Hex,
  Transport,
  WalletClient,
} from "viem";
import type { TxAa } from "@base-org/eip8130-viem";
import {
  signAaTransactionAsPayer,
  effectivePayer,
} from "@base-org/eip8130-viem";

export interface SponsorshipRequest {
  tx: TxAa;
  payerAddress: Address;
}

export interface SponsorshipResponse {
  payerAuth: Hex;
  payer: Address;
}

/**
 * Signs an AA transaction as a gas sponsor (payer).
 *
 * Returns the `payerAuth` bytes and the payer address to set on the
 * transaction before submission. The caller should set:
 *
 * ```ts
 * tx.payer = response.payer;
 * tx.payerAuth = response.payerAuth;
 * ```
 */
export async function sponsorTransaction(
  payerClient: WalletClient<Transport, Chain, Account>,
  request: SponsorshipRequest,
): Promise<SponsorshipResponse> {
  const txWithPayer: TxAa = {
    ...request.tx,
    payer: request.payerAddress,
  };

  const payerAuth = await signAaTransactionAsPayer(payerClient, txWithPayer);

  return {
    payerAuth,
    payer: request.payerAddress,
  };
}

/**
 * Applies sponsorship to a transaction, returning a new TxAa with payer fields set.
 */
export function applySponsorshipToTx(
  tx: TxAa,
  sponsorship: SponsorshipResponse,
): TxAa {
  return {
    ...tx,
    payer: sponsorship.payer,
    payerAuth: sponsorship.payerAuth,
  };
}
