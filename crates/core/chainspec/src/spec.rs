use alloc::{
    boxed::Box,
    collections::BTreeMap,
    string::{String, ToString},
    sync::Arc,
    vec,
    vec::Vec,
};
use core::fmt::Debug;

use alloy_chains::Chain;
use alloy_consensus::{Header, constants::EMPTY_WITHDRAWALS, proofs::storage_root_unhashed};
pub use alloy_eips::eip1559::BaseFeeParams;
use alloy_eips::{
    eip1559::INITIAL_BASE_FEE, eip7685::EMPTY_REQUESTS_HASH, eip7840::BlobParams,
    eip7892::BlobScheduleBlobParams,
};
use alloy_evm::eth::spec::EthExecutorSpec;
use alloy_genesis::{ChainConfig, Genesis};
use alloy_hardforks::Hardfork;
use alloy_primitives::{Address, B256, BlockNumber, U256};
use alloy_serde::OtherFields;
use alloy_trie::root::state_root_ref_unhashed;
use base_alloy_chains::{BaseChainConfig, BaseChainUpgrades, BaseUpgrade, BaseUpgrades};
use derive_more::From;
use reth_ethereum_forks::{
    BaseChainUpgradesExt, ChainHardforks, DisplayHardforks, EthereumHardfork, EthereumHardforks,
    ForkCondition, ForkFilter, ForkFilterKey, ForkHash, ForkId, Hardforks, Head,
};
use reth_network_peers::NodeRecord;
use reth_primitives_traits::{SealedHeader, sync::LazyLock};
use serde::Deserialize;

use crate::{
    EthChainSpec,
    constants::{L2_TO_L1_MESSAGE_PASSER, MAINNET_PRUNE_DELETE_LIMIT},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BaseFeeSchedule {
    Ethereum,
    Configured,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GenesisHeaderSeal {
    KnownHash,
    Compute,
}

#[derive(Debug, Clone, Copy)]
struct BuiltInBaseSpec {
    config: &'static BaseChainConfig,
    chain: Chain,
    prune_delete_limit: usize,
    base_fee_schedule: BaseFeeSchedule,
    genesis_header_seal: GenesisHeaderSeal,
}

fn configured_base_fee_params(config: &BaseChainConfig) -> BaseFeeParamsKind {
    BaseFeeParamsKind::Variable(
        vec![
            (
                EthereumHardfork::London.boxed(),
                BaseFeeParams::new(
                    config.eip1559_denominator as u128,
                    config.eip1559_elasticity as u128,
                ),
            ),
            (
                BaseUpgrade::Canyon.boxed(),
                BaseFeeParams::new(
                    config.eip1559_denominator_canyon as u128,
                    config.eip1559_elasticity as u128,
                ),
            ),
        ]
        .into(),
    )
}

fn built_in_hardforks(config: &BaseChainConfig) -> ChainHardforks {
    BaseChainUpgrades::from_config(config).to_chain_hardforks()
}

fn build_builtin_spec(def: BuiltInBaseSpec) -> Arc<ChainSpec> {
    let BuiltInBaseSpec {
        config,
        chain,
        prune_delete_limit,
        base_fee_schedule,
        genesis_header_seal,
    } = def;

    let genesis: Genesis = serde_json::from_str(config.genesis_json)
        .unwrap_or_else(|_| panic!("Can't deserialize {chain} genesis json"));
    let hardforks = built_in_hardforks(config);
    let blob_params = genesis.config.blob_schedule_blob_params();
    let genesis_header = match genesis_header_seal {
        GenesisHeaderSeal::KnownHash => {
            SealedHeader::new(make_genesis_header(&genesis, &hardforks), config.genesis_l2_hash)
        }
        GenesisHeaderSeal::Compute => {
            SealedHeader::seal_slow(make_genesis_header(&genesis, &hardforks))
        }
    };
    let base_fee_params = match base_fee_schedule {
        BaseFeeSchedule::Ethereum => BaseFeeParams::ethereum().into(),
        BaseFeeSchedule::Configured => configured_base_fee_params(config),
    };

    Arc::new(ChainSpec {
        chain,
        genesis_header,
        genesis,
        paris_block_and_final_difficulty: Some((0, U256::ZERO)),
        hardforks,
        deposit_contract: None,
        base_fee_params,
        prune_delete_limit,
        blob_params,
    })
}

/// The Base mainnet spec.
pub static BASE_MAINNET: LazyLock<Arc<ChainSpec>> = LazyLock::new(|| {
    build_builtin_spec(BuiltInBaseSpec {
        config: BaseChainConfig::mainnet(),
        chain: Chain::base_mainnet(),
        prune_delete_limit: 20_000,
        base_fee_schedule: BaseFeeSchedule::Configured,
        genesis_header_seal: GenesisHeaderSeal::KnownHash,
    })
});

/// The Base Sepolia spec.
pub static BASE_SEPOLIA: LazyLock<Arc<ChainSpec>> = LazyLock::new(|| {
    build_builtin_spec(BuiltInBaseSpec {
        config: BaseChainConfig::sepolia(),
        chain: Chain::base_sepolia(),
        prune_delete_limit: 10_000,
        base_fee_schedule: BaseFeeSchedule::Configured,
        genesis_header_seal: GenesisHeaderSeal::KnownHash,
    })
});

/// The Base devnet-0-sepolia-dev-0 spec.
pub static BASE_DEVNET_0_SEPOLIA_DEV_0: LazyLock<Arc<ChainSpec>> = LazyLock::new(|| {
    build_builtin_spec(BuiltInBaseSpec {
        config: BaseChainConfig::alpha(),
        chain: Chain::from_id(BaseChainConfig::alpha().chain_id),
        prune_delete_limit: 10_000,
        base_fee_schedule: BaseFeeSchedule::Configured,
        genesis_header_seal: GenesisHeaderSeal::KnownHash,
    })
});

/// The Base Zeronet spec.
pub static BASE_ZERONET: LazyLock<Arc<ChainSpec>> = LazyLock::new(|| {
    build_builtin_spec(BuiltInBaseSpec {
        config: BaseChainConfig::zeronet(),
        chain: Chain::from_id(BaseChainConfig::zeronet().chain_id),
        prune_delete_limit: 10_000,
        base_fee_schedule: BaseFeeSchedule::Configured,
        genesis_header_seal: GenesisHeaderSeal::KnownHash,
    })
});

/// Base dev testnet specification.
pub static BASE_DEV: LazyLock<Arc<ChainSpec>> = LazyLock::new(|| {
    build_builtin_spec(BuiltInBaseSpec {
        config: BaseChainConfig::devnet(),
        chain: Chain::dev(),
        prune_delete_limit: 20_000,
        base_fee_schedule: BaseFeeSchedule::Ethereum,
        genesis_header_seal: GenesisHeaderSeal::Compute,
    })
});

/// All supported chain names for the CLI.
pub const SUPPORTED_CHAINS: &[&str] =
    &["base", "base_sepolia", "base-sepolia", "base-devnet-0-sepolia-dev-0", "base-zeronet", "dev"];

/// Container type for Base-specific fields embedded in genesis `extra_fields`.
#[derive(Default, Debug, Clone, Copy, Eq, PartialEq)]
pub struct OpChainInfo {
    /// Genesis hardfork activation data.
    pub genesis_info: Option<OpChainGenesisInfo>,
    /// Base fee configuration data.
    pub base_fee_info: Option<OpBaseFeeInfo>,
}

impl OpChainInfo {
    /// Extracts all Base-specific fields from a genesis file.
    pub fn extract_from(others: &OtherFields) -> Self {
        Self {
            genesis_info: OpChainGenesisInfo::extract_from(others),
            base_fee_info: OpBaseFeeInfo::extract_from(others),
        }
    }
}

/// Base-specific nested hardfork fields in genesis `extra_fields`.
#[derive(Default, Debug, Clone, Copy, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpBaseHardforkInfo {
    /// Base V1 hardfork timestamp.
    pub v1: Option<u64>,
}

/// Base-specific genesis hardfork activation data.
#[derive(Default, Debug, Clone, Copy, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpChainGenesisInfo {
    /// Bedrock block number.
    pub bedrock_block: Option<u64>,
    /// Regolith activation timestamp.
    pub regolith_time: Option<u64>,
    /// Canyon activation timestamp.
    pub canyon_time: Option<u64>,
    /// Ecotone activation timestamp.
    pub ecotone_time: Option<u64>,
    /// Fjord activation timestamp.
    pub fjord_time: Option<u64>,
    /// Granite activation timestamp.
    pub granite_time: Option<u64>,
    /// Holocene activation timestamp.
    pub holocene_time: Option<u64>,
    /// Isthmus activation timestamp.
    pub isthmus_time: Option<u64>,
    /// Jovian activation timestamp.
    pub jovian_time: Option<u64>,
    /// Additional Base-specific activation data.
    #[serde(default)]
    pub base: OpBaseHardforkInfo,
}

