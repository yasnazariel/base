//! Loads or generates the secp256k1 signing key used by the CL bootnode.

use std::path::Path;

use base_consensus_peers::SecretKeyLoader;
use discv5::enr::k256;
use libp2p_identity::Keypair;
use tracing::warn;

use crate::{BootnodeError, BootnodeResult};

/// Namespace for CL signing-key utilities.
#[derive(Debug, Clone, Copy)]
pub struct ClKeyLoader;

impl ClKeyLoader {
    /// Loads a libp2p keypair from `path` (generating and persisting one if the
    /// file does not exist), or generates an ephemeral keypair if `path` is
    /// `None`. Returns the corresponding [`k256::ecdsa::SigningKey`] usable
    /// with [`base_consensus_disc::LocalNode`].
    ///
    /// Bootnode operators should always supply a path so that the advertised
    /// ENR is stable across restarts.
    pub fn load_or_generate(path: Option<&Path>) -> BootnodeResult<k256::ecdsa::SigningKey> {
        let keypair = match path {
            Some(path) => SecretKeyLoader::load(path).map_err(|source| {
                BootnodeError::ClSecretKey { path: path.to_path_buf(), source }
            })?,
            None => {
                warn!(
                    target: "bootnode::cl::key",
                    "no CL secret-key path supplied; generated ephemeral keypair (ENR will not be stable across restarts)"
                );
                Keypair::generate_secp256k1()
            }
        };

        let bytes = keypair
            .try_into_secp256k1()
            .map_err(|e| BootnodeError::ClKeyConversion(Box::new(e)))?
            .secret()
            .to_bytes();

        k256::ecdsa::SigningKey::from_bytes(&bytes.into())
            .map_err(|e| BootnodeError::ClKeyConversion(Box::new(e)))
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn ephemeral_when_path_is_none() {
        let key = ClKeyLoader::load_or_generate(None).expect("should generate ephemeral key");
        assert_eq!(key.to_bytes().len(), 32);
    }

    #[test]
    fn persisted_key_is_stable_across_loads() {
        let dir = TempDir::new().expect("create tempdir");
        let path = dir.path().join("cl-secret.key");

        let first = ClKeyLoader::load_or_generate(Some(&path)).expect("first load creates file");
        let second = ClKeyLoader::load_or_generate(Some(&path)).expect("second load reads file");

        assert_eq!(first.to_bytes(), second.to_bytes());
    }
}
