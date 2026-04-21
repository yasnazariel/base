//! Loads or generates the secp256k1 signing key used by the EL bootnode.

use std::path::Path;

use reth_cli_util::{get_secret_key, load_secret_key::rng_secret_key};
use secp256k1::SecretKey;
use tracing::warn;

use crate::BootnodeResult;

/// Namespace for EL signing-key utilities.
#[derive(Debug, Clone, Copy)]
pub struct ElKeyLoader;

impl ElKeyLoader {
    /// Loads the configured secp256k1 secret key from `path` (generating and persisting one if
    /// the file does not exist), or generates an ephemeral key if `path` is `None`.
    ///
    /// Bootnode operators should always supply a path so that the advertised ENR is stable
    /// across restarts.
    pub fn load_or_generate(path: Option<&Path>) -> BootnodeResult<SecretKey> {
        match path {
            Some(path) => Ok(get_secret_key(path)?),
            None => {
                warn!(
                    target: "bootnode::el::key",
                    "no EL secret-key path supplied; generated ephemeral key (ENR will not be stable across restarts)"
                );
                Ok(rng_secret_key())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn ephemeral_when_path_is_none() {
        let key = ElKeyLoader::load_or_generate(None).expect("should generate ephemeral key");
        assert_eq!(key.secret_bytes().len(), 32);
    }

    #[test]
    fn persisted_key_is_stable_across_loads() {
        let dir = TempDir::new().expect("create tempdir");
        let path = dir.path().join("el-secret.key");

        let first = ElKeyLoader::load_or_generate(Some(&path)).expect("first load creates file");
        let second = ElKeyLoader::load_or_generate(Some(&path)).expect("second load reads file");

        assert_eq!(first.secret_bytes(), second.secret_bytes());
    }
}
