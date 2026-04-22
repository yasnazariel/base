//! View-model structs for askama templates. Templates read these directly
//! so we keep all the formatting logic in one place and avoid pushing any
//! storage types into the presentation layer.

use crate::{
    rpc_proxy::{BaseBlock, BaseReceipt, BaseTransaction},
    storage::{ActivityRole, ActivityRow, BlockRow, Stats, TxRow},
};
use alloy_consensus::Typed2718 as _;
use alloy_network_primitives::{ReceiptResponse as _, TransactionResponse as _};
use alloy_primitives::{Address, B256, U256};
use alloy_rpc_types_eth::TransactionTrait as _;
use std::fmt;

/// Common footer/context fields present on every rendered page.
#[derive(Debug, Clone)]
pub struct PageCtx {
    pub branch: String,
    pub commit: String,
    pub public_rpc_url: Option<String>,
}

/// A block for listing rows.
pub struct BlockListItem {
    pub number: u64,
    pub hash: AddrLabel,
    pub timestamp: u64,
    pub age: String,
    pub miner: AddrLabel,
    pub tx_count: u64,
    pub gas_used: u64,
    pub gas_limit: u64,
}

impl From<BlockRow> for BlockListItem {
    fn from(b: BlockRow) -> Self {
        Self {
            number: b.number,
            hash: AddrLabel::from_b256(&b.hash),
            timestamp: b.timestamp,
            age: format_age(b.timestamp),
            miner: AddrLabel::from_addr(&b.miner),
            tx_count: b.tx_count,
            gas_used: b.gas_used,
            gas_limit: b.gas_limit,
        }
    }
}

/// Short + full hex pair, easier to iterate in templates than a tuple.
pub struct AddrLabel {
    pub full: String,
    pub short: String,
}

impl AddrLabel {
    pub fn from_addr(a: &Address) -> Self {
        Self { full: hex_prefix(a), short: short_hex(a) }
    }
    pub fn from_b256(h: &B256) -> Self {
        Self { full: hex_prefix(h), short: short_hex(h) }
    }
}

/// A transaction for listing rows.
pub struct TxListItem {
    pub hash: AddrLabel,
    pub block_num: u64,
    pub from: AddrLabel,
    pub to: Option<AddrLabel>,
    pub created: Option<AddrLabel>,
    pub value_eth: String,
    pub status: u8,
}

impl From<TxRow> for TxListItem {
    fn from(t: TxRow) -> Self {
        Self {
            hash: AddrLabel::from_b256(&t.hash),
            block_num: t.block_num,
            from: AddrLabel::from_addr(&t.from_addr),
            to: t.to_addr.as_ref().map(AddrLabel::from_addr),
            created: t.created.as_ref().map(AddrLabel::from_addr),
            value_eth: format_eth(t.value),
            status: t.status,
        }
    }
}

/// One activity feed item on an address page.
pub struct ActivityItem {
    pub block_num: u64,
    pub tx_hash_hex: String,
    pub tx_hash_short: String,
    pub role: &'static str,
    pub role_detail: String,
}

impl From<ActivityRow> for ActivityItem {
    fn from(a: ActivityRow) -> Self {
        let role = match a.role {
            ActivityRole::Sender => "sender",
            ActivityRole::Recipient => "recipient",
            ActivityRole::Creator => "created",
            ActivityRole::LogFrom => "token-out",
            ActivityRole::LogTo => "token-in",
        };
        let role_detail = match (a.role, a.token) {
            (ActivityRole::LogFrom | ActivityRole::LogTo, Some(token)) => {
                format!("token {}", short_hex(&token))
            }
            _ => String::new(),
        };
        Self {
            block_num: a.block_num,
            tx_hash_hex: hex_prefix(&a.tx_hash),
            tx_hash_short: short_hex(&a.tx_hash),
            role,
            role_detail,
        }
    }
}

/// Fields surfaced on a block detail page.
pub struct BlockDetail {
    pub number: u64,
    pub hash: AddrLabel,
    pub parent: AddrLabel,
    pub timestamp: u64,
    pub age: String,
    pub miner: AddrLabel,
    pub gas_used: u64,
    pub gas_limit: u64,
    pub base_fee_gwei: Option<String>,
    pub txs: Vec<TxListItem>,
}

