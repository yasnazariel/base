export {
  AA_TX_TYPE_ID,
  AA_PAYER_TYPE,
  AA_BASE_COST,
  DEPLOYMENT_HEADER_SIZE,
  MAX_SIGNATURE_SIZE,
  NONCE_KEY_COLD_GAS,
  NONCE_KEY_WARM_GAS,
  BYTECODE_BASE_GAS,
  BYTECODE_PER_BYTE_GAS,
  CONFIG_CHANGE_OP_GAS,
  CONFIG_CHANGE_SKIP_GAS,
  SLOAD_GAS,
  EOA_AUTH_GAS,
  VERIFIER_CUSTOM,
  VERIFIER_K1,
  VERIFIER_P256_RAW,
  VERIFIER_P256_WEBAUTHN,
  VERIFIER_DELEGATE,
  ACCOUNT_CONFIG_ADDRESS,
  NONCE_MANAGER_ADDRESS,
  TX_CONTEXT_ADDRESS,
  DEFAULT_ACCOUNT_ADDRESS,
  K1_VERIFIER_ADDRESS,
  P256_RAW_VERIFIER_ADDRESS,
  P256_WEBAUTHN_VERIFIER_ADDRESS,
  DELEGATE_VERIFIER_ADDRESS,
  INonceManagerAbi,
  IAccountConfigAbi,
  OwnerScope,
  hasScope,
} from "./constants.js";

export type {
  Call,
  Owner,
  ConfigOperation,
  CreateEntry,
  ConfigChangeEntry,
  AccountChangeEntry,
  SignedAuthorization,
  TxAa,
} from "./types.js";

export {
  OP_AUTHORIZE_OWNER,
  OP_REVOKE_OWNER,
  isEoa,
  isSelfPay,
  effectivePayer,
} from "./types.js";

export {
  rlpEncodeTxAa,
  encode2718,
  txHash,
  senderSigningPayload,
  payerSigningPayload,
  senderSignatureHash,
  payerSignatureHash,
} from "./rlp.js";

export {
  signAaTransaction,
  signAaTransactionP256,
  signAaTransactionWebAuthn,
  signAaTransactionAsPayer,
} from "./signing.js";
export type { WebAuthnAssertion } from "./signing.js";

export {
  sendAaTransaction,
  getAaNonce,
  getOwnerConfig,
  getAaNonceViaRpc,
} from "./actions.js";

export {
  deploymentHeader,
  deploymentCode,
  effectiveSalt,
  create2Address,
  deriveAccountAddress,
} from "./address.js";