impl OpChainGenesisInfo {
    /// Extracts Base genesis activation fields from a genesis file.
    pub fn extract_from(others: &OtherFields) -> Option<Self> {
        others.deserialize_as().ok()
    }
}

/// Base-specific EIP-1559 configuration embedded in genesis `extra_fields`.
#[derive(Default, Debug, Clone, Copy, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpBaseFeeInfo {
    /// EIP-1559 elasticity.
    pub eip1559_elasticity: Option<u64>,
    /// EIP-1559 denominator.
    pub eip1559_denominator: Option<u64>,
    /// EIP-1559 denominator after Canyon.
    pub eip1559_denominator_canyon: Option<u64>,
}

impl OpBaseFeeInfo {
    /// Extracts Base base fee configuration from the `optimism` key in genesis `extra_fields`.
    pub fn extract_from(others: &OtherFields) -> Option<Self> {
        others.get_deserialized::<Self>("optimism").and_then(Result::ok)
    }
}

/// Genesis info extracted from a Base genesis config.
#[derive(Default, Debug)]
pub struct OpGenesisInfo {
    /// Base chain info extracted from genesis extra fields.
    pub optimism_chain_info: OpChainInfo,
    /// Base fee params derived from the genesis config.
    pub base_fee_params: BaseFeeParamsKind,
}

impl OpGenesisInfo {
    /// Extracts Base genesis info from a [`Genesis`].
    pub fn extract_from(genesis: &Genesis) -> Self {
        let mut info = Self {
            optimism_chain_info: OpChainInfo::extract_from(&genesis.config.extra_fields),
            ..Default::default()
        };

        if let Some(optimism_base_fee_info) = &info.optimism_chain_info.base_fee_info
            && let (Some(elasticity), Some(denominator)) = (
                optimism_base_fee_info.eip1559_elasticity,
                optimism_base_fee_info.eip1559_denominator,
            )
        {
            info.base_fee_params = optimism_base_fee_info.eip1559_denominator_canyon.map_or_else(
                || BaseFeeParams::new(denominator as u128, elasticity as u128).into(),
                |canyon_denominator| {
                    BaseFeeParamsKind::Variable(
                        vec![
                            (
                                EthereumHardfork::London.boxed(),
                                BaseFeeParams::new(denominator as u128, elasticity as u128),
                            ),
                            (
                                BaseUpgrade::Canyon.boxed(),
                                BaseFeeParams::new(canyon_denominator as u128, elasticity as u128),
                            ),
                        ]
                        .into(),
                    )
                },
            );
        }

        info
    }
}

