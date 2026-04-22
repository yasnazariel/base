//! The zkvm ELF binaries.

/// Aggregation program ELF binary.
pub const AGGREGATION_ELF: &[u8] = include_bytes!("../../../elf/aggregation-elf");

/// Range program ELF binary.
pub const RANGE_ELF_EMBEDDED: &[u8] = include_bytes!("../../../elf/range-elf-embedded");
