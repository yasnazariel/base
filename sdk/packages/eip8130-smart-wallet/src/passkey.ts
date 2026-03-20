import type { Hex } from "viem";
import { concat, toHex } from "viem";
import type { TxAa } from "@base-org/eip8130-viem";
import {
  senderSignatureHash,
  VERIFIER_P256_WEBAUTHN,
} from "@base-org/eip8130-viem";

export interface PasskeyCredential {
  id: string;
  publicKey: Hex;
}

export interface PasskeySignatureResult {
  senderAuth: Hex;
  authenticatorData: Hex;
  clientDataJSON: string;
  signature: Hex;
}

/**
 * Signs an AA transaction using a WebAuthn passkey.
 *
 * This function triggers the browser's WebAuthn assertion ceremony and
 * encodes the result as EIP-8130 `sender_auth` with the P256_WEBAUTHN
 * verifier type prefix.
 *
 * Requires a browser environment with WebAuthn support.
 */
export async function signWithPasskey(
  tx: TxAa,
  credentialId: string,
  rpId: string,
): Promise<PasskeySignatureResult> {
  if (typeof navigator === "undefined" || !navigator.credentials) {
    throw new Error("WebAuthn is not available in this environment");
  }

  const hash = senderSignatureHash(tx);
  const challenge = hexToUint8Array(hash);

  const credential = (await navigator.credentials.get({
    publicKey: {
      challenge,
      allowCredentials: [
        {
          id: base64UrlDecode(credentialId),
          type: "public-key",
        },
      ],
      rpId,
      userVerification: "required",
      timeout: 60000,
    },
  })) as PublicKeyCredential;

  const response = credential.response as AuthenticatorAssertionResponse;

  const authenticatorData = toHex(new Uint8Array(response.authenticatorData));
  const clientDataJSON = new TextDecoder().decode(response.clientDataJSON);
  const signature = toHex(new Uint8Array(response.signature));

  const clientDataBytes = toHex(
    new TextEncoder().encode(clientDataJSON),
  );

  const envelope = concat([
    authenticatorData,
    toHex(Math.floor((clientDataBytes.length - 2) / 2), { size: 4 }),
    clientDataBytes,
    signature,
  ]);

  const senderAuth = concat([
    toHex(VERIFIER_P256_WEBAUTHN, { size: 1 }),
    envelope,
  ]);

  return {
    senderAuth,
    authenticatorData,
    clientDataJSON,
    signature,
  };
}

function hexToUint8Array(hex: string): Uint8Array {
  const stripped = hex.startsWith("0x") ? hex.slice(2) : hex;
  const bytes = new Uint8Array(stripped.length / 2);
  for (let i = 0; i < bytes.length; i++) {
    bytes[i] = parseInt(stripped.slice(i * 2, i * 2 + 2), 16);
  }
  return bytes;
}

function base64UrlDecode(input: string): ArrayBuffer {
  const base64 = input.replace(/-/g, "+").replace(/_/g, "/");
  const padded = base64 + "=".repeat((4 - (base64.length % 4)) % 4);
  const binary = atob(padded);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes.buffer;
}
