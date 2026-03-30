//! Metadata provider trait for reading and writing node metadata.

use alloc::vec::Vec;

use reth_db_api::models::StorageSettings;
use reth_storage_errors::provider::ProviderResult;

/// Client trait for reading node metadata from the database.
#[auto_impl::auto_impl(&)]
pub trait MetadataProvider: Send {
    /// Get a metadata value by key
    fn get_metadata(&self, key: &str) -> ProviderResult<Option<Vec<u8>>>;
}

/// Client trait for writing node metadata to the database.
pub trait MetadataWriter: Send {
    /// Write a metadata value
    fn write_metadata(&self, key: &str, value: Vec<u8>) -> ProviderResult<()>;
}

/// Trait for caching storage settings on a provider factory.
pub trait StorageSettingsCache: Send {
    /// Gets the cached storage settings.
    fn cached_storage_settings(&self) -> StorageSettings;

    /// Sets the storage settings of this `ProviderFactory`.
    ///
    /// IMPORTANT: It does not save settings in storage, that should be done by
    /// [`MetadataWriter::write_storage_settings`]
    fn set_storage_settings_cache(&self, settings: StorageSettings);
}
