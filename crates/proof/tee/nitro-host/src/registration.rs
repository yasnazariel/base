//! Shared signer-validity checker backed by the on-chain `TEEProverRegistry`.
//!
//! Two consumers, two policies:
//! - **Health endpoint** — latching: once valid, stays healthy forever (avoids
//!   ASG replacement on transient L1 failures).
//! - **Proving guard** — fail-closed: rejects proof requests when the signer
//!   is invalid or L1 is unreachable.
//!
//! # Trade-off: latching health after deregistration
//!
//! After a signer deregistration or image rotation the health latch stays set
//! while the proving guard rejects every request.  The prover will continue
//! receiving traffic from the load balancer (because `/healthz` returns 200)
//! but respond with `-32001` errors.  This is intentional: the ASG must not
//! terminate the instance on a transient L1 blip, and proof-request callers
//! already retry on other nodes.  If `/healthz` is ever the **sole** LB
//! signal with no retry layer, switch to a bounded latch (e.g. stay healthy
//! for N minutes after the last successful validation).

use std::{sync::Arc, time::Duration};

use alloy_primitives::Address;
use alloy_signer::utils::public_key_to_address;
use base_proof_contracts::TEEProverRegistryClient;
use k256::ecdsa::VerifyingKey;
use thiserror::Error;
use tokio::sync::OnceCell;
use tracing::warn;

use super::transport::NitroTransport;

const CHECK_TIMEOUT: Duration = Duration::from_secs(10);

/// Errors from signer-validity checks.
#[derive(Debug, Error)]
pub enum RegistrationError {
    /// Enclave signer key could not be retrieved or parsed.
    #[error("signer setup failed: {0}")]
    Setup(String),
    /// L1 RPC call failed or timed out.
    #[error("L1 RPC failed for signer {signer}: {reason}")]
    Rpc {
        /// The signer address that was being checked.
        signer: Address,
        /// The underlying error message.
        reason: String,
    },
    /// The signer is not a valid signer in `TEEProverRegistry`.
    #[error("signer {signer} is not a valid signer in TEEProverRegistry")]
    NotValid {
        /// The signer address that failed validation.
        signer: Address,
    },
    #[error("no valid signer found among: {signers:?}")]
    NoValidSigner {
        signers: Vec<Address>,
    },
}

#[derive(Debug)]
pub struct ValidSigner {
    pub index: usize,
    pub signer: Address,
}

/// Checks whether the enclave signer is a **valid** signer in the on-chain
/// `TEEProverRegistry` (registered AND matching the current image hash).
///
/// Each call to [`require_valid_signer`](Self::require_valid_signer) performs a
/// live L1 query — proof requests are infrequent enough that caching is
/// unnecessary.  A separate latching flag tracks whether the signer has *ever*
/// been valid — once set, [`check_health`](Self::check_health) always succeeds.
pub struct RegistrationChecker {
    transports: Vec<Arc<NitroTransport>>,
    registry: Box<dyn TEEProverRegistryClient>,
    healthy: OnceCell<()>,
}

impl std::fmt::Debug for RegistrationChecker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistrationChecker").finish_non_exhaustive()
    }
}

impl RegistrationChecker {
    /// Creates a new checker for the given transport and registry client.
    pub fn new(
        transports: Vec<Arc<NitroTransport>>,
        registry: impl TEEProverRegistryClient + 'static,
    ) -> Self {
        Self { transports, registry: Box::new(registry), healthy: OnceCell::new() }
    }

    async fn signer_address(
        transport: &NitroTransport,
    ) -> Result<Address, RegistrationError> {
        let public_key = transport
            .signer_public_key()
            .await
            .map_err(|e| RegistrationError::Setup(format!("signer public key: {e}")))?;
        let verifying_key = VerifyingKey::from_sec1_bytes(&public_key)
            .map_err(|e| RegistrationError::Setup(format!("invalid public key: {e}")))?;
        Ok(public_key_to_address(&verifying_key))
    }

