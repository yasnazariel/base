use alloc::sync::Arc;
use core::fmt::Debug;

use alloy_consensus::{BlockHeader, Header};
use alloy_evm::{EvmFactory, FromRecoveredTx, FromTxWithEncoded};
use base_alloy_consensus::{EIP1559ParamError, OpBlock, OpReceipt};
use base_alloy_rpc_types_engine::OpExecutionData;
use reth_chainspec::{ChainSpec, EthChainSpec};
#[cfg(feature = "std")]
use reth_evm::{ConfigureEngineEvm, ExecutableTxIterator};
use reth_evm::{ConfigureEvm, EvmEnv, TransactionEnv, precompiles::PrecompilesMap};
use reth_primitives::{OpHeader, OpPrimitives, OpTransactionSigned};
use reth_primitives_traits::{SealedBlock, SealedHeader, SignedTransaction};
use revm::context::{BlockEnv, TxEnv};
#[allow(unused_imports)]
use {
    alloy_eips::Decodable2718,
    alloy_primitives::{Bytes, U256},
    reth_evm::{EvmEnvFor, ExecutionCtxFor},
    reth_primitives_traits::{TxTy, WithEncoded},
    reth_storage_errors::any::AnyError,
    revm::{
        context::CfgEnv, context_interface::block::BlobExcessGasAndPrice,
        primitives::hardfork::SpecId,
    },
};

use crate::{
    OpBlockAssembler, OpBlockExecutionCtx, OpBlockExecutorFactory, OpEvmFactory,
    OpNextBlockEnvAttributes, OpReceiptBuilder, OpRethReceiptBuilder, OpSpecId, OpTransaction,
    OpTxEnv, revm_spec_by_timestamp_after_bedrock,
};

/// Helper type with backwards compatible methods to obtain executor providers.
pub type OpExecutorProvider = OpEvmConfig;

fn op_evm_env(header: &Header, chain_spec: &ChainSpec) -> EvmEnv<OpSpecId> {
    let spec = revm_spec_by_timestamp_after_bedrock(chain_spec, header.timestamp);
    let cfg_env =
        CfgEnv::new().with_chain_id(chain_spec.chain().id()).with_spec_and_mainnet_gas_params(spec);

    let blob_excess_gas_and_price = spec
        .into_eth_spec()
        .is_enabled_in(SpecId::CANCUN)
        .then_some(BlobExcessGasAndPrice { excess_blob_gas: 0, blob_gasprice: 1 });

    let is_merge = spec.into_eth_spec() >= SpecId::MERGE;

    let block_env = BlockEnv {
        number: U256::from(header.number),
        beneficiary: header.beneficiary,
        timestamp: U256::from(header.timestamp),
        difficulty: if is_merge { U256::ZERO } else { header.difficulty },
        prevrandao: if is_merge { Some(header.mix_hash) } else { None },
        gas_limit: header.gas_limit,
        basefee: header.base_fee_per_gas.unwrap_or_default(),
        blob_excess_gas_and_price,
    };

    EvmEnv { cfg_env, block_env }
}

fn op_next_evm_env(
    parent: &Header,
    attributes: &OpNextBlockEnvAttributes,
    base_fee_per_gas: u64,
    chain_spec: &ChainSpec,
) -> EvmEnv<OpSpecId> {
    let spec = revm_spec_by_timestamp_after_bedrock(chain_spec, attributes.timestamp);
    let cfg_env =
        CfgEnv::new().with_chain_id(chain_spec.chain().id()).with_spec_and_mainnet_gas_params(spec);

    let blob_excess_gas_and_price = spec
        .into_eth_spec()
        .is_enabled_in(SpecId::CANCUN)
        .then_some(BlobExcessGasAndPrice { excess_blob_gas: 0, blob_gasprice: 1 });

    let is_merge = spec.into_eth_spec() >= SpecId::MERGE;

    let block_env = BlockEnv {
        number: U256::from(parent.number.saturating_add(1)),
        beneficiary: attributes.suggested_fee_recipient,
        timestamp: U256::from(attributes.timestamp),
        difficulty: if is_merge { U256::ZERO } else { parent.difficulty },
        prevrandao: if is_merge { Some(attributes.prev_randao) } else { None },
        gas_limit: attributes.gas_limit,
        basefee: base_fee_per_gas,
        blob_excess_gas_and_price,
    };

    EvmEnv { cfg_env, block_env }
}

