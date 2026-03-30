//! Base fee related utilities for Base chains.

use core::{cmp::max, fmt};

use alloy_consensus::BlockHeader;
use alloy_eips::calc_next_block_base_fee;
use base_alloy_chains::BaseUpgrades;

use crate::{BaseFeeParams, EthChainSpec};

const HOLOCENE_EXTRA_DATA_VERSION: u8 = 0;
const JOVIAN_EXTRA_DATA_VERSION: u8 = 1;

/// Error type for EIP-1559 parameter decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EIP1559ParamError {
    /// Thrown if the extra data begins with the wrong version byte.
    InvalidVersion(u8),
    /// No EIP-1559 parameters provided.
    NoEIP1559Params,
    /// Denominator overflow.
    DenominatorOverflow,
    /// Elasticity overflow.
    ElasticityOverflow,
    /// Extra data is not the correct length.
    InvalidExtraDataLength,
    /// Minimum base fee must be `None` before Jovian.
    MinBaseFeeMustBeNone,
    /// Minimum base fee cannot be `None` after Jovian.
    MinBaseFeeNotSet,
}

impl fmt::Display for EIP1559ParamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidVersion(version) => write!(f, "Invalid EIP1559 version byte: {version}"),
            Self::NoEIP1559Params => write!(f, "No EIP1559 parameters provided"),
            Self::DenominatorOverflow => write!(f, "Denominator overflow"),
            Self::ElasticityOverflow => write!(f, "Elasticity overflow"),
            Self::InvalidExtraDataLength => write!(f, "Extra data is not the correct length"),
            Self::MinBaseFeeMustBeNone => write!(f, "Minimum base fee must be None before Jovian"),
            Self::MinBaseFeeNotSet => write!(f, "Minimum base fee cannot be None after Jovian"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for EIP1559ParamError {}

fn decode_holocene_extra_data(extra_data: &[u8]) -> Result<(u32, u32), EIP1559ParamError> {
    if extra_data.len() != 9 {
        return Err(EIP1559ParamError::InvalidExtraDataLength);
    }
    if extra_data[0] != HOLOCENE_EXTRA_DATA_VERSION {
        return Err(EIP1559ParamError::InvalidVersion(extra_data[0]));
    }

    let denominator = u32::from_be_bytes(extra_data[1..5].try_into().expect("checked length"));
    let elasticity = u32::from_be_bytes(extra_data[5..9].try_into().expect("checked length"));
    Ok((elasticity, denominator))
}

fn decode_jovian_extra_data(extra_data: &[u8]) -> Result<(u32, u32, u64), EIP1559ParamError> {
    if extra_data.len() != 17 {
        return Err(EIP1559ParamError::InvalidExtraDataLength);
    }
    if extra_data[0] != JOVIAN_EXTRA_DATA_VERSION {
        return Err(EIP1559ParamError::InvalidVersion(extra_data[0]));
    }

    let denominator = u32::from_be_bytes(extra_data[1..5].try_into().expect("checked length"));
    let elasticity = u32::from_be_bytes(extra_data[5..9].try_into().expect("checked length"));
    let min_base_fee = u64::from_be_bytes(extra_data[9..17].try_into().expect("checked length"));
    Ok((elasticity, denominator, min_base_fee))
}

/// Extracts the Holocene 1559 parameters from the encoded parent extra data.
pub fn decode_holocene_base_fee<H>(
    chain_spec: impl EthChainSpec + BaseUpgrades,
    parent: &H,
    timestamp: u64,
) -> Result<u64, EIP1559ParamError>
where
    H: BlockHeader,
{
    let (elasticity, denominator) = decode_holocene_extra_data(parent.extra_data())?;

    let base_fee_params = if elasticity == 0 && denominator == 0 {
        chain_spec.base_fee_params_at_timestamp(timestamp)
    } else {
        BaseFeeParams::new(denominator as u128, elasticity as u128)
    };

    Ok(parent.next_block_base_fee(base_fee_params).unwrap_or_default())
}

/// Extracts the Jovian 1559 parameters from the encoded parent extra data.
pub fn compute_jovian_base_fee<H>(
    chain_spec: impl EthChainSpec + BaseUpgrades,
    parent: &H,
    timestamp: u64,
) -> Result<u64, EIP1559ParamError>
where
    H: BlockHeader,
{
    let (elasticity, denominator, min_base_fee) = decode_jovian_extra_data(parent.extra_data())?;

    let base_fee_params = if elasticity == 0 && denominator == 0 {
        chain_spec.base_fee_params_at_timestamp(timestamp)
    } else {
        BaseFeeParams::new(denominator as u128, elasticity as u128)
    };

    let gas_used = max(parent.gas_used(), parent.blob_gas_used().unwrap_or_default());
    let next_base_fee = calc_next_block_base_fee(
        gas_used,
        parent.gas_limit(),
        parent.base_fee_per_gas().unwrap_or_default(),
        base_fee_params,
    );

    if next_base_fee < min_base_fee {
        return Ok(min_base_fee);
    }

    Ok(next_base_fee)
}