/// Helper method building a [`Header`] given [`Genesis`] and [`ChainHardforks`].
pub fn make_genesis_header(genesis: &Genesis, hardforks: &ChainHardforks) -> Header {
    let base_fee_per_gas = hardforks
        .fork(EthereumHardfork::London)
        .active_at_block(0)
        .then(|| genesis.base_fee_per_gas.map(|fee| fee as u64).unwrap_or(INITIAL_BASE_FEE));

    let withdrawals_root = hardforks
        .fork(EthereumHardfork::Shanghai)
        .active_at_timestamp(genesis.timestamp)
        .then_some(EMPTY_WITHDRAWALS);

    let (parent_beacon_block_root, blob_gas_used, excess_blob_gas) =
        if hardforks.fork(EthereumHardfork::Cancun).active_at_timestamp(genesis.timestamp) {
            let blob_gas_used = genesis.blob_gas_used.unwrap_or(0);
            let excess_blob_gas = genesis.excess_blob_gas.unwrap_or(0);
            (Some(B256::ZERO), Some(blob_gas_used), Some(excess_blob_gas))
        } else {
            (None, None, None)
        };

    let requests_hash = hardforks
        .fork(EthereumHardfork::Prague)
        .active_at_timestamp(genesis.timestamp)
        .then_some(EMPTY_REQUESTS_HASH);

    let mut header = Header {
        number: genesis.number.unwrap_or_default(),
        parent_hash: genesis.parent_hash.unwrap_or_default(),
        gas_limit: genesis.gas_limit,
        difficulty: genesis.difficulty,
        nonce: genesis.nonce.into(),
        extra_data: genesis.extra_data.clone(),
        state_root: state_root_ref_unhashed(&genesis.alloc),
        timestamp: genesis.timestamp,
        mix_hash: genesis.mix_hash,
        beneficiary: genesis.coinbase,
        base_fee_per_gas,
        withdrawals_root,
        parent_beacon_block_root,
        blob_gas_used,
        excess_blob_gas,
        requests_hash,
        ..Default::default()
    };

    if hardforks.fork(BaseUpgrade::Isthmus).active_at_timestamp(header.timestamp)
        && let Some(predeploy) = genesis.alloc.get(&L2_TO_L1_MESSAGE_PASSER)
        && let Some(storage) = &predeploy.storage
    {
        header.withdrawals_root = Some(storage_root_unhashed(storage.iter().filter_map(
            |(k, v)| {
                if v.is_zero() { None } else { Some((*k, (*v).into())) }
            },
        )));
    }

    header
}

/// Creates a [`ChainConfig`] from the given chain, hardforks, deposit contract address, and blob
/// schedule.
pub fn create_chain_config(
    chain: Option<Chain>,
    hardforks: &ChainHardforks,
    deposit_contract_address: Option<Address>,
    blob_schedule: BTreeMap<String, BlobParams>,
) -> ChainConfig {
    let block_num = |fork: EthereumHardfork| hardforks.fork(fork).block_number();
    let timestamp = |fork: EthereumHardfork| -> Option<u64> {
        match hardforks.fork(fork) {
            ForkCondition::Timestamp(t) => Some(t),
            _ => None,
        }
    };
    let (terminal_total_difficulty, terminal_total_difficulty_passed) =
        match hardforks.fork(EthereumHardfork::Paris) {
            ForkCondition::TTD { total_difficulty, .. } => (Some(total_difficulty), true),
            _ => (None, false),
        };

    #[allow(clippy::needless_update)]
    ChainConfig {
        chain_id: chain.map(|c| c.id()).unwrap_or(0),
        homestead_block: block_num(EthereumHardfork::Homestead),
        dao_fork_block: None,
        dao_fork_support: false,
        eip150_block: block_num(EthereumHardfork::Tangerine),
        eip155_block: block_num(EthereumHardfork::SpuriousDragon),
        eip158_block: block_num(EthereumHardfork::SpuriousDragon),
        byzantium_block: block_num(EthereumHardfork::Byzantium),
        constantinople_block: block_num(EthereumHardfork::Constantinople),
        petersburg_block: block_num(EthereumHardfork::Petersburg),
        istanbul_block: block_num(EthereumHardfork::Istanbul),
        muir_glacier_block: block_num(EthereumHardfork::MuirGlacier),
        berlin_block: block_num(EthereumHardfork::Berlin),
        london_block: block_num(EthereumHardfork::London),
        arrow_glacier_block: block_num(EthereumHardfork::ArrowGlacier),
        gray_glacier_block: block_num(EthereumHardfork::GrayGlacier),
        merge_netsplit_block: None,
        shanghai_time: timestamp(EthereumHardfork::Shanghai),
        cancun_time: timestamp(EthereumHardfork::Cancun),
        prague_time: timestamp(EthereumHardfork::Prague),
        osaka_time: timestamp(EthereumHardfork::Osaka),
        bpo1_time: timestamp(EthereumHardfork::Bpo1),
        bpo2_time: timestamp(EthereumHardfork::Bpo2),
        bpo3_time: timestamp(EthereumHardfork::Bpo3),
        bpo4_time: timestamp(EthereumHardfork::Bpo4),
        bpo5_time: timestamp(EthereumHardfork::Bpo5),
        terminal_total_difficulty,
        terminal_total_difficulty_passed,
        ethash: None,
        clique: None,
        parlia: None,
        extra_fields: Default::default(),
        deposit_contract_address,
        blob_schedule,
        ..Default::default()
    }
}

/// Converts the given [`BlobScheduleBlobParams`] into blob schedule entries.
pub fn blob_params_to_schedule(
    params: &BlobScheduleBlobParams,
    hardforks: &ChainHardforks,
) -> BTreeMap<String, BlobParams> {
    let mut schedule = BTreeMap::new();
    schedule.insert("cancun".to_string(), params.cancun);
    schedule.insert("prague".to_string(), params.prague);
    schedule.insert("osaka".to_string(), params.osaka);

    for (timestamp, blob_params) in &params.scheduled {
        for bpo_fork in EthereumHardfork::bpo_variants() {
            if let ForkCondition::Timestamp(fork_ts) = hardforks.fork(bpo_fork)
                && fork_ts == *timestamp
            {
                schedule.insert(bpo_fork.name().to_lowercase(), *blob_params);
                break;
            }
        }
    }

    schedule
}

/// A wrapper around [`BaseFeeParams`] that allows for specifying constant or dynamic EIP-1559
/// parameters based on the active hardfork.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BaseFeeParamsKind {
    /// Constant base fee params.
    Constant(BaseFeeParams),
    /// Variable base fee params selected by hardfork activation order.
    Variable(ForkBaseFeeParams),
}

impl Default for BaseFeeParamsKind {
    fn default() -> Self {
        BaseFeeParams::ethereum().into()
    }
}

impl From<BaseFeeParams> for BaseFeeParamsKind {
    fn from(params: BaseFeeParams) -> Self {
        Self::Constant(params)
    }
}

impl From<ForkBaseFeeParams> for BaseFeeParamsKind {
    fn from(params: ForkBaseFeeParams) -> Self {
        Self::Variable(params)
    }
}

/// Base fee params indexed by hardfork activation order.
#[derive(Clone, Debug, PartialEq, Eq, From)]
pub struct ForkBaseFeeParams(Vec<(Box<dyn Hardfork>, BaseFeeParams)>);

