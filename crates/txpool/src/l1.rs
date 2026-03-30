use alloy_consensus::Transaction;
use alloy_primitives::{U16, U256, hex};
use base_alloy_chains::BaseUpgrades;
use reth_revm::{L1BlockInfo, OpSpecId};

/// The function selector of `setL1BlockValuesEcotone`.
const L1_BLOCK_ECOTONE_SELECTOR: [u8; 4] = hex!("440a5e20");
/// The function selector of `setL1BlockValuesIsthmus`.
const L1_BLOCK_ISTHMUS_SELECTOR: [u8; 4] = hex!("098999be");
/// The function selector of `setL1BlockValuesJovian`.
const L1_BLOCK_JOVIAN_SELECTOR: [u8; 4] = hex!("3db6be2b");

pub(crate) fn extract_l1_info_from_tx<T: Transaction>(tx: &T) -> Option<L1BlockInfo> {
    let input = tx.input();
    if input.len() < 4 {
        return None;
    }
    parse_l1_info(input)
}

fn parse_l1_info(input: &[u8]) -> Option<L1BlockInfo> {
    if input[0..4] == L1_BLOCK_JOVIAN_SELECTOR {
        parse_l1_info_tx_jovian(&input[4..])
    } else if input[0..4] == L1_BLOCK_ISTHMUS_SELECTOR {
        parse_l1_info_tx_isthmus(&input[4..])
    } else if input[0..4] == L1_BLOCK_ECOTONE_SELECTOR {
        parse_l1_info_tx_ecotone(&input[4..])
    } else {
        parse_l1_info_tx_bedrock(&input[4..])
    }
}

fn parse_l1_info_tx_bedrock(data: &[u8]) -> Option<L1BlockInfo> {
    if data.len() != 256 {
        return None;
    }

    let l1_base_fee = U256::try_from_be_slice(&data[64..96])?;
    let l1_fee_overhead = U256::try_from_be_slice(&data[192..224])?;
    let l1_fee_scalar = U256::try_from_be_slice(&data[224..256])?;

    Some(L1BlockInfo {
        l1_base_fee,
        l1_fee_overhead: Some(l1_fee_overhead),
        l1_base_fee_scalar: l1_fee_scalar,
        ..Default::default()
    })
}

fn parse_l1_info_tx_ecotone(data: &[u8]) -> Option<L1BlockInfo> {
    if data.len() != 160 {
        return None;
    }

    let l1_base_fee_scalar = U256::try_from_be_slice(&data[..4])?;
    let l1_blob_base_fee_scalar = U256::try_from_be_slice(&data[4..8])?;
    let l1_base_fee = U256::try_from_be_slice(&data[32..64])?;
    let l1_blob_base_fee = U256::try_from_be_slice(&data[64..96])?;

    Some(L1BlockInfo {
        l1_base_fee,
        l1_base_fee_scalar,
        l1_blob_base_fee: Some(l1_blob_base_fee),
        l1_blob_base_fee_scalar: Some(l1_blob_base_fee_scalar),
        ..Default::default()
    })
}

fn parse_l1_info_tx_isthmus(data: &[u8]) -> Option<L1BlockInfo> {
    if data.len() != 172 {
        return None;
    }

    let l1_base_fee_scalar = U256::try_from_be_slice(&data[..4])?;
    let l1_blob_base_fee_scalar = U256::try_from_be_slice(&data[4..8])?;
    let l1_base_fee = U256::try_from_be_slice(&data[32..64])?;
    let l1_blob_base_fee = U256::try_from_be_slice(&data[64..96])?;
    let operator_fee_scalar = U256::try_from_be_slice(&data[160..164])?;
    let operator_fee_constant = U256::try_from_be_slice(&data[164..172])?;

    Some(L1BlockInfo {
        l1_base_fee,
        l1_base_fee_scalar,
        l1_blob_base_fee: Some(l1_blob_base_fee),
        l1_blob_base_fee_scalar: Some(l1_blob_base_fee_scalar),
        operator_fee_scalar: Some(operator_fee_scalar),
        operator_fee_constant: Some(operator_fee_constant),
        ..Default::default()
    })
}

fn parse_l1_info_tx_jovian(data: &[u8]) -> Option<L1BlockInfo> {
    if data.len() != 174 {
        return None;
    }

    let l1_base_fee_scalar = U256::try_from_be_slice(&data[..4])?;
    let l1_blob_base_fee_scalar = U256::try_from_be_slice(&data[4..8])?;
    let l1_base_fee = U256::try_from_be_slice(&data[32..64])?;
    let l1_blob_base_fee = U256::try_from_be_slice(&data[64..96])?;
    let operator_fee_scalar = U256::try_from_be_slice(&data[160..164])?;
    let operator_fee_constant = U256::try_from_be_slice(&data[164..172])?;
    let da_footprint_gas_scalar: u16 = U16::try_from_be_slice(&data[172..174])?.to();

    Some(L1BlockInfo {
        l1_base_fee,
        l1_base_fee_scalar,
        l1_blob_base_fee: Some(l1_blob_base_fee),
        l1_blob_base_fee_scalar: Some(l1_blob_base_fee_scalar),
        operator_fee_scalar: Some(operator_fee_scalar),
        operator_fee_constant: Some(operator_fee_constant),
        da_footprint_gas_scalar: Some(da_footprint_gas_scalar),
        ..Default::default()
    })
}

fn op_spec_id(chain_spec: &impl BaseUpgrades, timestamp: u64) -> OpSpecId {
    if chain_spec.is_base_v1_active_at_timestamp(timestamp) {
        OpSpecId::BASE_V1
    } else if chain_spec.is_jovian_active_at_timestamp(timestamp) {
        OpSpecId::JOVIAN
    } else if chain_spec.is_isthmus_active_at_timestamp(timestamp) {
        OpSpecId::ISTHMUS
    } else if chain_spec.is_holocene_active_at_timestamp(timestamp) {
        OpSpecId::HOLOCENE
    } else if chain_spec.is_granite_active_at_timestamp(timestamp) {
        OpSpecId::GRANITE
    } else if chain_spec.is_fjord_active_at_timestamp(timestamp) {
        OpSpecId::FJORD
    } else if chain_spec.is_ecotone_active_at_timestamp(timestamp) {
        OpSpecId::ECOTONE
    } else if chain_spec.is_canyon_active_at_timestamp(timestamp) {
        OpSpecId::CANYON
    } else if chain_spec.is_regolith_active_at_timestamp(timestamp) {
        OpSpecId::REGOLITH
    } else {
        OpSpecId::BEDROCK
    }
}

pub(crate) trait RethL1BlockInfoExt {
    fn l1_tx_data_fee(
        &mut self,
        chain_spec: impl BaseUpgrades,
        timestamp: u64,
        input: &[u8],
        is_deposit: bool,
    ) -> U256;
}

impl RethL1BlockInfoExt for L1BlockInfo {
    fn l1_tx_data_fee(
        &mut self,
        chain_spec: impl BaseUpgrades,
        timestamp: u64,
        input: &[u8],
        is_deposit: bool,
    ) -> U256 {
        if is_deposit {
            return U256::ZERO;
        }

        self.calculate_tx_l1_cost(input, op_spec_id(&chain_spec, timestamp))
    }
}
