export {
  supportsEip8130,
  detectEip8130Support,
  resolveTransactionPath,
} from "./detect.js";
export type { TransactionPathKind } from "./detect.js";

export { signWithPasskey } from "./passkey.js";
export type {
  PasskeyCredential,
  PasskeySignatureResult,
} from "./passkey.js";

export {
  sponsorTransaction,
  applySponsorshipToTx,
} from "./sponsorship.js";
export type {
  SponsorshipRequest,
  SponsorshipResponse,
} from "./sponsorship.js";

export { SmartWalletClient } from "./client.js";
export type {
  SmartWalletClientConfig,
  SigningMethod,
} from "./client.js";
