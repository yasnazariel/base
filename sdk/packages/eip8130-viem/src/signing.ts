import {
  type Account,
  type Address,
  type Hex,
  type WalletClient,
  type Transport,
  type Chain,
  concat,
  toHex,
  hexToBytes,
  bytesToHex,
} from "viem";
import type { TxAa } from "./types.js";
import { senderSignatureHash, payerSignatureHash } from "./rlp.js";
import {
  VERIFIER_K1,
  VERIFIER_P256_RAW,
  VERIFIER_P256_WEBAUTHN,
} from "./constants.js";

/**
 * Signs an AA transaction as the sender using secp256k1 (K1 verifier).
 *
 * The resulting `senderAuth` is `0x01 || signature(65)` for configured owners,
 * or a raw 65-byte ECDSA signature for EOA mode (from == address(0)).
 */
export async function signAaTransaction(
  client: WalletClient<Transport, Chain, Account>,
  tx: TxAa,
): Promise<Hex> {
  const hash = senderSignatureHash(tx);
  const signature = await client.signMessage({
    message: { raw: hexToBytes(hash) },
  });

  const isEoa =
    tx.from === "0x0000000000000000000000000000000000000000";
  if (isEoa) {
    return signature;
  }
  return concat([toHex(VERIFIER_K1, { size: 1 }), signature]);
}

/**
 * Signs an AA transaction as the sender using P256 raw ECDSA.
 *
 * Requires a P256 signing function since viem's built-in signer uses secp256k1.
 * The `p256Sign` callback must return a 64-byte raw (r || s) signature.
 */
export async function signAaTransactionP256(
  tx: TxAa,
  p256Sign: (hash: Hex) => Promise<Hex>,
): Promise<Hex> {
  const hash = senderSignatureHash(tx);
  const rawSig = await p256Sign(hash);
  return concat([toHex(VERIFIER_P256_RAW, { size: 1 }), rawSig]);
}

/**
 * WebAuthn assertion envelope for P256 WebAuthn verification.
 */
export interface WebAuthnAssertion {
  authenticatorData: Hex;
  clientDataJSON: string;
  signature: Hex;
}

/**
 * Signs an AA transaction using WebAuthn / passkey (P256_WEBAUTHN verifier).
 *
 * The `webauthnSign` callback should perform the WebAuthn assertion ceremony
 * and return the authenticator data, client data JSON, and signature.
 */
export async function signAaTransactionWebAuthn(
  tx: TxAa,
  webauthnSign: (hash: Hex) => Promise<WebAuthnAssertion>,
): Promise<Hex> {
  const hash = senderSignatureHash(tx);
  const assertion = await webauthnSign(hash);

  const clientDataBytes = toHex(
    new TextEncoder().encode(assertion.clientDataJSON),
  );
  const envelope = concat([
    assertion.authenticatorData,
    toHex(clientDataBytes.length / 2 - 1, { size: 4 }),
    clientDataBytes,
    assertion.signature,
  ]);
  return concat([toHex(VERIFIER_P256_WEBAUTHN, { size: 1 }), envelope]);
}

/**
 * Signs an AA transaction as the payer using secp256k1.
 */
export async function signAaTransactionAsPayer(
  client: WalletClient<Transport, Chain, Account>,
  tx: TxAa,
): Promise<Hex> {
  const hash = payerSignatureHash(tx);
  const signature = await client.signMessage({
    message: { raw: hexToBytes(hash) },
  });
  return concat([toHex(VERIFIER_K1, { size: 1 }), signature]);
}
