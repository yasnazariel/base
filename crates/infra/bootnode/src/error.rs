//! Error types for the bootnode crate.

use std::path::PathBuf;

use thiserror::Error;

/// Result alias used throughout this crate.
pub type BootnodeResult<T> = Result<T, BootnodeError>;

/// A boxed type-erased error used to carry source chains from foreign crates without having to
/// expose every internal error type.
type BoxedError = Box<dyn std::error::Error + Send + Sync>;

/// Errors that can occur while running an EL or CL bootnode.
#[derive(Debug, Error)]
pub enum BootnodeError {
    /// `Bootnode::run` was called with both halves disabled.
    #[error("bootnode has no EL or CL service configured")]
    NothingToRun,

    /// The advertised CL ENR would carry an unspecified IP (`0.0.0.0` or `::`), making it
    /// undialable by other peers. Operators must pass `--cl.advertise-ip` (or set
    /// `--cl.listen-ip` to a routable address) to opt out.
    #[error("CL advertise IP {ip} is unspecified; pass --cl.advertise-ip with a routable address")]
    UnroutableClAdvertiseIp {
        /// The unspecified IP that would have been advertised.
        ip: std::net::IpAddr,
    },

    /// Failed to load or generate the CL secret key.
    #[error("failed to load CL secret key from {path}")]
    ClSecretKey {
        /// The path that was attempted.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: base_consensus_peers::KeypairError,
    },

    /// The libp2p secp256k1 keypair could not be converted to a `k256` signing key.
    #[error("CL keypair could not be converted to a k256 signing key")]
    ClKeyConversion(#[source] BoxedError),

    /// Failed to parse a user-supplied bootnode string.
    #[error("failed to parse CL bootnode '{raw}'")]
    ClBootnodeParse {
        /// The raw bootnode string that failed to parse.
        raw: String,
        /// The underlying parser error.
        #[source]
        source: base_consensus_peers::BootNodeParseError,
    },

    /// The CL discovery service failed to build.
    #[error("CL discovery build failed: {0}")]
    ClBuild(#[from] base_consensus_disc::Discv5BuilderError),

    /// Failed to load or generate the EL secret key.
    #[error("failed to load EL secret key: {0}")]
    ElSecretKey(#[from] reth_cli_util::load_secret_key::SecretKeyError),

    /// The EL discv4 service failed to bind.
    #[error("EL discv4 bind failed: {0}")]
    ElDiscv4(#[source] std::io::Error),

    /// The EL discv5 service failed to start.
    #[error("EL discv5 start failed")]
    ElDiscv5(#[source] BoxedError),

    /// A spawned bootnode task failed to join cleanly.
    #[error("bootnode task panicked: {0}")]
    TaskJoin(#[from] tokio::task::JoinError),
}