impl BlockDetail {
    pub fn from_rpc(block: &BaseBlock, receipts: Option<&[BaseReceipt]>) -> Self {
        let mut txs = Vec::with_capacity(block.transactions.len());
        for (idx, t) in block.transactions.txns().enumerate() {
            let hash = t.tx_hash();
            let rcpt = receipts.and_then(|rs| rs.iter().find(|r| r.transaction_hash() == hash));
            let status = rcpt.map(|r| u8::from(r.status())).unwrap_or(0);
            let to_addr = t.to();
            let created =
                if to_addr.is_none() { rcpt.and_then(|r| r.contract_address()) } else { None };
            let from_addr = t.from();
            txs.push(TxListItem {
                hash: AddrLabel::from_b256(&hash),
                block_num: block.header.number,
                from: AddrLabel::from_addr(&from_addr),
                to: to_addr.as_ref().map(AddrLabel::from_addr),
                created: created.as_ref().map(AddrLabel::from_addr),
                value_eth: format_eth(t.value()),
                status,
            });
            let _ = idx;
        }

        Self {
            number: block.header.number,
            hash: AddrLabel::from_b256(&block.header.hash),
            parent: AddrLabel::from_b256(&block.header.parent_hash),
            timestamp: block.header.timestamp,
            age: format_age(block.header.timestamp),
            miner: AddrLabel::from_addr(&block.header.beneficiary),
            gas_used: block.header.gas_used,
            gas_limit: block.header.gas_limit,
            base_fee_gwei: block.header.base_fee_per_gas.map(|v| format_gwei(U256::from(v))),
            txs,
        }
    }
}

/// Selected fields plucked from the tx's containing block. Threaded into
/// [`TxDetail::from_rpc`] so the tx page can surface block-level context
/// (timestamp, base fee) without the template having to know about
/// the full block type.
#[derive(Clone, Copy, Debug, Default)]
pub struct TxBlockMeta {
    pub timestamp: u64,
    pub base_fee_per_gas: Option<u64>,
}

/// Fields surfaced on a tx detail page.
pub struct TxDetail {
    pub hash: AddrLabel,
    pub block_num: u64,
    /// Unix seconds (from the containing block), or `None` if the tx is
    /// still pending or we couldn't fetch the block.
    pub timestamp: Option<u64>,
    /// Pretty relative age (e.g. `"3m ago"`) when `timestamp` is known.
    pub age: Option<String>,
    pub from: AddrLabel,
    pub to: Option<AddrLabel>,
    pub created: Option<AddrLabel>,
    pub value_eth: String,
    pub nonce: u64,
    pub gas_limit: u64,
    pub gas_used: Option<u64>,
    pub gas_price_gwei: Option<String>,
    pub status_label: &'static str,
    pub input_hex: String,
    pub input_short: String,
    pub input_bytes: usize,
    /// Method selector (first 4 bytes of input) as `0x########`, or `None`
    /// for calls with less than 4 bytes of input (value transfers).
    pub selector: Option<String>,
    pub logs: Vec<LogDetail>,
    /// Transaction type as a hex byte (e.g. `0x02`, `0x7e`).
    pub tx_type_hex: String,
    pub tx_type_label: &'static str,
    /// EIP-1559 max fee per gas in gwei.
    pub max_fee_gwei: Option<String>,
    /// EIP-1559 max priority fee per gas in gwei.
    pub max_priority_fee_gwei: Option<String>,
    /// Block's base fee per gas in gwei (caller passes it in since the
    /// receipt doesn't carry it).
    pub base_fee_gwei: Option<String>,
    /// gas_used * effective_gas_price, formatted as ETH.
    pub fee_eth: Option<String>,
    /// gas_used / gas_limit, formatted like `"42.18%"`.
    pub gas_usage_pct: Option<String>,
}

