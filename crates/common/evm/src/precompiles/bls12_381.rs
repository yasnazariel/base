use revm::precompile::{
    self as precompile, Precompile, PrecompileHalt, PrecompileId, PrecompileOutput,
    PrecompileResult,
    bls12_381_const::{G1_MSM_ADDRESS, G2_MSM_ADDRESS, PAIRING_ADDRESS},
};

/// Max input size for the BLS12-381 G1 MSM precompile after the Isthmus hardfork.
pub const ISTHMUS_G1_MSM_MAX_INPUT_SIZE: usize = 513760;
/// Max input size for the BLS12-381 G1 MSM precompile after the Jovian hardfork.
pub const JOVIAN_G1_MSM_MAX_INPUT_SIZE: usize = 288_960;

/// Max input size for the BLS12-381 G2 MSM precompile after the Isthmus hardfork.
pub const ISTHMUS_G2_MSM_MAX_INPUT_SIZE: usize = 488448;
/// Max input size for the BLS12-381 G2 MSM precompile after the Jovian hardfork.
pub const JOVIAN_G2_MSM_MAX_INPUT_SIZE: usize = 278_784;

/// Max input size for the BLS12-381 pairing precompile after the Isthmus hardfork.
pub const ISTHMUS_PAIRING_MAX_INPUT_SIZE: usize = 235008;
/// Max input size for the BLS12-381 pairing precompile after the Jovian hardfork.
pub const JOVIAN_PAIRING_MAX_INPUT_SIZE: usize = 156_672;

/// BLS12-381 G1 MSM precompile with Isthmus input limits.
pub const ISTHMUS_G1_MSM: Precompile =
    Precompile::new(PrecompileId::Bls12G1Msm, G1_MSM_ADDRESS, run_g1_msm_isthmus);
/// BLS12-381 G2 MSM precompile with Isthmus input limits.
pub const ISTHMUS_G2_MSM: Precompile =
    Precompile::new(PrecompileId::Bls12G2Msm, G2_MSM_ADDRESS, run_g2_msm_isthmus);
/// BLS12-381 pairing precompile with Isthmus input limits.
pub const ISTHMUS_PAIRING: Precompile =
    Precompile::new(PrecompileId::Bls12Pairing, PAIRING_ADDRESS, run_pairing_isthmus);

/// BLS12-381 G1 MSM precompile with Jovian input limits.
pub const JOVIAN_G1_MSM: Precompile =
    Precompile::new(PrecompileId::Bls12G1Msm, G1_MSM_ADDRESS, run_g1_msm_jovian);
/// BLS12-381 G2 MSM precompile with Jovian input limits.
pub const JOVIAN_G2_MSM: Precompile =
    Precompile::new(PrecompileId::Bls12G2Msm, G2_MSM_ADDRESS, run_g2_msm_jovian);
/// BLS12-381 pairing precompile with Jovian input limits.
pub const JOVIAN_PAIRING: Precompile =
    Precompile::new(PrecompileId::Bls12Pairing, PAIRING_ADDRESS, run_pairing_jovian);

/// Run the BLS12-381 G1 MSM precompile with Isthmus input limit.
pub fn run_g1_msm_isthmus(input: &[u8], gas_limit: u64, reservoir: u64) -> PrecompileResult {
    if input.len() > ISTHMUS_G1_MSM_MAX_INPUT_SIZE {
        return Ok(PrecompileOutput::halt(PrecompileHalt::Bls12381G1MsmInputLength, reservoir));
    }
    let result = precompile::bls12_381::g1_msm::g1_msm(input, gas_limit);
    Ok(PrecompileOutput::from_eth_result(result, reservoir))
}

/// Run the BLS12-381 G1 MSM precompile with Jovian input limit.
pub fn run_g1_msm_jovian(input: &[u8], gas_limit: u64, reservoir: u64) -> PrecompileResult {
    if input.len() > JOVIAN_G1_MSM_MAX_INPUT_SIZE {
        return Ok(PrecompileOutput::halt(PrecompileHalt::Bls12381G1MsmInputLength, reservoir));
    }
    let result = precompile::bls12_381::g1_msm::g1_msm(input, gas_limit);
    Ok(PrecompileOutput::from_eth_result(result, reservoir))
}

/// Run the BLS12-381 G2 MSM precompile with Isthmus input limit.
pub fn run_g2_msm_isthmus(input: &[u8], gas_limit: u64, reservoir: u64) -> PrecompileResult {
    if input.len() > ISTHMUS_G2_MSM_MAX_INPUT_SIZE {
        return Ok(PrecompileOutput::halt(PrecompileHalt::Bls12381G2MsmInputLength, reservoir));
    }
    let result = precompile::bls12_381::g2_msm::g2_msm(input, gas_limit);
    Ok(PrecompileOutput::from_eth_result(result, reservoir))
}