/// A Base chain specification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainSpec {
    /// The chain ID.
    pub chain: Chain,
    /// The genesis block.
    pub genesis: Genesis,
    /// The sealed header corresponding to the genesis block.
    pub genesis_header: SealedHeader<Header>,
    /// The block at which Paris was activated and the final difficulty at that block.
    pub paris_block_and_final_difficulty: Option<(u64, U256)>,
    /// Active hardforks and activation conditions.
    pub hardforks: ChainHardforks,
    /// Deposit contract metadata if configured.
    pub deposit_contract: Option<DepositContract>,
    /// Parameters that configure next-block base fee computation.
    pub base_fee_params: BaseFeeParamsKind,
    /// The delete limit for pruner, per run.
    pub prune_delete_limit: usize,
    /// Blob parameter settings.
    pub blob_params: BlobScheduleBlobParams,
}

impl Default for ChainSpec {
    fn default() -> Self {
        BASE_MAINNET.as_ref().clone()
    }
}

impl core::ops::Deref for ChainSpec {
    type Target = ChainHardforks;

    fn deref(&self) -> &Self::Target {
        &self.hardforks
    }
}

impl ChainSpec {
    /// Converts the given [`Genesis`] into a [`ChainSpec`].
    pub fn from_genesis(genesis: Genesis) -> Self {
        genesis.into()
    }

    /// Builds a chain spec using [`ChainSpecBuilder`].
    pub fn builder() -> ChainSpecBuilder {
        ChainSpecBuilder::default()
    }

    /// Map a chain ID to a known chain spec, if available.
    pub fn from_chain_id(chain_id: u64) -> Option<Arc<Self>> {
        if chain_id == Chain::dev().id() {
            return Some(BASE_DEV.clone());
        }

        let config = BaseChainConfig::by_chain_id(chain_id)?;
        Some(match config.chain_id {
            id if id == BaseChainConfig::mainnet().chain_id => BASE_MAINNET.clone(),
            id if id == BaseChainConfig::sepolia().chain_id => BASE_SEPOLIA.clone(),
            id if id == BaseChainConfig::alpha().chain_id => BASE_DEVNET_0_SEPOLIA_DEV_0.clone(),
            id if id == BaseChainConfig::zeronet().chain_id => BASE_ZERONET.clone(),
            _ => return None,
        })
    }

    /// Parses a chain name into a builtin chainspec, if recognized.
    pub fn parse_chain(s: &str) -> Option<Arc<Self>> {
        match s {
            "dev" => Some(BASE_DEV.clone()),
            "base" => Some(BASE_MAINNET.clone()),
            "base_sepolia" | "base-sepolia" => Some(BASE_SEPOLIA.clone()),
            "base-devnet-0-sepolia-dev-0" => Some(BASE_DEVNET_0_SEPOLIA_DEV_0.clone()),
            "base-zeronet" => Some(BASE_ZERONET.clone()),
            _ => None,
        }
    }

    /// Activates or updates the given hardfork condition in-place.
    pub fn set_fork<H: Hardfork>(&mut self, fork: H, condition: ForkCondition) {
        self.hardforks.insert(fork, condition);
    }

    /// Sets the sealed genesis hash in-place.
    pub fn set_genesis_hash(&mut self, hash: B256) {
        self.genesis_header = SealedHeader::new(self.genesis_header.clone_header(), hash);
    }

    /// Get information about the chain itself.
    pub const fn chain(&self) -> Chain {
        self.chain
    }

    /// Returns `true` if this chain contains Ethereum configuration.
    #[inline]
    pub const fn is_ethereum(&self) -> bool {
        self.chain.is_ethereum()
    }

    /// Returns `true` if this chain is Base mainnet.
    #[inline]
    pub fn is_optimism_mainnet(&self) -> bool {
        self.chain == Chain::base_mainnet()
    }

    /// Returns the known Paris block, if it exists.
    #[inline]
    pub fn paris_block(&self) -> Option<u64> {
        self.paris_block_and_final_difficulty.map(|(block, _)| block)
    }

    /// Returns the genesis block specification.
    pub const fn genesis(&self) -> &Genesis {
        &self.genesis
    }

    /// Returns the genesis header.
    pub fn genesis_header(&self) -> &Header {
        &self.genesis_header
    }

    /// Returns the sealed genesis header.
    pub fn sealed_genesis_header(&self) -> SealedHeader<Header> {
        SealedHeader::new(self.genesis_header().clone(), self.genesis_hash())
    }

    /// Returns the initial base fee of the genesis block.
    pub fn initial_base_fee(&self) -> Option<u64> {
        let genesis_base_fee =
            self.genesis.base_fee_per_gas.map(|fee| fee as u64).unwrap_or(INITIAL_BASE_FEE);

        self.hardforks.fork(EthereumHardfork::London).active_at_block(0).then_some(genesis_base_fee)
    }

    /// Returns the active [`BaseFeeParams`] at the given timestamp.
    pub fn base_fee_params_at_timestamp(&self, timestamp: u64) -> BaseFeeParams {
        match self.base_fee_params {
            BaseFeeParamsKind::Constant(bf_params) => bf_params,
            BaseFeeParamsKind::Variable(ForkBaseFeeParams(ref bf_params)) => {
                for (fork, params) in bf_params.iter().rev() {
                    if self.hardforks.is_fork_active_at_timestamp(fork.clone(), timestamp) {
                        return *params;
                    }
                }

                bf_params.first().map(|(_, params)| *params).unwrap_or_else(BaseFeeParams::ethereum)
            }
        }
    }

    /// Returns the hash of the genesis block.
    pub fn genesis_hash(&self) -> B256 {
        self.genesis_header.hash()
    }

    /// Returns the genesis timestamp.
    pub const fn genesis_timestamp(&self) -> u64 {
        self.genesis.timestamp
    }

    /// Returns the final total difficulty if Paris is known.
    pub fn get_final_paris_total_difficulty(&self) -> Option<U256> {
        self.paris_block_and_final_difficulty.map(|(_, final_difficulty)| final_difficulty)
    }

    /// Returns the fork filter for the given hardfork.
    pub fn hardfork_fork_filter<HF: Hardfork + Clone>(&self, fork: HF) -> Option<ForkFilter> {
        match self.hardforks.fork(fork.clone()) {
            ForkCondition::Never => None,
            _ => Some(self.fork_filter(self.satisfy(self.hardforks.fork(fork)))),
        }
    }