/// Base EVM configuration exposed from the reth network-specific EVM crate.
#[derive(Debug)]
pub struct OpEvmConfig<R = OpRethReceiptBuilder, EvmFactory = OpEvmFactory> {
    /// Inner Base block executor factory.
    pub executor_factory: OpBlockExecutorFactory<R, Arc<ChainSpec>, EvmFactory>,
    /// Base block assembler.
    pub block_assembler: OpBlockAssembler,
}

impl<R: Clone, EvmFactory: Clone> Clone for OpEvmConfig<R, EvmFactory> {
    fn clone(&self) -> Self {
        Self {
            executor_factory: self.executor_factory.clone(),
            block_assembler: self.block_assembler.clone(),
        }
    }
}

impl OpEvmConfig {
    /// Creates a new Base EVM configuration with the default Base receipt builder.
    pub fn optimism(chain_spec: Arc<ChainSpec>) -> Self {
        Self::new(chain_spec, OpRethReceiptBuilder::default())
    }
}

impl<R> OpEvmConfig<R> {
    /// Creates a new Base EVM configuration with the given chain spec.
    pub fn new(chain_spec: Arc<ChainSpec>, receipt_builder: R) -> Self {
        Self {
            block_assembler: OpBlockAssembler::new(Arc::clone(&chain_spec)),
            executor_factory: OpBlockExecutorFactory::new(
                receipt_builder,
                chain_spec,
                OpEvmFactory::default(),
            ),
        }
    }
}

impl<R, EvmFactory> OpEvmConfig<R, EvmFactory> {
    /// Returns the chain spec associated with this configuration.
    pub const fn chain_spec(&self) -> &Arc<ChainSpec> {
        self.executor_factory.spec()
    }
}

impl<R, EvmF> ConfigureEvm for OpEvmConfig<R, EvmF>
where
    OpTransaction<TxEnv>:
        FromRecoveredTx<OpTransactionSigned> + FromTxWithEncoded<OpTransactionSigned>,
    R: OpReceiptBuilder<Receipt = OpReceipt, Transaction = OpTransactionSigned>,
    EvmF: EvmFactory<
            Tx: FromRecoveredTx<OpTransactionSigned>
                    + FromTxWithEncoded<OpTransactionSigned>
                    + TransactionEnv
                    + OpTxEnv,
            Precompiles = PrecompilesMap,
            Spec = OpSpecId,
            BlockEnv = BlockEnv,
        > + Debug,
    Self: Send + Sync + Unpin + Clone + 'static,
{
    type Primitives = OpPrimitives;
    type Error = EIP1559ParamError;
    type NextBlockEnvCtx = OpNextBlockEnvAttributes;
    type BlockExecutorFactory = OpBlockExecutorFactory<R, Arc<ChainSpec>, EvmF>;
    type BlockAssembler = OpBlockAssembler;

    fn block_executor_factory(&self) -> &Self::BlockExecutorFactory {
        &self.executor_factory
    }

    fn block_assembler(&self) -> &Self::BlockAssembler {
        &self.block_assembler
    }

    fn evm_env(&self, header: &Header) -> Result<EvmEnv<OpSpecId>, Self::Error> {
        Ok(op_evm_env(header, self.chain_spec()))
    }

    fn next_evm_env(
        &self,
        parent: &Header,
        attributes: &Self::NextBlockEnvCtx,
    ) -> Result<EvmEnv<OpSpecId>, Self::Error> {
        let base_fee =
            self.chain_spec().next_block_base_fee(parent, attributes.timestamp).unwrap_or_default();

        Ok(op_next_evm_env(parent, attributes, base_fee, self.chain_spec()))
    }

    fn context_for_block(
        &self,
        block: &'_ SealedBlock<OpBlock>,
    ) -> Result<OpBlockExecutionCtx, Self::Error> {
        Ok(OpBlockExecutionCtx {
            parent_hash: block.header().parent_hash(),
            parent_beacon_block_root: block.header().parent_beacon_block_root(),
            extra_data: block.header().extra_data().clone(),
        })
    }

    fn context_for_next_block(
        &self,
        parent: &SealedHeader<OpHeader>,
        attributes: Self::NextBlockEnvCtx,
    ) -> Result<OpBlockExecutionCtx, Self::Error> {
        Ok(OpBlockExecutionCtx {
            parent_hash: parent.hash(),
            parent_beacon_block_root: attributes.parent_beacon_block_root,
            extra_data: attributes.extra_data,
        })
    }
}

