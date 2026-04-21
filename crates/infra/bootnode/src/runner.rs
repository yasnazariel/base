//! Top-level bootnode orchestrator that runs any subset of `{EL, CL}` halves.

use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::{BootnodeError, BootnodeResult, ClBootnode, ElBootnode};

/// Identifies which half of the bootnode produced a given outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootnodeSide {
    /// The execution-layer (reth) bootnode.
    El,
    /// The consensus-layer (`base-consensus-disc`) bootnode.
    Cl,
}

impl BootnodeSide {
    /// Returns the static log target / display name for this side.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::El => "el",
            Self::Cl => "cl",
        }
    }
}

/// Composes any subset of `{EL, CL}` bootnodes and runs them under a shared
/// [`CancellationToken`].
///
/// When both halves are configured and one returns, the other is signalled to shut down via its
/// child token, then awaited so its outcome is captured rather than silently dropped. The first
/// non-`Ok` result observed is returned; if both succeed the orchestrator returns `Ok(())`.
///
/// Calling [`Bootnode::run`] with no halves configured returns [`BootnodeError::NothingToRun`].
#[derive(Debug, Default)]
pub struct Bootnode {
    /// The execution-layer bootnode, if enabled.
    el: Option<ElBootnode>,
    /// The consensus-layer bootnode, if enabled.
    cl: Option<ClBootnode>,
}

impl Bootnode {
    /// Creates an empty bootnode orchestrator.
    pub const fn new() -> Self {
        Self { el: None, cl: None }
    }

    /// Adds the execution-layer bootnode to the orchestrator.
    pub fn with_el(mut self, el: ElBootnode) -> Self {
        self.el = Some(el);
        self
    }

    /// Adds the consensus-layer bootnode to the orchestrator.
    pub fn with_cl(mut self, cl: ClBootnode) -> Self {
        self.cl = Some(cl);
        self
    }

    /// Returns `true` if at least one half is configured.
    pub const fn has_any(&self) -> bool {
        self.el.is_some() || self.cl.is_some()
    }

    /// Runs all configured bootnode halves until `cancel` is triggered or one of them fails.
    pub async fn run(self, cancel: CancellationToken) -> BootnodeResult<()> {
        match (self.el, self.cl) {
            (None, None) => Err(BootnodeError::NothingToRun),
            (Some(el), None) => el.run(cancel).await,
            (None, Some(cl)) => cl.run(cancel).await,
            (Some(el), Some(cl)) => Self::run_both(el, cl, cancel).await,
        }
    }

    pub async fn run_both(
        el: ElBootnode,
        cl: ClBootnode,
        cancel: CancellationToken,
    ) -> BootnodeResult<()> {
        let el_cancel = cancel.child_token();
        let cl_cancel = cancel.child_token();

        let mut set: JoinSet<(BootnodeSide, BootnodeResult<()>)> = JoinSet::new();
        let el_token = el_cancel.clone();
        set.spawn(async move { (BootnodeSide::El, el.run(el_token).await) });
        let cl_token = cl_cancel.clone();
        set.spawn(async move { (BootnodeSide::Cl, cl.run(cl_token).await) });

        let mut first_error: Option<BootnodeError> = None;
        let mut first_completed = false;

        while let Some(joined) = set.join_next().await {
            let (side, result) = match joined {
                Ok(sr) => sr,
                Err(join_err) => {
                    el_cancel.cancel();
                    cl_cancel.cancel();
                    while set.join_next().await.is_some() {}
                    return Err(BootnodeError::TaskJoin(join_err));
                }
            };
            info!(target: "bootnode", side = side.as_str(), "bootnode half exited");

            if !first_completed {
                first_completed = true;
                match side {
                    BootnodeSide::El => cl_cancel.cancel(),
                    BootnodeSide::Cl => el_cancel.cancel(),
                }
            }

            if let Err(err) = result
                && first_error.is_none()
            {
                first_error = Some(err);
            }
        }

        cancel.cancel();
        first_error.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn run_without_halves_errors() {
        let cancel = CancellationToken::new();
        let err = Bootnode::new().run(cancel).await.unwrap_err();
        assert!(matches!(err, BootnodeError::NothingToRun));
    }

    #[test]
    fn has_any_reflects_configuration() {
        assert!(!Bootnode::new().has_any());
    }

    #[test]
    fn side_as_str_matches_log_target() {
        assert_eq!(BootnodeSide::El.as_str(), "el");
        assert_eq!(BootnodeSide::Cl.as_str(), "cl");
    }
}