impl TxDetail {
    pub fn from_rpc(
        tx: &BaseTransaction,
        receipt: Option<&BaseReceipt>,
        block_meta: Option<TxBlockMeta>,
    ) -> Self {
        let base_fee_per_gas = block_meta.and_then(|m| m.base_fee_per_gas);
        let timestamp = block_meta.map(|m| m.timestamp);
        let age = timestamp.map(format_age);
        let input = tx.input();
        let input_hex = hex::encode(input);
        let input_short = if input.is_empty() {
            "(empty)".to_string()
        } else if input.len() <= 32 {
            format!("0x{input_hex}")
        } else {
            format!("0x{}… ({} bytes)", &input_hex[..64], input.len())
        };
        let selector = if input.len() >= 4 {
            Some(format!("0x{}", hex::encode(&input[..4])))
        } else {
            None
        };

        let logs = receipt
            .map(|r| {
                r.inner
                    .logs()
                    .iter()
                    .enumerate()
                    .map(|(i, log)| LogDetail {
                        index: i as u64,
                        address: AddrLabel::from_addr(&log.address()),
                        topics_hex: log.topics().iter().map(hex_prefix).collect(),
                        data_short: {
                            let bytes = &log.data().data;
                            if bytes.is_empty() {
                                "(empty)".to_string()
                            } else {
                                let d = hex::encode(bytes);
                                if d.len() <= 64 {
                                    format!("0x{d}")
                                } else {
                                    format!("0x{}… ({} bytes)", &d[..64], bytes.len())
                                }
                            }
                        },
                    })
                    .collect()
            })
            .unwrap_or_default();

        let tx_hash = tx.tx_hash();
        let from_addr = tx.from();
        let to_addr = tx.to();
        let created_addr = receipt.and_then(|r| r.contract_address());

        let gas_limit = tx.gas_limit();
        let gas_used = receipt.map(|r| r.gas_used());
        let effective_gas_price = receipt.map(|r| r.effective_gas_price()).unwrap_or(0);

        let fee_eth = match gas_used {
            Some(g) if effective_gas_price > 0 => Some(format_eth(
                U256::from(g).saturating_mul(U256::from(effective_gas_price)),
            )),
            _ => None,
        };
        let gas_usage_pct = gas_used.and_then(|g| {
            if gas_limit == 0 {
                None
            } else {
                let pct = (g as f64 / gas_limit as f64) * 100.0;
                Some(format!("{pct:.2}%"))
            }
        });

        // EIP-1559 fee caps only apply to type-2+ txs. The consensus trait
        // returns `u128::MAX` / `None` for legacy/deposit, so we gate on
        // presence before displaying. Fully qualify to disambiguate between
        // the `TransactionResponse` and `TransactionTrait` impls that both
        // expose `max_fee_per_gas`.
        use alloy_rpc_types_eth::TransactionTrait;
        let max_fee = TransactionTrait::max_fee_per_gas(tx);
        let max_prio = TransactionTrait::max_priority_fee_per_gas(tx);
        let max_fee_gwei = if max_fee > 0 { Some(format_gwei(U256::from(max_fee))) } else { None };
        let max_priority_fee_gwei = max_prio.map(|v| format_gwei(U256::from(v)));

        let ty = tx.ty();

        Self {
            hash: AddrLabel::from_b256(&tx_hash),
            block_num: tx.block_number().unwrap_or(0),
            timestamp,
            age,
            from: AddrLabel::from_addr(&from_addr),
            to: to_addr.as_ref().map(AddrLabel::from_addr),
            created: created_addr.as_ref().map(AddrLabel::from_addr),
            value_eth: format_eth(tx.value()),
            nonce: tx.nonce(),
            gas_limit,
            gas_used,
            gas_price_gwei: if effective_gas_price == 0 {
                None
            } else {
                Some(format_gwei(U256::from(effective_gas_price)))
            },
            status_label: match receipt.map(|r| u8::from(r.status())) {
                Some(1) => "ok",
                Some(_) => "fail",
                None => "pending",
            },
            input_hex: format!("0x{input_hex}"),
            input_short,
            input_bytes: input.len(),
            selector,
            logs,
            tx_type_hex: format!("0x{ty:02x}"),
            tx_type_label: tx_type_label(ty),
            max_fee_gwei,
            max_priority_fee_gwei,
            base_fee_gwei: base_fee_per_gas.map(|v| format_gwei(U256::from(v))),
            fee_eth,
            gas_usage_pct,
        }
    }
}

