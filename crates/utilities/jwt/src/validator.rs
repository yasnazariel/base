//! JWT validation utilities.

use alloy_rpc_types_engine::JwtSecret;

#[cfg(feature = "engine-validation")]
use crate::JwtValidationError;

/// A JWT validator that can verify JWT secrets against an engine API.
#[derive(Debug, Clone, Copy)]
pub struct JwtValidator {
    secret: JwtSecret,
}

impl JwtValidator {
    /// Creates a new JWT validator with the given secret.
    pub const fn new(secret: JwtSecret) -> Self {
        Self { secret }
    }

    /// Returns the underlying JWT secret.
    pub const fn secret(&self) -> JwtSecret {
        self.secret
    }

    /// Consumes the validator and returns the JWT secret.
    pub const fn into_inner(self) -> JwtSecret {
        self.secret
    }

    /// Check if an error is related to JWT signature validation.
    ///
    /// Walks the error chain to detect JWT authentication failures by
    /// looking for common error message patterns.
    pub fn is_jwt_signature_error(error: &dyn std::error::Error) -> bool {
        let mut source = Some(error);
        while let Some(err) = source {
            let err_str = err.to_string().to_lowercase();
            if err_str.contains("signature invalid")
                || (err_str.contains("jwt") && err_str.contains("invalid"))
                || err_str.contains("unauthorized")
                || err_str.contains("authentication failed")
            {
                return true;
            }
            source = err.source();
        }
        false
    }

    /// Helper to check JWT signature error from `eyre::Error` (for retry condition).
    #[cfg(feature = "engine-validation")]
    pub fn is_jwt_signature_error_from_eyre(error: &eyre::Error) -> bool {
        Self::is_jwt_signature_error(error.as_ref() as &dyn std::error::Error)
    }
}

#[cfg(feature = "engine-validation")]
impl JwtValidator {
    /// Validates the JWT secret by exchanging capabilities with an engine API.
    ///
    /// Uses exponential backoff for transient failures, but fails immediately
    /// on authentication errors (invalid JWT signature).
    ///
    /// # Arguments
    /// * `engine_url` - The URL of the engine API endpoint. Supports HTTP(S), WS(S), and
    ///   `file://` URLs. This uses the same transport selection as the runtime engine client.
    ///   In the IPC case this checks engine reachability and capability exchange over the socket
    ///   path; JWT authentication is not exercised because IPC access is gated by filesystem
    ///   permissions instead.
    ///
    /// # Returns
    /// * `Ok(JwtSecret)` - The validated JWT secret
    /// * `Err(JwtValidationError::InvalidSignature)` - JWT authentication failed
    /// * `Err(JwtValidationError::CapabilityExchange(_))` - Transient error after retries
    pub async fn validate_with_engine(
        self,
        engine_url: url::Url,
    ) -> Result<JwtSecret, JwtValidationError> {
        use alloy_provider::RootProvider;
        use alloy_transport_http::Http;
        use backon::{ExponentialBuilder, Retryable};
        use base_common_network::Base;
        use base_common_provider::BaseEngineApi;
        use base_consensus_engine::{BaseEngineClient, HyperAuthClient};
        use tracing::{debug, error};

        let exchange = || async {
            // Recreate the client on every retry because WS and IPC transports connect eagerly.
            // This allows startup retries to cover the initial socket connect and handshake when
            // the engine listener is not ready yet.
            let engine = BaseEngineClient::<RootProvider, RootProvider<Base>>::rpc_client::<Base>(
                engine_url.clone(),
                self.secret,
            )
            .await
            .map_err(|e| eyre::eyre!(JwtValidationError::CapabilityExchange(e.to_string())))?;

            match <RootProvider<Base> as BaseEngineApi<
                Base,
                Http<HyperAuthClient>,
            >>::exchange_capabilities(&engine, vec![])
            .await {
                Ok(_) => {
                    debug!("Successfully exchanged capabilities with engine");
                    Ok(self.secret)
                }
                Err(e) => {
                    if Self::is_jwt_signature_error(&e) {
                        error!(
                            "Engine API JWT secret differs from the one specified by --l2.jwt-secret/--l2.jwt-secret-encoded"
                        );
                        error!(
                            "Ensure that the JWT secret file specified is correct (by default it is `jwt.hex` in the current directory)"
                        );
                        return Err(JwtValidationError::InvalidSignature.into());
                    }
                    Err(JwtValidationError::CapabilityExchange(e.to_string()).into())
                }
            }
        };

        exchange
            .retry(ExponentialBuilder::default())
            .when(|e: &eyre::Error| !Self::is_jwt_signature_error_from_eyre(e))
            .notify(|_, duration| {
                debug!(duration = ?duration, "Retrying engine capability handshake");
            })
            .await
            .map_err(|e| {
                // Convert eyre::Error back to JwtValidationError
                if Self::is_jwt_signature_error_from_eyre(&e) {
                    JwtValidationError::InvalidSignature
                } else {
                    JwtValidationError::CapabilityExchange(e.to_string())
                }
            })
    }
}