    async fn is_valid_signer(&self, signer: Address) -> Result<bool, RegistrationError> {
        let result =
            tokio::time::timeout(CHECK_TIMEOUT, self.registry.is_valid_signer(signer)).await;

        match result {
            Ok(Ok(valid)) => Ok(valid),
            Ok(Err(e)) => Err(RegistrationError::Rpc { signer, reason: e.to_string() }),
            Err(_) => Err(RegistrationError::Rpc { signer, reason: "request timed out".into() }),
        }
    }

    async fn fetch_validity(&self) -> Result<bool, RegistrationError> {
        let mut any_valid = false;
        let mut rpc_error = None;

        for (index, transport) in self.transports.iter().enumerate() {
            let signer = match Self::signer_address(transport).await {
                Ok(signer) => signer,
                Err(e) => {
                    warn!(error = %e, index, "skipping transport: key fetch failed");
                    continue;
                }
            };

            match self.is_valid_signer(signer).await {
                Ok(valid) => {
                    if valid {
                        any_valid = true;
                    } else {
                        warn!(signer = %signer, index, "signer is not a valid signer in TEEProverRegistry");
                    }
                }
                Err(e) => {
                    if rpc_error.is_none() {
                        rpc_error = Some(e);
                    }
                }
            }
        }

        if any_valid {
            return Ok(true);
        }

        if let Some(error) = rpc_error {
            return Err(error);
        }

        Ok(false)
    }

    /// Latching health check: returns `true` once the signer has ever been
    /// confirmed valid, and stays `true` forever after — even if the signer
    /// is later deregistered.  See the [module-level docs](self) for the
    /// trade-off this implies.
    pub async fn check_health(&self) -> Result<bool, RegistrationError> {
        if self.healthy.get().is_some() {
            return Ok(true);
        }
        let valid = self.fetch_validity().await?;
        if valid {
            let _ = self.healthy.set(());
        }
        Ok(valid)
    }

    /// Fails the request unless the signer is currently valid.
    ///
    /// Fail-closed: if L1 is unreachable or the signer is not valid, the
    /// proof request is rejected.
    pub async fn require_valid_signer(&self) -> Result<(), RegistrationError> {
        let transport = self
            .transports
            .first()
            .ok_or_else(|| RegistrationError::NoValidSigner { signers: Vec::new() })?;
        let signer = Self::signer_address(transport).await?;

        match self.is_valid_signer(signer).await {
            Ok(true) => Ok(()),
            Ok(false) => Err(RegistrationError::NotValid { signer }),
            Err(e) => Err(e),
        }
    }

    pub async fn select_valid_enclave(&self) -> Result<ValidSigner, RegistrationError> {
        let mut discovered = Vec::new();
        let mut valid_signers = Vec::new();

        for (index, transport) in self.transports.iter().enumerate() {
            let signer = match Self::signer_address(transport).await {
                Ok(signer) => signer,
                Err(e) => {
                    warn!(error = %e, index, "skipping transport: key fetch failed");
                    continue;
                }
            };

            discovered.push(signer);

            match self.is_valid_signer(signer).await {
                Ok(true) => valid_signers.push(ValidSigner { index, signer }),
                Ok(false) => {
                    warn!(signer = %signer, index, "signer is not a valid signer in TEEProverRegistry");
                }
                Err(e) => return Err(e),
            }
        }

        if valid_signers.is_empty() {
            return Err(RegistrationError::NoValidSigner { signers: discovered });
        }

        if valid_signers.len() > 1 {
            let signers: Vec<Address> =
                valid_signers.iter().map(|valid| valid.signer).collect();
            warn!(signers = ?signers, "multiple valid signers found; using first");
        }

        Ok(valid_signers.remove(0))
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::{Arc, atomic::Ordering},
    };

    use base_proof_contracts::TEEProverRegistryClient;

    use super::*;
    use crate::test_utils::{AddressBasedMockRegistry, MockRegistry};

