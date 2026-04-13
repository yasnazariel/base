//! Error types for engine operations.

mod build;
pub use build::{BuildTaskError, EngineBuildError};

mod consolidate;
pub use consolidate::ConsolidateTaskError;

mod finalize;
pub use finalize::FinalizeTaskError;

mod insert;
pub use insert::InsertTaskError;

mod reset;
pub use reset::EngineResetError;

mod seal;
pub use seal::SealTaskError;

mod synchronize;
pub use synchronize::SynchronizeTaskError;

mod task;
pub use task::{EngineTaskError, EngineTaskErrorSeverity};

mod consolidate_input;
pub use consolidate_input::ConsolidateInput;