/// Human label for an EIP-2718 tx type byte. Covers the Ethereum standard
/// types plus OP-stack deposit (0x7e); unknown types fall through to
/// "unknown" so we don't silently mislabel future variants.
fn tx_type_label(ty: u8) -> &'static str {
    match ty {
        0x00 => "legacy",
        0x01 => "access list (EIP-2930)",
        0x02 => "dynamic fee (EIP-1559)",
        0x03 => "blob (EIP-4844)",
        0x04 => "set code (EIP-7702)",
        0x7e => "deposit (OP-stack)",
        _ => "unknown",
    }
}

pub struct LogDetail {
    pub index: u64,
    pub address: AddrLabel,
    pub topics_hex: Vec<String>,
    pub data_short: String,
}

/// Fields on an address page.
pub struct AddressDetail {
    pub hex: String,
    pub balance_eth: String,
    pub nonce: u64,
    pub is_contract: bool,
    pub code_size: usize,
    pub activity: Vec<ActivityItem>,
    pub next_cursor: Option<String>,
}

/// Home page stats.
pub struct StatsBlock {
    pub blocks: u64,
    pub txs: u64,
    pub addresses: u64,
    pub head: u64,
}

impl StatsBlock {
    pub fn new(s: Stats, head: u64) -> Self {
        Self { blocks: s.blocks, txs: s.txs, addresses: s.addresses, head }
    }
}

// ---- formatting helpers -------------------------------------------------

pub fn hex_prefix<T: AsRef<[u8]>>(bytes: &T) -> String {
    format!("0x{}", hex::encode(bytes.as_ref()))
}

pub fn short_hex<T: AsRef<[u8]>>(bytes: &T) -> String {
    let hex = hex::encode(bytes.as_ref());
    if hex.len() <= 10 {
        format!("0x{hex}")
    } else {
        format!("0x{}…{}", &hex[..6], &hex[hex.len() - 4..])
    }
}

pub fn format_age(ts: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(ts);
    if ts > now {
        return "just now".to_string();
    }
    let diff = now - ts;
    if diff < 60 {
        return format!("{diff}s ago");
    }
    if diff < 3600 {
        return format!("{}m ago", diff / 60);
    }
    if diff < 86400 {
        return format!("{}h ago", diff / 3600);
    }
    format!("{}d ago", diff / 86400)
}

pub fn format_eth(value: U256) -> String {
    // 18 decimals. We only need display precision, so truncate rather than
    // round and keep everything in u128-safe arithmetic via division.
    let wei = value;
    let whole = wei / U256::from(10u128.pow(18));
    let frac = wei % U256::from(10u128.pow(18));
    if frac == U256::ZERO {
        return format!("{whole} ETH");
    }
    // Trim trailing zeros from the 18-digit fractional part.
    let frac_str = format!("{frac:018}");
    let frac_trimmed = frac_str.trim_end_matches('0');
    if frac_trimmed.is_empty() {
        format!("{whole} ETH")
    } else {
        format!("{whole}.{frac_trimmed} ETH")
    }
}

pub fn format_gwei(value: U256) -> String {
    let gwei = value / U256::from(1_000_000_000u64);
    let frac = value % U256::from(1_000_000_000u64);
    if frac == U256::ZERO {
        format!("{gwei} gwei")
    } else {
        format!("{gwei}.{:09} gwei", frac).trim_end_matches('0').to_string()
    }
}

// Implement Display passthroughs so templates can `{{ a | safe }}` address /
// hash formatted strings without fuss.
impl fmt::Display for BlockListItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "block #{}", self.number)
    }
}

/// Helper: turn an [`Address`] or [`B256`] into the pair (full hex, short hex).
pub fn hex_pair_b256(h: &B256) -> (String, String) {
    (hex_prefix(h), short_hex(h))
}

pub fn hex_pair_addr(a: &Address) -> (String, String) {
    (hex_prefix(a), short_hex(a))
}