/// Run the BLS12-381 G2 MSM precompile with Jovian input limit.
pub fn run_g2_msm_jovian(input: &[u8], gas_limit: u64, reservoir: u64) -> PrecompileResult {
    if input.len() > JOVIAN_G2_MSM_MAX_INPUT_SIZE {
        return Ok(PrecompileOutput::halt(PrecompileHalt::Bls12381G2MsmInputLength, reservoir));
    }
    let result = precompile::bls12_381::g2_msm::g2_msm(input, gas_limit);
    Ok(PrecompileOutput::from_eth_result(result, reservoir))
}

/// Run the BLS12-381 pairing precompile with Isthmus input limit.
pub fn run_pairing_isthmus(input: &[u8], gas_limit: u64, reservoir: u64) -> PrecompileResult {
    if input.len() > ISTHMUS_PAIRING_MAX_INPUT_SIZE {
        return Ok(PrecompileOutput::halt(PrecompileHalt::Bls12381PairingInputLength, reservoir));
    }
    let result = precompile::bls12_381::pairing::pairing(input, gas_limit);
    Ok(PrecompileOutput::from_eth_result(result, reservoir))
}

/// Run the BLS12-381 pairing precompile with Jovian input limit.
pub fn run_pairing_jovian(input: &[u8], gas_limit: u64, reservoir: u64) -> PrecompileResult {
    if input.len() > JOVIAN_PAIRING_MAX_INPUT_SIZE {
        return Ok(PrecompileOutput::halt(PrecompileHalt::Bls12381PairingInputLength, reservoir));
    }
    let result = precompile::bls12_381::pairing::pairing(input, gas_limit);
    Ok(PrecompileOutput::from_eth_result(result, reservoir))
}

#[cfg(test)]
mod tests {
    use revm::{precompile::PrecompileHalt, primitives::Bytes};

    use super::*;

    #[test]
    fn test_g1_msm_isthmus_max_size() {
        let input = Bytes::from(vec![0u8; ISTHMUS_G1_MSM_MAX_INPUT_SIZE + 1]);
        assert!(matches!(
            run_g1_msm_isthmus(&input, 260_000, 0),
            Ok(output) if matches!(output.halt_reason(), Some(PrecompileHalt::Bls12381G1MsmInputLength))
        ));
    }

    #[test]
    fn test_g1_msm_jovian_max_size() {
        let input = Bytes::from(vec![0u8; JOVIAN_G1_MSM_MAX_INPUT_SIZE + 1]);
        assert!(matches!(
            run_g1_msm_jovian(&input, u64::MAX, 0),
            Ok(output) if matches!(output.halt_reason(), Some(PrecompileHalt::Bls12381G1MsmInputLength))
        ));
    }

    #[test]
    fn test_g2_msm_isthmus_max_size() {
        let input = Bytes::from(vec![0u8; ISTHMUS_G2_MSM_MAX_INPUT_SIZE + 1]);
        assert!(matches!(
            run_g2_msm_isthmus(&input, 260_000, 0),
            Ok(output) if matches!(output.halt_reason(), Some(PrecompileHalt::Bls12381G2MsmInputLength))
        ));
    }

    #[test]
    fn test_g2_msm_jovian_max_size() {
        let input = Bytes::from(vec![0u8; JOVIAN_G2_MSM_MAX_INPUT_SIZE + 1]);
        assert!(matches!(
            run_g2_msm_jovian(&input, u64::MAX, 0),
            Ok(output) if matches!(output.halt_reason(), Some(PrecompileHalt::Bls12381G2MsmInputLength))
        ));
    }

    #[test]
    fn test_pairing_isthmus_max_size() {
        let input = Bytes::from(vec![0u8; ISTHMUS_PAIRING_MAX_INPUT_SIZE + 1]);
        assert!(matches!(
            run_pairing_isthmus(&input, 260_000, 0),
            Ok(output) if matches!(output.halt_reason(), Some(PrecompileHalt::Bls12381PairingInputLength))
        ));
    }

    #[test]
    fn test_pairing_jovian_max_size() {
        let input = Bytes::from(vec![0u8; JOVIAN_PAIRING_MAX_INPUT_SIZE + 1]);
        assert!(matches!(
            run_pairing_jovian(&input, u64::MAX, 0),
            Ok(output) if matches!(output.halt_reason(), Some(PrecompileHalt::Bls12381PairingInputLength))
        ));
    }
}