    /// Returns the hardfork display helper.
    pub fn display_hardforks(&self) -> DisplayHardforks {
        let op_forks = self.hardforks.forks_iter().filter(|(fork, _)| {
            !EthereumHardfork::VARIANTS.iter().any(|h| h.name() == (*fork).name())
        });
        DisplayHardforks::new(op_forks)
    }

    /// Returns the fork ID for the given hardfork.
    pub fn hardfork_fork_id<HF: Hardfork + Clone>(&self, fork: HF) -> Option<ForkId> {
        let condition = self.hardforks.fork(fork);
        match condition {
            ForkCondition::Never => None,
            _ => Some(self.fork_id(&self.satisfy(condition))),
        }
    }

    /// Convenience method to get the Shanghai fork ID.
    pub fn shanghai_fork_id(&self) -> Option<ForkId> {
        self.hardfork_fork_id(EthereumHardfork::Shanghai)
    }

    /// Convenience method to get the Cancun fork ID.
    pub fn cancun_fork_id(&self) -> Option<ForkId> {
        self.hardfork_fork_id(EthereumHardfork::Cancun)
    }

    /// Convenience method to get the latest fork ID from the chainspec.
    pub fn latest_fork_id(&self) -> ForkId {
        self.hardfork_fork_id(self.hardforks.last().unwrap().0).unwrap()
    }

    /// Creates a [`ForkFilter`] for the block described by [Head].
    pub fn fork_filter(&self, head: Head) -> ForkFilter {
        let forks = self.hardforks.forks_iter().filter_map(|(_, condition)| {
            Some(match condition {
                ForkCondition::Block(block)
                | ForkCondition::TTD { fork_block: Some(block), .. } => ForkFilterKey::Block(block),
                ForkCondition::Timestamp(time) => ForkFilterKey::Time(time),
                _ => return None,
            })
        });

        ForkFilter::new(head, self.genesis_hash(), self.genesis_timestamp(), forks)
    }

    /// Compute the [`ForkId`] for the given [`Head`] following EIP-6122.
    pub fn fork_id(&self, head: &Head) -> ForkId {
        let mut forkhash = ForkHash::from(self.genesis_hash());
        let mut current_applied = 0;

        for (_, cond) in self.hardforks.forks_iter() {
            if let ForkCondition::Block(block)
            | ForkCondition::TTD { fork_block: Some(block), .. } = cond
            {
                if head.number >= block {
                    if block != current_applied {
                        forkhash += block;
                        current_applied = block;
                    }
                } else {
                    return ForkId { hash: forkhash, next: block };
                }
            }
        }

        for timestamp in self.hardforks.forks_iter().filter_map(|(_, cond)| {
            cond.as_timestamp().filter(|time| time > &self.genesis.timestamp)
        }) {
            if head.timestamp >= timestamp {
                if timestamp != current_applied {
                    forkhash += timestamp;
                    current_applied = timestamp;
                }
            } else {
                return ForkId { hash: forkhash, next: timestamp };
            }
        }

        ForkId { hash: forkhash, next: 0 }
    }

    pub(crate) fn satisfy(&self, cond: ForkCondition) -> Head {
        match cond {
            ForkCondition::Block(number) => Head { number, ..Default::default() },
            ForkCondition::Timestamp(timestamp) => Head {
                timestamp,
                number: self.last_block_fork_before_merge_or_timestamp().unwrap_or_default(),
                ..Default::default()
            },
            ForkCondition::TTD { total_difficulty, fork_block, .. } => Head {
                total_difficulty,
                number: fork_block.unwrap_or_default(),
                ..Default::default()
            },
            ForkCondition::Never => unreachable!(),
        }
    }

    pub(crate) fn last_block_fork_before_merge_or_timestamp(&self) -> Option<u64> {
        let mut hardforks_iter = self.hardforks.forks_iter().peekable();
        while let Some((_, curr_cond)) = hardforks_iter.next() {
            if let Some((_, next_cond)) = hardforks_iter.peek() {
                match next_cond {
                    ForkCondition::TTD { fork_block: Some(block), .. } => return Some(*block),
                    ForkCondition::TTD { .. } | ForkCondition::Timestamp(_) => {
                        if let ForkCondition::Block(block_num) = curr_cond {
                            return Some(block_num);
                        }
                    }
                    ForkCondition::Block(_) | ForkCondition::Never => {}
                }
            }
        }
        None
    }

    /// Returns the known bootnode records for the given chain.
    pub fn bootnodes(&self) -> Option<Vec<NodeRecord>> {
        let config = if self.chain == Chain::dev() {
            BaseChainConfig::devnet()
        } else {
            BaseChainConfig::by_chain_id(self.chain.id())?
        };
        let bootnodes: Vec<_> =
            config.bootnodes.iter().filter_map(|node| node.parse().ok()).collect();
        (!bootnodes.is_empty()).then_some(bootnodes)
    }
}