    fn test_checker_with_mock(
        registry: impl TEEProverRegistryClient + 'static,
    ) -> RegistrationChecker {
        let server = Arc::new(base_proof_tee_nitro_enclave::Server::new_local().unwrap());
        let transport = Arc::new(NitroTransport::local(server));
        RegistrationChecker::new(vec![transport], registry)
    }

    fn test_checker() -> RegistrationChecker {
        let server = Arc::new(base_proof_tee_nitro_enclave::Server::new_local().unwrap());
        let transport = Arc::new(NitroTransport::local(server));
        let dummy_url = url::Url::parse("http://localhost:1").unwrap();
        let registry = base_proof_contracts::TEEProverRegistryContractClient::new(
            alloy_primitives::Address::ZERO,
            dummy_url,
        );
        RegistrationChecker::new(vec![transport], registry)
    }

    fn two_transport_checker(
        registry: impl TEEProverRegistryClient + 'static,
    ) -> RegistrationChecker {
        let s1 = Arc::new(base_proof_tee_nitro_enclave::Server::new_local().unwrap());
        let s2 = Arc::new(base_proof_tee_nitro_enclave::Server::new_local().unwrap());
        let t1 = Arc::new(NitroTransport::local(s1));
        let t2 = Arc::new(NitroTransport::local(s2));
        RegistrationChecker::new(vec![t1, t2], registry)
    }

    async fn transport_signer_address(transport: &NitroTransport) -> Address {
        let public_key = transport
            .signer_public_key()
            .await
            .expect("signer public key should be available in tests");
        let verifying_key = VerifyingKey::from_sec1_bytes(&public_key)
            .expect("signer public key should parse in tests");
        public_key_to_address(&verifying_key)
    }

    async fn two_transport_signers(checker: &RegistrationChecker) -> (Address, Address) {
        let signer_one = transport_signer_address(&checker.transports[0]).await;
        let signer_two = transport_signer_address(&checker.transports[1]).await;
        (signer_one, signer_two)
    }

    #[tokio::test]
    async fn health_returns_true_when_valid() {
        let checker = test_checker_with_mock(MockRegistry::new(true));
        assert!(checker.check_health().await.unwrap());
    }

    #[tokio::test]
    async fn health_returns_false_when_not_valid() {
        let checker = test_checker_with_mock(MockRegistry::new(false));
        assert!(!checker.check_health().await.unwrap());
    }

    #[tokio::test]
    async fn health_latches_after_first_success() {
        let registry = MockRegistry::new(true);
        let checker = test_checker_with_mock(registry.clone());

        assert!(checker.check_health().await.unwrap());

        registry.valid.store(false, Ordering::Relaxed);
        registry.should_fail.store(true, Ordering::Relaxed);

        assert!(checker.check_health().await.unwrap());
    }

    #[tokio::test]
    async fn health_errors_on_rpc_failure_before_latch() {
        let registry = MockRegistry::new(false);
        registry.should_fail.store(true, Ordering::Relaxed);
        let checker = test_checker_with_mock(registry);
        assert!(checker.check_health().await.is_err());
    }

    #[tokio::test]
    async fn health_ok_on_rpc_failure_after_latch() {
        let registry = MockRegistry::new(true);
        let checker = test_checker_with_mock(registry.clone());
        assert!(checker.check_health().await.unwrap());

        registry.should_fail.store(true, Ordering::Relaxed);
        assert!(checker.check_health().await.unwrap());
    }

    #[tokio::test]
    async fn require_valid_signer_ok() {
        let checker = test_checker_with_mock(MockRegistry::new(true));
        assert!(checker.require_valid_signer().await.is_ok());
    }

    #[tokio::test]
    async fn require_valid_signer_rejects_when_invalid() {
        let checker = test_checker_with_mock(MockRegistry::new(false));
        assert!(matches!(
            checker.require_valid_signer().await.unwrap_err(),
            RegistrationError::NotValid { .. }
        ));
    }

    #[tokio::test]
    async fn require_valid_signer_rejects_on_rpc_error() {
        let checker = test_checker();
        assert!(matches!(
            checker.require_valid_signer().await.unwrap_err(),
            RegistrationError::Rpc { .. }
        ));
    }

