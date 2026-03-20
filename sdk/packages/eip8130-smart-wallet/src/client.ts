import type {
  Account,
  Address,
  Chain,
  Hex,
  PublicClient,
  Transport,
  WalletClient,
} from "viem";
import type { TxAa, Call, AccountChangeEntry } from "@base-org/eip8130-viem";
import {
  signAaTransaction,
  signAaTransactionP256,
  sendAaTransaction,
  getAaNonce,
  AA_TX_TYPE_ID,
} from "@base-org/eip8130-viem";
import type { WebAuthnAssertion } from "@base-org/eip8130-viem";
import { signAaTransactionWebAuthn } from "@base-org/eip8130-viem";
import { supportsEip8130, resolveTransactionPath } from "./detect.js";
import { sponsorTransaction, applySponsorshipToTx } from "./sponsorship.js";

export interface SmartWalletClientConfig {
  publicClient: PublicClient<Transport, Chain>;
  walletClient?: WalletClient<Transport, Chain, Account>;
  chainId: number;
  account: Address;
}

export type SigningMethod =
  | { type: "k1" }
  | { type: "p256"; sign: (hash: Hex) => Promise<Hex> }
  | { type: "webauthn"; sign: (hash: Hex) => Promise<WebAuthnAssertion> };

/**
 * High-level client for Coinbase Smart Wallet with EIP-8130 support.
 *
 * Automatically detects whether the connected chain supports EIP-8130 and
 * routes transactions accordingly. On supported chains, uses the native
 * AA transaction path; otherwise, falls back to ERC-4337.
 */
export class SmartWalletClient {
  readonly publicClient: PublicClient<Transport, Chain>;
  readonly walletClient?: WalletClient<Transport, Chain, Account>;
  readonly chainId: number;
  readonly account: Address;
  readonly isNativeAa: boolean;

  constructor(config: SmartWalletClientConfig) {
    this.publicClient = config.publicClient;
    this.walletClient = config.walletClient;
    this.chainId = config.chainId;
    this.account = config.account;
    this.isNativeAa = supportsEip8130(config.chainId);
  }

  get transactionPath() {
    return resolveTransactionPath(this.chainId);
  }

  async getNonce(nonceKey: bigint = 0n): Promise<bigint> {
    return getAaNonce(this.publicClient, {
      address: this.account,
      nonceKey,
    });
  }

  /**
   * Builds a TxAa for the given calls, auto-populating nonce and gas fields.
   */
  async buildTransaction(params: {
    calls: Call[][];
    nonceKey?: bigint;
    gasLimit?: bigint;
    maxFeePerGas?: bigint;
    maxPriorityFeePerGas?: bigint;
    expiry?: bigint;
    accountChanges?: AccountChangeEntry[];
    payer?: Address;
  }): Promise<TxAa> {
    const nonceKey = params.nonceKey ?? 0n;
    const nonce = await this.getNonce(nonceKey);

    const gasPrice = await this.publicClient.estimateFeesPerGas();

    return {
      chainId: BigInt(this.chainId),
      from: this.account,
      nonceKey,
      nonceSequence: nonce,
      expiry: params.expiry ?? 0n,
      maxPriorityFeePerGas:
        params.maxPriorityFeePerGas ?? gasPrice.maxPriorityFeePerGas ?? 1_000_000_000n,
      maxFeePerGas: params.maxFeePerGas ?? gasPrice.maxFeePerGas ?? 2_000_000_000n,
      gasLimit: params.gasLimit ?? 200_000n,
      authorizationList: [],
      accountChanges: params.accountChanges ?? [],
      calls: params.calls,
      payer: params.payer ?? ("0x0000000000000000000000000000000000000000" as Address),
      senderAuth: "0x",
      payerAuth: "0x",
    };
  }

  /**
   * Signs and sends an AA transaction using the specified signing method.
   */
  async sendTransaction(
    tx: TxAa,
    signingMethod: SigningMethod,
  ): Promise<Hex> {
    if (!this.isNativeAa) {
      throw new Error(
        `Chain ${this.chainId} does not support EIP-8130. Use ERC-4337 fallback.`,
      );
    }

    let senderAuth: Hex;
    switch (signingMethod.type) {
      case "k1": {
        if (!this.walletClient) {
          throw new Error("Wallet client required for K1 signing");
        }
        senderAuth = await signAaTransaction(this.walletClient, tx);
        break;
      }
      case "p256": {
        senderAuth = await signAaTransactionP256(tx, signingMethod.sign);
        break;
      }
      case "webauthn": {
        senderAuth = await signAaTransactionWebAuthn(tx, signingMethod.sign);
        break;
      }
    }

    const signedTx: TxAa = { ...tx, senderAuth };

    if (!this.walletClient) {
      throw new Error("Wallet client required to send transaction");
    }
    return sendAaTransaction(this.walletClient, signedTx);
  }

  /**
   * Sends a sponsored AA transaction where a third party pays for gas.
   */
  async sendSponsoredTransaction(
    tx: TxAa,
    signingMethod: SigningMethod,
    payerClient: WalletClient<Transport, Chain, Account>,
    payerAddress: Address,
  ): Promise<Hex> {
    const sponsored = await sponsorTransaction(payerClient, {
      tx,
      payerAddress,
    });

    const txWithSponsor = applySponsorshipToTx(tx, sponsored);
    return this.sendTransaction(txWithSponsor, signingMethod);
  }
}