impl From<Genesis> for ChainSpec {
    fn from(genesis: Genesis) -> Self {
        let optimism_genesis_info = OpGenesisInfo::extract_from(&genesis);
        let genesis_info =
            optimism_genesis_info.optimism_chain_info.genesis_info.unwrap_or_default();

        let hardfork_opts = [
            (EthereumHardfork::Frontier.boxed(), Some(0)),
            (EthereumHardfork::Homestead.boxed(), genesis.config.homestead_block),
            (EthereumHardfork::Tangerine.boxed(), genesis.config.eip150_block),
            (EthereumHardfork::SpuriousDragon.boxed(), genesis.config.eip155_block),
            (EthereumHardfork::Byzantium.boxed(), genesis.config.byzantium_block),
            (EthereumHardfork::Constantinople.boxed(), genesis.config.constantinople_block),
            (EthereumHardfork::Petersburg.boxed(), genesis.config.petersburg_block),
            (EthereumHardfork::Istanbul.boxed(), genesis.config.istanbul_block),
            (EthereumHardfork::MuirGlacier.boxed(), genesis.config.muir_glacier_block),
            (EthereumHardfork::Berlin.boxed(), genesis.config.berlin_block),
            (EthereumHardfork::London.boxed(), genesis.config.london_block),
            (EthereumHardfork::ArrowGlacier.boxed(), genesis.config.arrow_glacier_block),
            (EthereumHardfork::GrayGlacier.boxed(), genesis.config.gray_glacier_block),
            (BaseUpgrade::Bedrock.boxed(), genesis_info.bedrock_block),
        ];
        let mut block_hardforks = hardfork_opts
            .into_iter()
            .filter_map(|(hardfork, opt)| opt.map(|block| (hardfork, ForkCondition::Block(block))))
            .collect::<Vec<_>>();

        block_hardforks.push((
            EthereumHardfork::Paris.boxed(),
            ForkCondition::TTD {
                activation_block_number: 0,
                total_difficulty: U256::ZERO,
                fork_block: genesis.config.merge_netsplit_block,
            },
        ));

        let base_v1_time = genesis_info.base.v1;
        let time_hardfork_opts = [
            (BaseUpgrade::Regolith.boxed(), genesis_info.regolith_time),
            (EthereumHardfork::Shanghai.boxed(), genesis_info.canyon_time),
            (BaseUpgrade::Canyon.boxed(), genesis_info.canyon_time),
            (EthereumHardfork::Cancun.boxed(), genesis_info.ecotone_time),
            (BaseUpgrade::Ecotone.boxed(), genesis_info.ecotone_time),
            (BaseUpgrade::Fjord.boxed(), genesis_info.fjord_time),
            (BaseUpgrade::Granite.boxed(), genesis_info.granite_time),
            (BaseUpgrade::Holocene.boxed(), genesis_info.holocene_time),
            (EthereumHardfork::Prague.boxed(), genesis_info.isthmus_time),
            (BaseUpgrade::Isthmus.boxed(), genesis_info.isthmus_time),
            (BaseUpgrade::Jovian.boxed(), genesis_info.jovian_time),
            (EthereumHardfork::Osaka.boxed(), base_v1_time),
            (BaseUpgrade::V1.boxed(), base_v1_time),
        ];
        let mut time_hardforks = time_hardfork_opts
            .into_iter()
            .filter_map(|(hardfork, opt)| {
                opt.map(|time| (hardfork, ForkCondition::Timestamp(time)))
            })
            .collect::<Vec<_>>();

        block_hardforks.append(&mut time_hardforks);

        let mainnet_hardforks = built_in_hardforks(BaseChainConfig::mainnet());
        let mut ordered_hardforks = Vec::with_capacity(block_hardforks.len());
        for (hardfork, _) in mainnet_hardforks.forks_iter() {
            if let Some(pos) =
                block_hardforks.iter().position(|(candidate, _)| **candidate == *hardfork)
            {
                ordered_hardforks.push(block_hardforks.remove(pos));
            }
        }
        ordered_hardforks.append(&mut block_hardforks);

        let hardforks = ChainHardforks::new(ordered_hardforks);
        let blob_params = genesis.config.blob_schedule_blob_params();
        let genesis_header = SealedHeader::seal_slow(make_genesis_header(&genesis, &hardforks));

        Self {
            chain: genesis.config.chain_id.into(),
            genesis_header,
            genesis,
            paris_block_and_final_difficulty: Some((0, U256::ZERO)),
            hardforks,
            deposit_contract: None,
            base_fee_params: optimism_genesis_info.base_fee_params,
            prune_delete_limit: MAINNET_PRUNE_DELETE_LIMIT,
            blob_params,
        }
    }
}

impl Hardforks for ChainSpec {
    fn fork<HF: Hardfork>(&self, fork: HF) -> ForkCondition {
        self.hardforks.fork(fork)
    }

    fn forks_iter(&self) -> impl Iterator<Item = (&dyn Hardfork, ForkCondition)> {
        self.hardforks.forks_iter()
    }

    fn fork_id(&self, head: &Head) -> ForkId {
        self.fork_id(head)
    }

    fn latest_fork_id(&self) -> ForkId {
        self.latest_fork_id()
    }

    fn fork_filter(&self, head: Head) -> ForkFilter {
        self.fork_filter(head)
    }
}

impl EthereumHardforks for ChainSpec {
    fn ethereum_fork_activation(&self, fork: EthereumHardfork) -> ForkCondition {
        self.fork(fork)
    }
}

impl BaseUpgrades for ChainSpec {
    fn upgrade_activation(&self, fork: BaseUpgrade) -> ForkCondition {
        self.fork(fork)
    }
}

/// A trait for reading the current chainspec.
#[auto_impl::auto_impl(&, Arc)]
pub trait ChainSpecProvider: Debug + Send {
    /// The chain spec type.
    type ChainSpec: EthChainSpec + 'static;

    /// Get an [`Arc`] to the chainspec.
    fn chain_spec(&self) -> Arc<Self::ChainSpec>;
}

/// A helper to build custom chain specs.
#[derive(Debug, Default, Clone)]
pub struct ChainSpecBuilder {
    chain: Option<Chain>,
    genesis: Option<Genesis>,
    hardforks: ChainHardforks,
    deposit_contract: Option<DepositContract>,
    base_fee_params: Option<BaseFeeParamsKind>,
    prune_delete_limit: Option<usize>,
    blob_params: Option<BlobScheduleBlobParams>,
}

impl ChainSpecBuilder {
    /// Construct a new builder from the Base mainnet chain spec.
    pub fn base_mainnet() -> Self {
        Self::from(&*BASE_MAINNET)
    }

    /// Construct a new builder from the Base mainnet chain spec.
    pub fn mainnet() -> Self {
        Self::base_mainnet()
    }

    /// Set the chain ID.
    pub const fn chain(mut self, chain: Chain) -> Self {
        self.chain = Some(chain);
        self
    }

    /// Resets any existing hardforks from the builder.
    pub fn reset(mut self) -> Self {
        self.hardforks = ChainHardforks::default();
        self
    }

    /// Set the genesis block.
    pub fn genesis(mut self, genesis: Genesis) -> Self {
        self.genesis = Some(genesis);
        self
    }

    /// Add the given fork with the given activation condition to the spec.
    pub fn with_fork<H: Hardfork>(mut self, fork: H, condition: ForkCondition) -> Self {
        self.hardforks.insert(fork, condition);
        self
    }

    /// Add the given chain hardforks to the spec.
    pub fn with_forks(mut self, forks: ChainHardforks) -> Self {
        self.hardforks = forks;
        self
    }

    /// Remove the given fork from the spec.
    pub fn without_fork<H: Hardfork>(mut self, fork: H) -> Self {
        self.hardforks.remove(&fork);
        self
    }

    /// Enable the Paris hardfork at the given TTD.
    pub fn paris_at_ttd(self, ttd: U256, activation_block_number: BlockNumber) -> Self {
        self.with_fork(
            EthereumHardfork::Paris,
            ForkCondition::TTD { activation_block_number, total_difficulty: ttd, fork_block: None },
        )
    }

