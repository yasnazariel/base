use alloy_network::TransactionBuilder;
use alloy_primitives::{Address, Bytes, Signed, U160, U256};
use alloy_rpc_types::TransactionRequest;
use alloy_sol_types::{SolCall, sol};

use super::Payload;
use crate::workload::SeededRng;

type I24 = Signed<24, 1>;

sol! {
    interface IAerodromeRouter {
        struct Route {
            address from;
            address to;
            bool stable;
            address factory;
        }

        function swapExactETHForTokens(
            uint256 amountOutMin,
            Route[] calldata routes,
            address to,
            uint256 deadline
        ) external payable returns (uint256[] memory amounts);
    }

    interface IAerodromeClRouter {
        struct ExactInputSingleParams {
            address tokenIn;
            address tokenOut;
            int24 tickSpacing;
            address recipient;
            uint256 deadline;
            uint256 amountIn;
            uint256 amountOutMinimum;
            uint160 sqrtPriceLimitX96;
        }

        function exactInputSingle(
            ExactInputSingleParams calldata params
        ) external payable returns (uint256 amountOut);
    }
}

/// Generates Aerodrome V2 (classic AMM) swap transactions.
#[derive(Debug, Clone)]
pub struct AerodromeV2Payload {
    /// Router contract address.
    pub router: Address,
    /// WETH contract address.
    pub weth: Address,
    /// Output token address.
    pub token: Address,
    /// Whether to use stable pool.
    pub stable: bool,
    /// Factory address.
    pub factory: Address,
    /// Minimum swap amount.
    pub min_amount: U256,
    /// Maximum swap amount.
    pub max_amount: U256,
}

impl AerodromeV2Payload {
    /// Creates a new `AerodromeV2` payload.
    pub const fn new(
        router: Address,
        weth: Address,
        token: Address,
        stable: bool,
        factory: Address,
        min_amount: U256,
        max_amount: U256,
    ) -> Self {
        Self { router, weth, token, stable, factory, min_amount, max_amount }
    }
}

impl Payload for AerodromeV2Payload {
    fn name(&self) -> &'static str {
        "aerodrome_v2"
    }

    fn generate(&self, rng: &mut SeededRng, _from: Address, to: Address) -> TransactionRequest {
        let amount = if self.min_amount == self.max_amount {
            self.min_amount
        } else {
            let min: u128 = self.min_amount.try_into().unwrap_or(u128::MAX);
            let max: u128 = self.max_amount.try_into().unwrap_or(u128::MAX);
            U256::from(rng.gen_range(min..=max))
        };

        let call = IAerodromeRouter::swapExactETHForTokensCall {
            amountOutMin: U256::ZERO,
            routes: vec![IAerodromeRouter::Route {
                from: self.weth,
                to: self.token,
                stable: self.stable,
                factory: self.factory,
            }],
            to,
            deadline: U256::from(u64::MAX),
        };

        TransactionRequest::default()
            .with_to(self.router)
            .with_input(Bytes::from(call.abi_encode()))
            .with_value(amount)
            .with_gas_limit(200_000)
    }
}

/// Generates Aerodrome Slipstream (concentrated liquidity) swap transactions.
#[derive(Debug, Clone)]
pub struct AerodromeClPayload {
    /// CL Router contract address.
    pub router: Address,
    /// Input token address.
    pub token_in: Address,
    /// Output token address.
    pub token_out: Address,
    /// Tick spacing.
    pub tick_spacing: i32,
    /// Minimum swap amount.
    pub min_amount: U256,
    /// Maximum swap amount.
    pub max_amount: U256,
}

impl AerodromeClPayload {
    /// Creates a new `AerodromeCl` payload.
    pub const fn new(
        router: Address,
        token_in: Address,
        token_out: Address,
        tick_spacing: i32,
        min_amount: U256,
        max_amount: U256,
    ) -> Self {
        Self { router, token_in, token_out, tick_spacing, min_amount, max_amount }
    }
}

impl Payload for AerodromeClPayload {
    fn name(&self) -> &'static str {
        "aerodrome_cl"
    }

    fn generate(&self, rng: &mut SeededRng, _from: Address, to: Address) -> TransactionRequest {
        let amount = if self.min_amount == self.max_amount {
            self.min_amount
        } else {
            let min: u128 = self.min_amount.try_into().unwrap_or(u128::MAX);
            let max: u128 = self.max_amount.try_into().unwrap_or(u128::MAX);
            U256::from(rng.gen_range(min..=max))
        };

        let call = IAerodromeClRouter::exactInputSingleCall {
            params: IAerodromeClRouter::ExactInputSingleParams {
                tokenIn: self.token_in,
                tokenOut: self.token_out,
                // SAFETY: tick_spacing is validated to fit i24 at config parse time.
                tickSpacing: I24::try_from(self.tick_spacing)
                    .expect("validated at config parse time"),
                recipient: to,
                deadline: U256::from(u64::MAX),
                amountIn: amount,
                amountOutMinimum: U256::ZERO,
                sqrtPriceLimitX96: U160::ZERO,
            },
        };

        TransactionRequest::default()
            .with_to(self.router)
            .with_input(Bytes::from(call.abi_encode()))
            .with_gas_limit(250_000)
    }
}