    #[tokio::test]
    async fn each_call_hits_registry() {
        let registry = MockRegistry::new(true);
        let call_count = Arc::clone(&registry.call_count);
        let checker = test_checker_with_mock(registry);

        assert!(checker.require_valid_signer().await.is_ok());
        assert_eq!(call_count.load(Ordering::Relaxed), 1);

        assert!(checker.require_valid_signer().await.is_ok());
        assert_eq!(call_count.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn select_single_valid_returns_it() {
        let registry = AddressBasedMockRegistry::new(HashMap::new());
        let checker = two_transport_checker(registry.clone());
        let (signer_one, signer_two) = two_transport_signers(&checker).await;

        {
            let mut validity_map = registry
                .validity_map
                .lock()
                .expect("validity map lock should succeed");
            validity_map.insert(signer_one, true);
            validity_map.insert(signer_two, false);
        }

        let valid = checker.select_valid_enclave().await.expect("valid signer expected");
        assert_eq!(valid.index, 0);
        assert_eq!(valid.signer, signer_one);
    }

    #[tokio::test]
    async fn select_second_valid_returns_index_1() {
        let registry = AddressBasedMockRegistry::new(HashMap::new());
        let checker = two_transport_checker(registry.clone());
        let (signer_one, signer_two) = two_transport_signers(&checker).await;

        {
            let mut validity_map = registry
                .validity_map
                .lock()
                .expect("validity map lock should succeed");
            validity_map.insert(signer_one, false);
            validity_map.insert(signer_two, true);
        }

        let valid = checker.select_valid_enclave().await.expect("valid signer expected");
        assert_eq!(valid.index, 1);
        assert_eq!(valid.signer, signer_two);
    }

    #[tokio::test]
    async fn select_both_valid_returns_first() {
        let registry = AddressBasedMockRegistry::new(HashMap::new());
        let checker = two_transport_checker(registry.clone());
        let (signer_one, signer_two) = two_transport_signers(&checker).await;

        {
            let mut validity_map = registry
                .validity_map
                .lock()
                .expect("validity map lock should succeed");
            validity_map.insert(signer_one, true);
            validity_map.insert(signer_two, true);
        }

        let valid = checker.select_valid_enclave().await.expect("valid signer expected");
        assert_eq!(valid.index, 0);
        assert_eq!(valid.signer, signer_one);
    }

    #[tokio::test]
    async fn select_none_valid_returns_no_valid_signer() {
        let registry = AddressBasedMockRegistry::new(HashMap::new());
        let checker = two_transport_checker(registry);
        let (signer_one, signer_two) = two_transport_signers(&checker).await;

        let err = checker
            .select_valid_enclave()
            .await
            .expect_err("expected no valid signer error");

        match err {
            RegistrationError::NoValidSigner { signers } => {
                assert_eq!(signers.len(), 2);
                assert!(signers.contains(&signer_one));
                assert!(signers.contains(&signer_two));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    // select_skips_failed_key_fetch is not directly testable because NitroTransport::local
    // always returns a signer public key and RegistrationChecker is hard-wired to NitroTransport.

    #[tokio::test]
    async fn health_latches_with_multi_transport() {
        let registry = AddressBasedMockRegistry::new(HashMap::new());
        let checker = two_transport_checker(registry.clone());
        let (signer_one, signer_two) = two_transport_signers(&checker).await;

        {
            let mut validity_map = registry
                .validity_map
                .lock()
                .expect("validity map lock should succeed");
            validity_map.insert(signer_one, true);
            validity_map.insert(signer_two, false);
        }

        assert!(checker.check_health().await.expect("health should be ok"));

        {
            let mut validity_map = registry
                .validity_map
                .lock()
                .expect("validity map lock should succeed");
            validity_map.insert(signer_one, false);
            validity_map.insert(signer_two, false);
        }

        registry.should_fail.store(true, Ordering::Relaxed);
        assert!(checker.check_health().await.expect("latched health should stay ok"));
    }
}