    /// Enable Frontier at genesis.
    pub fn frontier_activated(mut self) -> Self {
        self.hardforks.insert(EthereumHardfork::Frontier, ForkCondition::Block(0));
        self
    }

    /// Enable Dao at genesis.
    pub fn dao_activated(mut self) -> Self {
        self = self.frontier_activated();
        self.hardforks.insert(EthereumHardfork::Dao, ForkCondition::Block(0));
        self
    }

    /// Enable Homestead at genesis.
    pub fn homestead_activated(mut self) -> Self {
        self = self.dao_activated();
        self.hardforks.insert(EthereumHardfork::Homestead, ForkCondition::Block(0));
        self
    }

    /// Enable Tangerine at genesis.
    pub fn tangerine_whistle_activated(mut self) -> Self {
        self = self.homestead_activated();
        self.hardforks.insert(EthereumHardfork::Tangerine, ForkCondition::Block(0));
        self
    }

    /// Enable Spurious Dragon at genesis.
    pub fn spurious_dragon_activated(mut self) -> Self {
        self = self.tangerine_whistle_activated();
        self.hardforks.insert(EthereumHardfork::SpuriousDragon, ForkCondition::Block(0));
        self
    }

    /// Enable Byzantium at genesis.
    pub fn byzantium_activated(mut self) -> Self {
        self = self.spurious_dragon_activated();
        self.hardforks.insert(EthereumHardfork::Byzantium, ForkCondition::Block(0));
        self
    }

    /// Enable Constantinople at genesis.
    pub fn constantinople_activated(mut self) -> Self {
        self = self.byzantium_activated();
        self.hardforks.insert(EthereumHardfork::Constantinople, ForkCondition::Block(0));
        self
    }

    /// Enable Petersburg at genesis.
    pub fn petersburg_activated(mut self) -> Self {
        self = self.constantinople_activated();
        self.hardforks.insert(EthereumHardfork::Petersburg, ForkCondition::Block(0));
        self
    }

    /// Enable Istanbul at genesis.
    pub fn istanbul_activated(mut self) -> Self {
        self = self.petersburg_activated();
        self.hardforks.insert(EthereumHardfork::Istanbul, ForkCondition::Block(0));
        self
    }

    /// Enable Muir Glacier at genesis.
    pub fn muirglacier_activated(mut self) -> Self {
        self = self.istanbul_activated();
        self.hardforks.insert(EthereumHardfork::MuirGlacier, ForkCondition::Block(0));
        self
    }

    /// Enable Berlin at genesis.
    pub fn berlin_activated(mut self) -> Self {
        self = self.muirglacier_activated();
        self.hardforks.insert(EthereumHardfork::Berlin, ForkCondition::Block(0));
        self
    }

    /// Enable London at genesis.
    pub fn london_activated(mut self) -> Self {
        self = self.berlin_activated();
        self.hardforks.insert(EthereumHardfork::London, ForkCondition::Block(0));
        self
    }

    /// Enable Arrow Glacier at genesis.
    pub fn arrowglacier_activated(mut self) -> Self {
        self = self.london_activated();
        self.hardforks.insert(EthereumHardfork::ArrowGlacier, ForkCondition::Block(0));
        self
    }

    /// Enable Gray Glacier at genesis.
    pub fn grayglacier_activated(mut self) -> Self {
        self = self.arrowglacier_activated();
        self.hardforks.insert(EthereumHardfork::GrayGlacier, ForkCondition::Block(0));
        self
    }

    /// Enable Paris at genesis.
    pub fn paris_activated(mut self) -> Self {
        self = self.grayglacier_activated();
        self.hardforks.insert(
            EthereumHardfork::Paris,
            ForkCondition::TTD {
                activation_block_number: 0,
                total_difficulty: U256::ZERO,
                fork_block: None,
            },
        );
        self
    }

    /// Enable Shanghai at genesis.
    pub fn shanghai_activated(mut self) -> Self {
        self = self.paris_activated();
        self.hardforks.insert(EthereumHardfork::Shanghai, ForkCondition::Timestamp(0));
        self
    }

    /// Enable Cancun at genesis.
    pub fn cancun_activated(mut self) -> Self {
        self = self.shanghai_activated();
        self.hardforks.insert(EthereumHardfork::Cancun, ForkCondition::Timestamp(0));
        self
    }

    /// Enable Prague at genesis.
    pub fn prague_activated(mut self) -> Self {
        self = self.cancun_activated();
        self.hardforks.insert(EthereumHardfork::Prague, ForkCondition::Timestamp(0));
        self
    }

    /// Enable Prague at the given timestamp.
    pub fn with_prague_at(mut self, timestamp: u64) -> Self {
        self.hardforks.insert(EthereumHardfork::Prague, ForkCondition::Timestamp(timestamp));
        self
    }

    /// Enable Osaka at genesis.
    pub fn osaka_activated(mut self) -> Self {
        self = self.prague_activated();
        self.hardforks.insert(EthereumHardfork::Osaka, ForkCondition::Timestamp(0));
        self
    }

    /// Enable Osaka at the given timestamp.
    pub fn with_osaka_at(mut self, timestamp: u64) -> Self {
        self.hardforks.insert(EthereumHardfork::Osaka, ForkCondition::Timestamp(timestamp));
        self
    }

    /// Enable Bedrock at genesis.
    pub fn bedrock_activated(mut self) -> Self {
        self = self.paris_activated();
        self.hardforks.insert(BaseUpgrade::Bedrock, ForkCondition::Block(0));
        self
    }

    /// Enable Regolith at genesis.
    pub fn regolith_activated(mut self) -> Self {
        self = self.bedrock_activated();
        self.hardforks.insert(BaseUpgrade::Regolith, ForkCondition::Timestamp(0));
        self
    }

    /// Enable Canyon at genesis.
    pub fn canyon_activated(mut self) -> Self {
        self = self.regolith_activated();
        self.hardforks.insert(EthereumHardfork::Shanghai, ForkCondition::Timestamp(0));
        self.hardforks.insert(BaseUpgrade::Canyon, ForkCondition::Timestamp(0));
        self
    }