#[cfg(feature = "std")]
impl<R> ConfigureEngineEvm<OpExecutionData> for OpEvmConfig<R>
where
    OpTransaction<TxEnv>:
        FromRecoveredTx<OpTransactionSigned> + FromTxWithEncoded<OpTransactionSigned>,
    R: OpReceiptBuilder<Receipt = OpReceipt, Transaction = OpTransactionSigned>,
    Self: Send + Sync + Unpin + Clone + 'static,
{
    fn evm_env_for_payload(
        &self,
        payload: &OpExecutionData,
    ) -> Result<EvmEnvFor<Self>, Self::Error> {
        let timestamp = payload.payload.timestamp();
        let block_number = payload.payload.block_number();

        let spec = revm_spec_by_timestamp_after_bedrock(self.chain_spec(), timestamp);

        let cfg_env = CfgEnv::new()
            .with_chain_id(self.chain_spec().chain().id())
            .with_spec_and_mainnet_gas_params(spec);

        let blob_excess_gas_and_price = spec
            .into_eth_spec()
            .is_enabled_in(SpecId::CANCUN)
            .then_some(BlobExcessGasAndPrice { excess_blob_gas: 0, blob_gasprice: 1 });

        let block_env = BlockEnv {
            number: U256::from(block_number),
            beneficiary: payload.payload.as_v1().fee_recipient,
            timestamp: U256::from(timestamp),
            difficulty: if spec.into_eth_spec() >= SpecId::MERGE {
                U256::ZERO
            } else {
                payload.payload.as_v1().prev_randao.into()
            },
            prevrandao: (spec.into_eth_spec() >= SpecId::MERGE)
                .then(|| payload.payload.as_v1().prev_randao),
            gas_limit: payload.payload.as_v1().gas_limit,
            basefee: payload.payload.as_v1().base_fee_per_gas.to(),
            blob_excess_gas_and_price,
        };

        Ok(EvmEnv { cfg_env, block_env })
    }

    fn context_for_payload<'a>(
        &self,
        payload: &'a OpExecutionData,
    ) -> Result<ExecutionCtxFor<'a, Self>, Self::Error> {
        Ok(OpBlockExecutionCtx {
            parent_hash: payload.parent_hash(),
            parent_beacon_block_root: payload.sidecar.parent_beacon_block_root(),
            extra_data: payload.payload.as_v1().extra_data.clone(),
        })
    }

    fn tx_iterator_for_payload(
        &self,
        payload: &OpExecutionData,
    ) -> Result<impl ExecutableTxIterator<Self>, Self::Error> {
        let transactions = payload.payload.transactions().clone();
        let convert = |encoded: Bytes| {
            let tx = TxTy::<Self::Primitives>::decode_2718_exact(encoded.as_ref())
                .map_err(AnyError::new)?;
            let signer = tx.try_recover().map_err(AnyError::new)?;
            Ok::<_, AnyError>(WithEncoded::new(encoded, tx.with_signer(signer)))
        };

        Ok((transactions, convert))
    }
}