    /// Enable Ecotone at genesis.
    pub fn ecotone_activated(mut self) -> Self {
        self = self.canyon_activated();
        self.hardforks.insert(EthereumHardfork::Cancun, ForkCondition::Timestamp(0));
        self.hardforks.insert(BaseUpgrade::Ecotone, ForkCondition::Timestamp(0));
        self
    }

    /// Enable Fjord at genesis.
    pub fn fjord_activated(mut self) -> Self {
        self = self.ecotone_activated();
        self.hardforks.insert(BaseUpgrade::Fjord, ForkCondition::Timestamp(0));
        self
    }

    /// Enable Granite at genesis.
    pub fn granite_activated(mut self) -> Self {
        self = self.fjord_activated();
        self.hardforks.insert(BaseUpgrade::Granite, ForkCondition::Timestamp(0));
        self
    }

    /// Enable Holocene at genesis.
    pub fn holocene_activated(mut self) -> Self {
        self = self.granite_activated();
        self.hardforks.insert(BaseUpgrade::Holocene, ForkCondition::Timestamp(0));
        self
    }

    /// Enable Isthmus at genesis.
    pub fn isthmus_activated(mut self) -> Self {
        self = self.holocene_activated();
        self.hardforks.insert(EthereumHardfork::Prague, ForkCondition::Timestamp(0));
        self.hardforks.insert(BaseUpgrade::Isthmus, ForkCondition::Timestamp(0));
        self
    }

    /// Enable Jovian at genesis.
    pub fn jovian_activated(mut self) -> Self {
        self = self.isthmus_activated();
        self.hardforks.insert(BaseUpgrade::Jovian, ForkCondition::Timestamp(0));
        self
    }

    /// Enable Base V1 at genesis.
    pub fn base_v1_activated(mut self) -> Self {
        self = self.jovian_activated();
        self.hardforks.insert(EthereumHardfork::Osaka, ForkCondition::Timestamp(0));
        self.hardforks.insert(BaseUpgrade::V1, ForkCondition::Timestamp(0));
        self
    }

    /// Build the resulting [`ChainSpec`].
    pub fn build(self) -> ChainSpec {
        let paris_block_and_final_difficulty =
            self.hardforks.get(EthereumHardfork::Paris).and_then(|cond| {
                if let ForkCondition::TTD { total_difficulty, activation_block_number, .. } = cond {
                    Some((activation_block_number, total_difficulty))
                } else {
                    None
                }
            });
        let genesis = self.genesis.expect("The genesis is required");
        let base_fee_params = self
            .base_fee_params
            .unwrap_or_else(|| OpGenesisInfo::extract_from(&genesis).base_fee_params);
        let blob_params =
            self.blob_params.unwrap_or_else(|| genesis.config.blob_schedule_blob_params());

        ChainSpec {
            chain: self.chain.expect("The chain is required"),
            genesis_header: SealedHeader::seal_slow(make_genesis_header(&genesis, &self.hardforks)),
            genesis,
            hardforks: self.hardforks,
            paris_block_and_final_difficulty,
            deposit_contract: self.deposit_contract,
            base_fee_params,
            prune_delete_limit: self.prune_delete_limit.unwrap_or(MAINNET_PRUNE_DELETE_LIMIT),
            blob_params,
        }
    }
}

impl From<&Arc<ChainSpec>> for ChainSpecBuilder {
    fn from(value: &Arc<ChainSpec>) -> Self {
        Self {
            chain: Some(value.chain),
            genesis: Some(value.genesis.clone()),
            hardforks: value.hardforks.clone(),
            deposit_contract: value.deposit_contract,
            base_fee_params: Some(value.base_fee_params.clone()),
            prune_delete_limit: Some(value.prune_delete_limit),
            blob_params: Some(value.blob_params.clone()),
        }
    }
}

impl EthExecutorSpec for ChainSpec {
    fn deposit_contract_address(&self) -> Option<Address> {
        self.deposit_contract.map(|deposit_contract| deposit_contract.address)
    }
}

/// Deposit contract details.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DepositContract {
    /// Deposit contract address.
    pub address: Address,
    /// Deployment block.
    pub block: BlockNumber,
    /// `DepositEvent` event signature.
    pub topic: B256,
}

impl DepositContract {
    /// Creates a new [`DepositContract`].
    pub const fn new(address: Address, block: BlockNumber, topic: B256) -> Self {
        Self { address, block, topic }
    }
}

/// Verifies [`ChainSpec`] configuration against expected data in the given cases.
#[cfg(any(test, feature = "test-utils"))]
pub fn test_fork_ids(spec: &ChainSpec, cases: &[(Head, ForkId)]) {
    for (block, expected_id) in cases {
        let computed_id = spec.fork_id(block);
        assert_eq!(
            expected_id, &computed_id,
            "Expected fork ID {:?}, computed fork ID {:?} at block {}",
            expected_id, computed_id, block.number
        );
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::hex;

    use super::*;

    #[test]
    fn parse_supported_chains() {
        for chain in SUPPORTED_CHAINS {
            assert!(ChainSpec::parse_chain(chain).is_some(), "failed to parse {chain}");
        }
    }

    #[test]
    fn base_mainnet_display_hardforks_omits_paired_eth_forks() {
        let content = BASE_MAINNET.display_hardforks().to_string();
        assert!(content.contains(BaseUpgrade::Bedrock.name()));
        assert!(content.contains(BaseUpgrade::Canyon.name()));
        assert!(!content.contains(EthereumHardfork::London.name()));
    }

    #[test]
    fn latest_base_mainnet_fork_id_is_stable() {
        assert_eq!(
            BASE_MAINNET.latest_fork_id(),
            ForkId { hash: ForkHash(hex!("0x326df0af")), next: 0 }
        );
    }

    #[test]
    fn custom_genesis_keeps_base_upgrade_mapping() {
        let spec: ChainSpec = BASE_SEPOLIA.genesis.clone().into();
        assert_eq!(spec.fork(BaseUpgrade::Canyon), spec.fork(EthereumHardfork::Shanghai));
        assert_eq!(spec.fork(BaseUpgrade::Ecotone), spec.fork(EthereumHardfork::Cancun));
    }

    #[test]
    fn custom_genesis_deposit_contract_defaults_to_none() {
        let spec: ChainSpec = BASE_MAINNET.genesis.clone().into();
        assert_eq!(spec.deposit_contract, None);
        assert_eq!(crate::DEPOSIT_CONTRACT_TOPIC, crate::constants::DEPOSIT_CONTRACT_TOPIC);
    }
}
