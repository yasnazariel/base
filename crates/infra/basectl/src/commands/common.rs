use std::{
    collections::{HashSet, VecDeque},
    time::{Duration, Instant},
};

use alloy_primitives::{Address, B256, Bytes};
use chrono::{DateTime, Local};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState},
};

use crate::rpc::{L1BlockInfo, L1ConnectionMode};

/// Size of a single blob in bytes (128 `KiB`).
pub(crate) const BLOB_SIZE: u64 = 128 * 1024;
/// Maximum number of entries retained in history buffers.
pub(crate) const MAX_HISTORY: usize = 1000;

const BLOCK_COLORS: [Color; 24] = [
    Color::Rgb(0, 82, 255),
    Color::Rgb(0, 140, 255),
    Color::Rgb(0, 180, 220),
    Color::Rgb(0, 190, 180),
    Color::Rgb(0, 180, 130),
    Color::Rgb(40, 180, 100),
    Color::Rgb(80, 180, 80),
    Color::Rgb(130, 180, 60),
    Color::Rgb(170, 170, 50),
    Color::Rgb(200, 160, 50),
    Color::Rgb(220, 140, 50),
    Color::Rgb(230, 110, 60),
    Color::Rgb(235, 90, 70),
    Color::Rgb(230, 70, 90),
    Color::Rgb(220, 60, 120),
    Color::Rgb(200, 60, 150),
    Color::Rgb(180, 70, 180),
    Color::Rgb(150, 80, 200),
    Color::Rgb(120, 90, 210),
    Color::Rgb(90, 100, 220),
    Color::Rgb(60, 110, 230),
    Color::Rgb(40, 130, 240),
    Color::Rgb(30, 160, 245),
    Color::Rgb(20, 180, 235),
];

const EIGHTH_BLOCKS: [char; 8] = ['▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];

// =============================================================================
// Color Constants
// =============================================================================

/// Primary Base blue color.
pub(crate) const COLOR_BASE_BLUE: Color = Color::Rgb(0, 82, 255);
/// Active border highlight color.
pub(crate) const COLOR_ACTIVE_BORDER: Color = Color::Rgb(100, 180, 255);

/// Background color for the currently selected table row.
pub(crate) const COLOR_ROW_SELECTED: Color = Color::Rgb(60, 60, 80);
/// Background color for a highlighted (cross-referenced) table row.
pub(crate) const COLOR_ROW_HIGHLIGHTED: Color = Color::Rgb(40, 40, 60);

/// Color for DA growth rate indicators.
pub(crate) const COLOR_GROWTH: Color = Color::Rgb(255, 180, 100);
/// Color for DA burn rate indicators.
pub(crate) const COLOR_BURN: Color = Color::Rgb(100, 200, 100);
/// Color for gas target markers.
pub(crate) const COLOR_TARGET: Color = Color::Rgb(255, 200, 100);
/// Color for gas bar fill below target.
pub(crate) const COLOR_GAS_FILL: Color = Color::Rgb(100, 180, 255);

// =============================================================================
// Duration Constants
// =============================================================================

/// Timeout for terminal event polling.
pub(crate) const EVENT_POLL_TIMEOUT: Duration = Duration::from_millis(100);
/// Rate calculation window of 30 seconds.
pub(crate) const RATE_WINDOW_30S: Duration = Duration::from_secs(30);
/// Rate calculation window of 2 minutes.
pub(crate) const RATE_WINDOW_2M: Duration = Duration::from_secs(120);
/// Rate calculation window of 5 minutes.
pub(crate) const RATE_WINDOW_5M: Duration = Duration::from_secs(300);
/// Number of recent L1 blocks used for blob share and target usage calculations.
pub(crate) const L1_BLOCK_WINDOW: usize = 10;

// =============================================================================
// Shared Data Types
// =============================================================================

/// A single flashblock entry displayed in the TUI.
#[derive(Clone, Debug)]
pub(crate) struct FlashblockEntry {
    /// L2 block number.
    pub block_number: u64,
    /// Flashblock index within the block.
    pub index: u64,
    /// Number of transactions in this flashblock.
    pub tx_count: usize,
    /// Cumulative gas used up to this flashblock.
    pub gas_used: u64,
    /// Block gas limit.
    pub gas_limit: u64,
    /// Base fee per gas in wei, if available.
    pub base_fee: Option<u128>,
    /// Previous block's base fee for delta display.
    pub prev_base_fee: Option<u128>,
    /// Local timestamp when this flashblock was received.
    pub timestamp: DateTime<Local>,
    /// Time difference in milliseconds from the previous flashblock.
    pub time_diff_ms: Option<i64>,
}

/// An L2 block's data availability contribution.
#[derive(Clone, Debug)]
pub(crate) struct BlockContribution {
    /// L2 block number.
    pub block_number: u64,
    /// DA bytes contributed by this block.
    pub da_bytes: u64,
    /// Unix timestamp of the block.
    pub timestamp: u64,
}

impl BlockContribution {
    /// Returns the age of this block in seconds since its timestamp.
    pub(crate) fn age_seconds(&self) -> u64 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now.saturating_sub(self.timestamp)
    }
}

/// An L1 block with blob and attribution data.
#[derive(Clone, Debug)]
pub(crate) struct L1Block {
    /// L1 block number.
    pub block_number: u64,
    /// Unix timestamp of the L1 block.
    pub timestamp: u64,
    /// Total number of blobs in this L1 block.
    pub total_blobs: u64,
    /// Number of blobs submitted by the Base batcher.
    pub base_blobs: u64,
    /// Number of L2 blocks attributed to this L1 block.
    pub l2_blocks_submitted: Option<u64>,
    /// Total DA bytes from L2 blocks attributed to this L1 block.
    pub l2_da_bytes: Option<u64>,
    /// Range of L2 block numbers attributed to this L1 block.
    pub l2_block_range: Option<(u64, u64)>,
}

impl L1Block {
    /// Creates a new `L1Block` from raw L1 block info.
    pub(crate) const fn from_info(info: L1BlockInfo) -> Self {
        Self {
            block_number: info.block_number,
            timestamp: info.timestamp,
            total_blobs: info.total_blobs,
            base_blobs: info.base_blobs,
            l2_blocks_submitted: None,
            l2_da_bytes: None,
            l2_block_range: None,
        }
    }

    /// Returns true if this L1 block contains any blobs.
    pub(crate) const fn has_blobs(&self) -> bool {
        self.total_blobs > 0
    }

    /// Returns true if this L1 block contains blobs from the Base batcher.
    pub(crate) const fn has_base_blobs(&self) -> bool {
        self.base_blobs > 0
    }

    /// Returns a formatted string of base/total blob counts.
    pub(crate) fn blobs_display(&self) -> String {
        format!("{}/{}", self.base_blobs, self.total_blobs)
    }

    /// Returns the block number truncated to fit within `max_width` characters.
    pub(crate) fn block_display(&self, max_width: usize) -> String {
        truncate_block_number(self.block_number, max_width)
    }

    /// Returns the number of attributed L2 blocks as a display string.
    pub(crate) fn l2_blocks_display(&self) -> String {
        self.l2_blocks_submitted.map_or_else(|| "-".to_string(), |n| n.to_string())
    }

    /// Returns the DA-to-L1 compression ratio, if data is available.
    pub(crate) fn compression_ratio(&self) -> Option<f64> {
        let da_bytes = self.l2_da_bytes?;
        if self.base_blobs == 0 {
            return None;
        }
        let l1_bytes = self.base_blobs * BLOB_SIZE;
        Some(da_bytes as f64 / l1_bytes as f64)
    }

    /// Returns the compression ratio as a formatted display string.
    pub(crate) fn compression_display(&self) -> String {
        self.compression_ratio().map_or_else(|| "-".to_string(), |r| format!("{r:.2}x"))
    }

    /// Returns the age of this L1 block in seconds.
    pub(crate) fn age_seconds(&self) -> u64 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        now.saturating_sub(self.timestamp)
    }

    /// Returns the block age as a human-readable duration string.
    pub(crate) fn age_display(&self) -> String {
        format_duration(Duration::from_secs(self.age_seconds()))
    }
}

/// Filter mode for the L1 blocks table display.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum L1BlockFilter {
    /// Show all L1 blocks.
    #[default]
    All,
    /// Show only L1 blocks containing blobs.
    WithBlobs,
    /// Show only L1 blocks containing Base batcher blobs.
    WithBaseBlobs,
}

impl L1BlockFilter {
    /// Returns the next filter in the cycle.
    pub(crate) const fn next(self) -> Self {
        match self {
            Self::All => Self::WithBlobs,
            Self::WithBlobs => Self::WithBaseBlobs,
            Self::WithBaseBlobs => Self::All,
        }
    }

    /// Returns a short label for this filter mode.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::WithBlobs => "Blobs",
            Self::WithBaseBlobs => "Base",
        }
    }
}

/// Tracks byte rate samples over a sliding time window.
#[derive(Debug)]
pub(crate) struct RateTracker {
    samples: VecDeque<(Instant, u64)>,
}

impl Default for RateTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl RateTracker {
    /// Creates a new rate tracker with an empty sample buffer.
    pub(crate) fn new() -> Self {
        Self { samples: VecDeque::with_capacity(300) }
    }

    /// Records a byte count sample at the current instant.
    pub(crate) fn add_sample(&mut self, bytes: u64) {
        let now = Instant::now();
        self.samples.push_back((now, bytes));
        let cutoff = now - Duration::from_secs(300);
        while self.samples.front().is_some_and(|(t, _)| *t < cutoff) {
            self.samples.pop_front();
        }
    }

    /// Computes the byte rate (bytes/sec) over the given duration window.
    pub(crate) fn rate_over(&self, duration: Duration) -> Option<f64> {
        let now = Instant::now();
        let cutoff = now - duration;

        let (count, total, earliest) = self.samples.iter().filter(|(t, _)| *t >= cutoff).fold(
            (0usize, 0u64, None::<Instant>),
            |(count, total, earliest), (t, b)| {
                (count + 1, total + b, Some(earliest.map_or(*t, |e: Instant| e.min(*t))))
            },
        );

        if count < 2 {
            return None;
        }

        let elapsed = now.duration_since(earliest?).as_secs_f64();
        if elapsed <= 0.0 {
            return None;
        }

        Some(total as f64 / elapsed)
    }
}

/// Progress state during initial backlog loading.
#[derive(Debug)]
pub(crate) struct LoadingState {
    /// Number of blocks fetched so far.
    pub current_block: u64,
    /// Total number of blocks to fetch.
    pub total_blocks: u64,
}

// =============================================================================
// DA Tracker - Shared State Management for DA Monitoring
// =============================================================================

/// Tracks DA backlog state, L2 block contributions, and L1 blob data.
#[derive(Debug)]
pub(crate) struct DaTracker {
    /// Latest safe L2 block number.
    pub safe_l2_block: u64,
    /// Total DA bytes in the backlog (unsafe minus safe).
    pub da_backlog_bytes: u64,
    /// Per-block DA byte contributions, newest first.
    pub block_contributions: VecDeque<BlockContribution>,
    /// Recent L1 blocks with blob information, newest first.
    pub l1_blocks: VecDeque<L1Block>,
    /// Tracks DA growth rate (bytes added from new L2 blocks).
    pub growth_tracker: RateTracker,
    /// Tracks DA burn rate (bytes consumed when blocks become safe).
    pub burn_tracker: RateTracker,
    /// Timestamp of the last L1 block containing Base blobs.
    pub last_base_blob_time: Option<Instant>,
    /// Safe L2 block at the time of last L1→L2 attribution.
    /// Used to compute the delta of L2 blocks to attribute to the next L1 blob block.
    last_attributed_safe_l2: u64,
}

impl Default for DaTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl DaTracker {
    /// Creates a new empty DA tracker.
    pub(crate) fn new() -> Self {
        Self {
            safe_l2_block: 0,
            da_backlog_bytes: 0,
            block_contributions: VecDeque::with_capacity(MAX_HISTORY),
            l1_blocks: VecDeque::with_capacity(MAX_HISTORY),
            growth_tracker: RateTracker::new(),
            burn_tracker: RateTracker::new(),
            last_base_blob_time: None,
            last_attributed_safe_l2: 0,
        }
    }

    /// Sets the initial backlog state from the safe block and total DA bytes.
    pub(crate) const fn set_initial_backlog(&mut self, safe_block: u64, da_bytes: u64) {
        self.safe_l2_block = safe_block;
        self.da_backlog_bytes = da_bytes;
        self.last_attributed_safe_l2 = safe_block;
    }

    /// Adds a block from the initial backlog fetch.
    pub(crate) fn add_backlog_block(&mut self, block_number: u64, da_bytes: u64, timestamp: u64) {
        let contribution = BlockContribution { block_number, da_bytes, timestamp };
        self.block_contributions.push_front(contribution);
        if self.block_contributions.len() > MAX_HISTORY {
            self.block_contributions.pop_back();
        }
    }

    /// Records a new L2 block and adds its DA bytes to the backlog.
    pub(crate) fn add_block(&mut self, block_number: u64, da_bytes: u64, timestamp: u64) {
        if block_number <= self.safe_l2_block {
            return;
        }

        self.da_backlog_bytes = self.da_backlog_bytes.saturating_add(da_bytes);
        self.growth_tracker.add_sample(da_bytes);

        let contribution = BlockContribution { block_number, da_bytes, timestamp };
        self.block_contributions.push_front(contribution);
        if self.block_contributions.len() > MAX_HISTORY {
            self.block_contributions.pop_back();
        }
    }

    /// Updates an existing block's DA bytes with accurate data from a full fetch.
    pub(crate) fn update_block_info(
        &mut self,
        block_number: u64,
        accurate_da_bytes: u64,
        timestamp: u64,
    ) {
        for contrib in &mut self.block_contributions {
            if contrib.block_number == block_number {
                let diff = accurate_da_bytes as i64 - contrib.da_bytes as i64;
                contrib.da_bytes = accurate_da_bytes;
                contrib.timestamp = timestamp;

                if block_number > self.safe_l2_block {
                    if diff > 0 {
                        self.da_backlog_bytes = self.da_backlog_bytes.saturating_add(diff as u64);
                    } else {
                        self.da_backlog_bytes =
                            self.da_backlog_bytes.saturating_sub((-diff) as u64);
                    }
                }
                return;
            }
        }

        // Block not found - insert it in sorted position (gap fill)
        let contribution =
            BlockContribution { block_number, da_bytes: accurate_da_bytes, timestamp };

        if block_number > self.safe_l2_block {
            self.da_backlog_bytes = self.da_backlog_bytes.saturating_add(accurate_da_bytes);
        }

        let insert_pos = self
            .block_contributions
            .iter()
            .position(|c| c.block_number < block_number)
            .unwrap_or(self.block_contributions.len());
        self.block_contributions.insert(insert_pos, contribution);

        if self.block_contributions.len() > MAX_HISTORY {
            self.block_contributions.pop_back();
        }
    }

    /// Updates the safe head and subtracts newly safe block bytes from the backlog.
    pub(crate) fn update_safe_head(&mut self, safe_block: u64) {
        if safe_block <= self.safe_l2_block {
            return;
        }

        let old_safe = self.safe_l2_block;
        self.safe_l2_block = safe_block;

        let submitted_bytes: u64 = self
            .block_contributions
            .iter()
            .filter(|c| c.block_number > old_safe && c.block_number <= safe_block)
            .map(|c| c.da_bytes)
            .sum();

        self.da_backlog_bytes = self.da_backlog_bytes.saturating_sub(submitted_bytes);
        self.burn_tracker.add_sample(submitted_bytes);

        self.try_attribute_l2_to_l1();
    }

    /// Records a new L1 block and attempts to attribute L2 blocks to it.
    pub(crate) fn record_l1_block(&mut self, info: L1BlockInfo) {
        if self.l1_blocks.iter().any(|b| b.block_number == info.block_number) {
            return;
        }

        let l1_block = L1Block::from_info(info);

        if l1_block.base_blobs > 0 {
            self.last_base_blob_time = Some(Instant::now());
        }

        self.l1_blocks.push_front(l1_block);
        if self.l1_blocks.len() > MAX_HISTORY {
            self.l1_blocks.pop_back();
        }

        self.try_attribute_l2_to_l1();
    }

    fn try_attribute_l2_to_l1(&mut self) {
        if self.safe_l2_block <= self.last_attributed_safe_l2 {
            return;
        }

        let mut unmatched: Vec<usize> = self
            .l1_blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| b.base_blobs > 0 && b.l2_blocks_submitted.is_none())
            .map(|(i, _)| i)
            .collect();

        if unmatched.is_empty() {
            return;
        }

        // Process oldest first (l1_blocks is newest-first, so reverse)
        unmatched.reverse();

        let total_blobs: u64 = unmatched.iter().map(|&i| self.l1_blocks[i].base_blobs).sum();
        if total_blobs == 0 {
            return;
        }

        let l2_delta = self.safe_l2_block - self.last_attributed_safe_l2;
        let mut cursor = self.last_attributed_safe_l2;

        // Integer apportionment: each entry gets floor(l2_delta * blobs / total_blobs),
        // then distribute remainders by largest fractional part.
        let mut shares: Vec<u64> = Vec::with_capacity(unmatched.len());
        let mut remainders: Vec<(usize, u64)> = Vec::with_capacity(unmatched.len());
        let mut allocated: u64 = 0;

        for (nth, &idx) in unmatched.iter().enumerate() {
            let blobs = self.l1_blocks[idx].base_blobs;
            let floor = l2_delta * blobs / total_blobs;
            // Fractional remainder scaled by total_blobs to avoid floats:
            // remainder = (l2_delta * blobs) % total_blobs
            let frac = (l2_delta * blobs) % total_blobs;
            shares.push(floor);
            remainders.push((nth, frac));
            allocated += floor;
        }

        // Distribute the leftover (l2_delta - allocated) to entries with largest remainders
        let mut leftover = l2_delta - allocated;
        remainders.sort_by(|a, b| b.1.cmp(&a.1));
        for &(nth, _) in &remainders {
            if leftover == 0 {
                break;
            }
            shares[nth] += 1;
            leftover -= 1;
        }

        for (nth, &idx) in unmatched.iter().enumerate() {
            let share = shares[nth];
            if share == 0 {
                // Skip zero-share entries — don't write invalid ranges
                continue;
            }

            let range_start = cursor + 1;
            let range_end = cursor + share;

            let da_bytes: u64 = self
                .block_contributions
                .iter()
                .filter(|c| c.block_number >= range_start && c.block_number <= range_end)
                .map(|c| c.da_bytes)
                .sum();

            let block = &mut self.l1_blocks[idx];
            block.l2_blocks_submitted = Some(share);
            block.l2_da_bytes = Some(da_bytes);
            block.l2_block_range = Some((range_start, range_end));

            cursor += share;
        }

        self.last_attributed_safe_l2 = self.safe_l2_block;
    }

    /// Returns an iterator over L1 blocks matching the given filter.
    pub(crate) fn filtered_l1_blocks(
        &self,
        filter: L1BlockFilter,
    ) -> impl Iterator<Item = &L1Block> {
        self.l1_blocks.iter().filter(move |b| match filter {
            L1BlockFilter::All => true,
            L1BlockFilter::WithBlobs => b.has_blobs(),
            L1BlockFilter::WithBaseBlobs => b.has_base_blobs(),
        })
    }

    /// Returns the Base batcher's share of total blobs over the last `n` L1 blocks.
    pub(crate) fn base_blob_share(&self, n: usize) -> Option<f64> {
        let blocks: Vec<_> = self.l1_blocks.iter().take(n).collect();
        if blocks.is_empty() {
            return None;
        }
        let total: u64 = blocks.iter().map(|b| b.total_blobs).sum();
        let base: u64 = blocks.iter().map(|b| b.base_blobs).sum();
        if total > 0 { Some(base as f64 / total as f64) } else { None }
    }

    /// Returns the blob target usage ratio over the last `n` L1 blocks.
    pub(crate) fn blob_target_usage(&self, n: usize, l1_blob_target: u64) -> Option<f64> {
        let blocks: Vec<_> = self.l1_blocks.iter().take(n).collect();
        if blocks.is_empty() || l1_blob_target == 0 {
            return None;
        }
        let total_blobs: u64 = blocks.iter().map(|b| b.total_blobs).sum();
        let expected = blocks.len() as f64 * l1_blob_target as f64;
        Some(total_blobs as f64 / expected)
    }
}

// =============================================================================
// Formatting Functions
// =============================================================================

/// Formats a byte count into a human-readable string (e.g. "1.5M").
pub(crate) fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1}G", bytes as f64 / 1_000_000_000.0)
    } else if bytes >= 1_000_000 {
        format!("{:.1}M", bytes as f64 / 1_000_000.0)
    } else if bytes >= 1_000 {
        format!("{:.0}K", bytes as f64 / 1_000.0)
    } else {
        format!("{bytes}B")
    }
}

/// Formats a gas value into a human-readable string (e.g. "30.0M").
pub(crate) fn format_gas(gas: u64) -> String {
    if gas >= 1_000_000 {
        format!("{:.1}M", gas as f64 / 1_000_000.0)
    } else if gas >= 1_000 {
        format!("{:.0}K", gas as f64 / 1_000.0)
    } else {
        gas.to_string()
    }
}

/// Truncates a block number to fit within `max_width` characters.
pub(crate) fn truncate_block_number(block_number: u64, max_width: usize) -> String {
    let s = block_number.to_string();
    if s.len() <= max_width { s } else { format!("…{}", &s[s.len() - (max_width - 1)..]) }
}

/// Formats a duration into a compact human-readable string (e.g. "2m30s").
pub(crate) fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 3600 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

/// Formats a byte rate into a human-readable string (e.g. "1.2K/s").
pub(crate) fn format_rate(rate: Option<f64>) -> String {
    match rate {
        Some(r) if r >= 1_000_000.0 => format!("{:.1}M/s", r / 1_000_000.0),
        Some(r) if r >= 1_000.0 => format!("{:.1}K/s", r / 1_000.0),
        Some(r) => format!("{r:.0}B/s"),
        None => "-".to_string(),
    }
}

/// Formats a wei value as gwei with appropriate precision.
pub(crate) fn format_gwei(wei: u128) -> String {
    let gwei = wei as f64 / 1_000_000_000.0;
    if gwei >= 1.0 { format!("{gwei:.2} gwei") } else { format!("{gwei:.4} gwei") }
}

const BACKLOG_THRESHOLDS: &[(u64, Color)] = &[
    (5_000_000, Color::Rgb(100, 200, 100)),
    (10_000_000, Color::Rgb(150, 220, 100)),
    (20_000_000, Color::Rgb(200, 220, 80)),
    (30_000_000, Color::Rgb(240, 200, 60)),
    (45_000_000, Color::Rgb(255, 160, 60)),
    (60_000_000, Color::Rgb(255, 100, 80)),
];

/// Returns a color indicating backlog severity based on byte count.
pub(crate) fn backlog_size_color(bytes: u64) -> Color {
    BACKLOG_THRESHOLDS
        .iter()
        .find(|(threshold, _)| bytes < *threshold)
        .map_or(Color::Rgb(255, 80, 120), |(_, color)| *color)
}

/// Returns a unique color for the given block number.
pub(crate) const fn block_color(block_number: u64) -> Color {
    BLOCK_COLORS[(block_number as usize) % BLOCK_COLORS.len()]
}

/// Returns a brightened version of the block color for emphasis.
pub(crate) const fn block_color_bright(block_number: u64) -> Color {
    let Color::Rgb(r, g, b) = BLOCK_COLORS[(block_number as usize) % BLOCK_COLORS.len()] else {
        unreachable!()
    };
    Color::Rgb(
        r.saturating_add((255 - r) / 2),
        g.saturating_add((255 - g) / 2),
        b.saturating_add((255 - b) / 2),
    )
}

const fn dim_color(color: Color, opacity: f64) -> Color {
    let Color::Rgb(r, g, b) = color else {
        return color;
    };
    Color::Rgb((r as f64 * opacity) as u8, (g as f64 * opacity) as u8, (b as f64 * opacity) as u8)
}

const GAS_COLOR_WARM: (u8, u8, u8) = (255, 200, 80);
const GAS_COLOR_HOT: (u8, u8, u8) = (255, 60, 60);

/// Builds a styled gas usage bar line with target marker.
pub(crate) fn build_gas_bar(
    gas_used: u64,
    gas_limit: u64,
    elasticity: u64,
    bar_chars: usize,
) -> Line<'static> {
    if gas_limit == 0 {
        return Line::from("-".to_string());
    }

    let bar_units = bar_chars * 8;
    let gas_target = gas_limit / elasticity;
    let target_char = ((gas_target as f64 / gas_limit as f64) * bar_chars as f64).round() as usize;

    let filled_units = ((gas_used as f64 / gas_limit as f64) * bar_units as f64).ceil() as usize;
    let filled_units = filled_units.min(bar_units);

    let target_units = target_char * 8;
    let excess_chars = bar_chars.saturating_sub(target_char).max(1);

    let excess_color = |char_idx: usize| -> Color {
        let t = (char_idx - target_char) as f64 / excess_chars as f64;
        lerp_rgb(GAS_COLOR_WARM, GAS_COLOR_HOT, t.clamp(0.0, 1.0))
    };

    let mut spans = Vec::new();
    let mut current_units = 0;

    for char_idx in 0..bar_chars {
        let char_end_units = (char_idx + 1) * 8;

        if char_idx == target_char {
            if filled_units <= target_units {
                spans.push(Span::styled("▏", Style::default().fg(COLOR_TARGET)));
            } else {
                let over_units = filled_units.saturating_sub(target_units).min(8);
                let color = excess_color(char_idx);
                if over_units >= 8 {
                    spans.push(Span::styled("█", Style::default().fg(color)));
                } else {
                    let opacity = over_units as f64 / 8.0;
                    let dimmed = dim_color(color, opacity);
                    spans.push(Span::styled(
                        EIGHTH_BLOCKS[over_units - 1].to_string(),
                        Style::default().fg(dimmed),
                    ));
                }
            }
        } else if current_units >= filled_units {
            spans.push(Span::raw(" "));
        } else if char_end_units <= filled_units {
            let fill_color =
                if char_idx < target_char { COLOR_GAS_FILL } else { excess_color(char_idx) };
            spans.push(Span::styled("█", Style::default().fg(fill_color)));
        } else {
            let units_in_char = filled_units - current_units;
            let opacity = units_in_char as f64 / 8.0;
            let fill_color =
                if char_idx < target_char { COLOR_GAS_FILL } else { excess_color(char_idx) };
            let dimmed = dim_color(fill_color, opacity);
            spans.push(Span::styled(
                EIGHTH_BLOCKS[units_in_char - 1].to_string(),
                Style::default().fg(dimmed),
            ));
        }

        current_units = char_end_units;
    }

    Line::from(spans)
}

/// Parameters for rendering the L1 blocks table.
#[derive(Debug)]
pub(crate) struct L1BlocksTableParams<'a, I: Iterator<Item = &'a L1Block>> {
    /// Iterator over L1 blocks to display.
    pub l1_blocks: I,
    /// Whether this panel is the active (focused) panel.
    pub is_active: bool,
    /// Table selection state.
    pub table_state: &'a mut TableState,
    /// Active L1 block filter.
    pub filter: L1BlockFilter,
    /// Title displayed in the panel border.
    pub title: &'a str,
    /// Current L1 connection mode indicator.
    pub connection_mode: Option<L1ConnectionMode>,
}

/// Renders the L1 blocks table panel.
pub(crate) fn render_l1_blocks_table<'a>(
    f: &mut Frame<'_>,
    area: Rect,
    params: L1BlocksTableParams<'a, impl Iterator<Item = &'a L1Block>>,
) {
    let L1BlocksTableParams { l1_blocks, is_active, table_state, filter, title, connection_mode } =
        params;
    let border_color = if is_active { Color::Rgb(255, 100, 100) } else { Color::Red };

    let filter_label = match filter {
        L1BlockFilter::All => String::new(),
        other => format!(" [{}]", other.label()),
    };
    let mode_label = match connection_mode {
        Some(L1ConnectionMode::WebSocket) => " WS",
        Some(L1ConnectionMode::Polling) => " Poll",
        None => "",
    };
    let block = Block::default()
        .title(format!(" {title}{filter_label}{mode_label} "))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let header_style = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    let header = Row::new(vec![
        Cell::from("L1 Blk").style(header_style),
        Cell::from("Blobs").style(header_style),
        Cell::from("L2").style(header_style),
        Cell::from("Ratio").style(header_style),
        Cell::from("Age").style(header_style),
    ]);

    let fixed_cols_width = 5 + 4 + 6 + 5 + 4;
    let l1_col_width = inner.width.saturating_sub(fixed_cols_width).clamp(4, 9) as usize;

    let selected_row = table_state.selected();

    let rows: Vec<Row<'_>> = l1_blocks
        .enumerate()
        .map(|(idx, l1_block)| {
            let is_selected = is_active && selected_row == Some(idx);

            let style = if is_selected {
                Style::default().fg(Color::White).bg(COLOR_ROW_SELECTED)
            } else {
                Style::default().fg(Color::White)
            };

            let blobs_style = if l1_block.base_blobs > 0 {
                Style::default().fg(COLOR_BASE_BLUE)
            } else if l1_block.total_blobs > 0 {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            Row::new(vec![
                Cell::from(l1_block.block_display(l1_col_width)),
                Cell::from(l1_block.blobs_display()).style(blobs_style),
                Cell::from(l1_block.l2_blocks_display()),
                Cell::from(l1_block.compression_display()),
                Cell::from(l1_block.age_display()),
            ])
            .style(style)
        })
        .collect();

    let widths = [
        Constraint::Max(9),
        Constraint::Length(5),
        Constraint::Length(4),
        Constraint::Length(6),
        Constraint::Min(5),
    ];

    let table = Table::new(rows, widths).header(header);
    f.render_stateful_widget(table, inner, table_state);
}

/// Renders a horizontal bar showing the DA backlog with per-block coloring.
pub(crate) fn render_da_backlog_bar(
    f: &mut Frame<'_>,
    area: Rect,
    tracker: &DaTracker,
    loading: Option<&LoadingState>,
    loaded: bool,
    highlighted_block: Option<u64>,
) {
    let block = Block::default()
        .title(" DA Backlog ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.width < 10 || inner.height < 1 {
        return;
    }

    let bar_width = inner.width.saturating_sub(12) as usize;

    if !loaded {
        let (line1, line2) = match loading {
            Some(ls) if ls.total_blocks > 0 => {
                let pct = (ls.current_block as f64 / ls.total_blocks as f64 * 100.0) as u64;
                let filled = (pct as usize * bar_width / 100).min(bar_width);
                let bar = format!("{}{}", "█".repeat(filled), "░".repeat(bar_width - filled));
                (
                    Line::from(Span::styled(bar, Style::default().fg(Color::Cyan))),
                    Line::from(Span::styled(
                        format!(" Loading {}/{}", ls.current_block, ls.total_blocks),
                        Style::default().fg(Color::Cyan),
                    )),
                )
            }
            _ => (
                Line::from(Span::styled(
                    "░".repeat(bar_width),
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(Span::styled(" Loading...", Style::default().fg(Color::Yellow))),
            ),
        };
        let para = Paragraph::new(vec![line1, line2]);
        f.render_widget(para, inner);
        return;
    }

    let backlog_blocks: Vec<_> = tracker
        .block_contributions
        .iter()
        .filter(|c| c.block_number > tracker.safe_l2_block)
        .collect();

    if backlog_blocks.is_empty() || tracker.da_backlog_bytes == 0 {
        let empty_bar = "░".repeat(bar_width);
        let text = format!("{empty_bar} {:>8}", format_bytes(0));
        let para = Paragraph::new(text).style(Style::default().fg(Color::DarkGray));
        f.render_widget(para, inner);
        return;
    }

    let total_backlog = tracker.da_backlog_bytes;
    let mut spans: Vec<Span<'_>> = Vec::new();
    let mut chars_used = 0usize;

    for contrib in backlog_blocks.iter().rev() {
        let color = block_color(contrib.block_number);
        let is_highlighted = highlighted_block == Some(contrib.block_number);

        let proportion = contrib.da_bytes as f64 / total_backlog as f64;
        let char_count = ((proportion * bar_width as f64).round() as usize).max(1);
        let char_count = char_count.min(bar_width - chars_used);

        if char_count > 0 {
            let style = if is_highlighted {
                Style::default().fg(Color::White).bg(color)
            } else {
                Style::default().fg(color)
            };
            let glyph = if is_highlighted { "⣿" } else { "█" };
            spans.push(Span::styled(glyph.repeat(char_count), style));
            chars_used += char_count;
        }

        if chars_used >= bar_width {
            break;
        }
    }

    if chars_used < bar_width {
        spans.push(Span::styled(
            "░".repeat(bar_width - chars_used),
            Style::default().fg(Color::DarkGray),
        ));
    }

    let backlog_color = backlog_size_color(total_backlog);
    spans.push(Span::styled(
        format!(" {:>8}", format_bytes(total_backlog)),
        Style::default().fg(backlog_color).add_modifier(Modifier::BOLD),
    ));

    let line = Line::from(spans);
    let para = Paragraph::new(line);
    f.render_widget(para, inner);
}

/// Renders a horizontal bar showing aggregate gas usage across recent blocks.
pub(crate) fn render_gas_usage_bar(
    f: &mut Frame<'_>,
    area: Rect,
    entries: &VecDeque<FlashblockEntry>,
    elasticity: u64,
    highlighted_block: Option<u64>,
) {
    let mut block_gas: Vec<(u64, u64)> = Vec::new();
    for entry in entries {
        if let Some(last) = block_gas.last_mut()
            && last.0 == entry.block_number
        {
            last.1 = last.1.max(entry.gas_used);
            continue;
        }
        block_gas.push((entry.block_number, entry.gas_used));
    }

    let n_label = block_gas.len();
    let title_widget = Block::default()
        .title(format!(" Gas Usage ({n_label} blocks) "))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = title_widget.inner(area);
    f.render_widget(title_widget, area);

    if inner.width < 10 || inner.height < 1 {
        return;
    }

    let bar_width = inner.width.saturating_sub(12) as usize;

    if block_gas.is_empty() {
        let empty_bar = "░".repeat(bar_width);
        let text = format!("{empty_bar} {:>5}", "0%");
        let para = Paragraph::new(text).style(Style::default().fg(Color::DarkGray));
        f.render_widget(para, inner);
        return;
    }

    let n_blocks = block_gas.len() as u64;
    let gas_limit = entries.front().map(|e| e.gas_limit).unwrap_or(0);
    let per_block_target = if elasticity > 0 && gas_limit > 0 { gas_limit / elasticity } else { 0 };
    let total_target = per_block_target * n_blocks;
    let total_limit = gas_limit * n_blocks;
    let total_gas: u64 = block_gas.iter().map(|(_, g)| *g).sum();

    let half = bar_width / 2;
    let target_char = half;

    let gas_to_chars = |gas: u64| -> f64 {
        if total_target == 0 {
            return 0.0;
        }
        let g = gas as f64;
        let t = total_target as f64;
        let l = total_limit as f64;
        if g <= t {
            (g / t) * half as f64
        } else {
            half as f64 + ((g - t) / (l - t)) * (bar_width - half) as f64
        }
    };

    let mut spans: Vec<Span<'_>> = Vec::new();
    let mut chars_used = 0usize;
    let mut cumulative_gas = 0u64;

    for &(block_number, gas_used) in block_gas.iter().rev() {
        if chars_used >= bar_width {
            break;
        }

        let color = block_color(block_number);
        let is_highlighted = highlighted_block == Some(block_number);

        let pos_before = gas_to_chars(cumulative_gas).round() as usize;
        cumulative_gas += gas_used;
        let pos_after = gas_to_chars(cumulative_gas).round() as usize;
        let char_count = pos_after.saturating_sub(pos_before).max(1).min(bar_width - chars_used);

        if char_count > 0 {
            let style = if is_highlighted {
                Style::default().fg(Color::White).bg(color)
            } else {
                Style::default().fg(color)
            };
            let glyph = if is_highlighted { "⣿" } else { "█" };

            if target_char > chars_used && target_char < chars_used + char_count {
                let before = target_char - chars_used;
                let after = char_count - before - 1;
                if before > 0 {
                    spans.push(Span::styled(glyph.repeat(before), style));
                }
                spans.push(Span::styled("│", Style::default().fg(COLOR_TARGET).bg(color)));
                if after > 0 {
                    spans.push(Span::styled(glyph.repeat(after), style));
                }
            } else {
                spans.push(Span::styled(glyph.repeat(char_count), style));
            }
            chars_used += char_count;
        }
    }

    while chars_used < bar_width {
        if chars_used == target_char {
            spans.push(Span::styled("│", Style::default().fg(COLOR_TARGET)));
        } else {
            spans.push(Span::styled("░", Style::default().fg(Color::DarkGray)));
        }
        chars_used += 1;
    }

    let usage_ratio = if total_target > 0 { total_gas as f64 / total_target as f64 } else { 0.0 };
    spans.push(Span::styled(
        format!(" {:>5.0}%", usage_ratio * 100.0),
        Style::default().fg(target_usage_color(usage_ratio)).add_modifier(Modifier::BOLD),
    ));

    let line = Line::from(spans);
    let para = Paragraph::new(line);
    f.render_widget(para, inner);
}

const TARGET_USAGE_MAX: f64 = 1.5;

/// Returns a color representing how close usage is to the target (blue to red).
pub(crate) fn target_usage_color(usage: f64) -> Color {
    let t = usage.clamp(0.0, TARGET_USAGE_MAX);
    if t <= 1.0 {
        lerp_rgb((0, 100, 255), (255, 255, 0), t)
    } else {
        lerp_rgb((255, 255, 0), (255, 0, 0), (t - 1.0) / (TARGET_USAGE_MAX - 1.0))
    }
}

const fn lerp_rgb(a: (u8, u8, u8), b: (u8, u8, u8), t: f64) -> Color {
    Color::Rgb(
        (a.0 as f64 + (b.0 as f64 - a.0 as f64) * t) as u8,
        (a.1 as f64 + (b.1 as f64 - a.1 as f64) * t) as u8,
        (a.2 as f64 + (b.2 as f64 - a.2 as f64) * t) as u8,
    )
}

const FLASHBLOCK_TARGET_MS: i64 = 200;
const FLASHBLOCK_TOLERANCE_MS: i64 = 50;

/// Returns a color indicating how close a time delta is to the 200ms target.
pub(crate) fn time_diff_color(ms: i64) -> Color {
    let target = FLASHBLOCK_TARGET_MS;
    let tol = FLASHBLOCK_TOLERANCE_MS;
    if (target - tol..=target + tol).contains(&ms) {
        Color::Green
    } else if (target - 2 * tol..target - tol).contains(&ms) {
        Color::Blue
    } else if ms < target - 2 * tol {
        Color::Magenta
    } else if (target + tol..target + 2 * tol).contains(&ms) {
        Color::Yellow
    } else {
        Color::Red
    }
}

// =============================================================================
// Receipt Log
// =============================================================================

/// A single log entry from a transaction receipt.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct ReceiptLog {
    /// The contract address that emitted this log.
    pub address: Address,
    /// Indexed log topics. `topics[0]` is the event signature hash.
    pub topics: Vec<B256>,
    /// Non-indexed ABI-encoded event data.
    pub data: Bytes,
}

// =============================================================================
// Event Activity Bar — Filters and State
// =============================================================================

/// Number of top-level event groups.
pub(crate) const EVENT_GROUP_COUNT: usize = 6;

/// Total number of sub-filters across all groups.
pub(crate) const SUB_FILTER_TOTAL: usize = 19;

/// Total number of rows in the hierarchical filter menu (groups + sub-filters).
pub(crate) const FILTER_MENU_ITEMS: usize = EVENT_GROUP_COUNT + SUB_FILTER_TOTAL;

/// Shared state for the hierarchical event filter popup menu.
///
/// Embedded by views that offer event filter toggling (command center,
/// flashblocks, `DeFi` activity). The view translates key events into method
/// calls; this struct owns cursor position and open/closed state.
#[derive(Debug, Clone, Default)]
pub(crate) struct FilterMenuState {
    /// Whether the filter popup is currently visible.
    pub open: bool,
    /// Current cursor position in the flat menu (`0..FILTER_MENU_ITEMS`).
    pub cursor: usize,
}

impl FilterMenuState {
    /// Moves the cursor up one row, clamping at the top.
    pub(crate) const fn move_up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Moves the cursor down one row, clamping at the bottom.
    pub(crate) const fn move_down(&mut self) {
        if self.cursor + 1 < FILTER_MENU_ITEMS {
            self.cursor += 1;
        }
    }

    /// Toggles the item under the cursor (group header or sub-filter).
    pub(crate) fn toggle(&self, activity: &mut ActivityBarState) {
        let (group_idx, sub_idx) = cursor_to_filter(self.cursor);
        match sub_idx {
            None => activity.toggle_group(group_idx),
            Some(si) => {
                let idx = EVENT_GROUP_DEFS[group_idx].sub_offset + si;
                activity.active[idx] = !activity.active[idx];
            }
        }
    }
}

/// Maximum number of blocks retained in the activity bar rolling window.
///
/// Sized so sparklines fill most of a wide terminal (~5 minutes of 2 s blocks).
pub(crate) const ACTIVITY_WINDOW_BLOCKS: usize = 150;

// Event topic[0] hashes (keccak256 of the event ABI signature).

/// `Transfer(address,address,uint256)` — ERC-20 token transfer.
const TOPIC_ERC20_TRANSFER: [u8; 32] = [
    0xdd, 0xf2, 0x52, 0xad, 0x1b, 0xe2, 0xc8, 0x9b, 0x69, 0xc2, 0xb0, 0x68, 0xfc, 0x37, 0x8d, 0xaa,
    0x95, 0x2b, 0xa7, 0xf1, 0x63, 0xc4, 0xa1, 0x16, 0x28, 0xf5, 0x5a, 0x4d, 0xf5, 0x23, 0xb3, 0xef,
];

/// `Swap(address,uint256,uint256,uint256,uint256,address)` — Uniswap V2-style AMM swap
/// (also emitted by Aerodrome, Velodrome, `SushiSwap` V2, and compatible forks).
const TOPIC_SWAP_V2: [u8; 32] = [
    0xd7, 0x8a, 0xd9, 0x5f, 0xa4, 0x6c, 0x99, 0x4b, 0x65, 0x51, 0xd0, 0xda, 0x85, 0xfc, 0x27, 0x5f,
    0xe6, 0x13, 0xce, 0x37, 0x65, 0x7f, 0xb8, 0xd5, 0xe3, 0xd1, 0x30, 0x84, 0x01, 0x59, 0xd8, 0x22,
];

/// `Swap(address,address,int256,int256,uint160,uint128,int24)` — Uniswap V3 concentrated-liquidity swap.
const TOPIC_SWAP_V3: [u8; 32] = [
    0xc4, 0x20, 0x79, 0xf9, 0x4a, 0x63, 0x50, 0xd7, 0xe6, 0x23, 0x5f, 0x29, 0x17, 0x49, 0x24, 0xf9,
    0x28, 0xcc, 0x2a, 0xc8, 0x18, 0xeb, 0x64, 0xfe, 0xd8, 0x00, 0x4e, 0x11, 0x5f, 0xbc, 0xca, 0x67,
];

/// `Swap(bytes32,address,int128,int128,uint160,uint128,int24,uint24)` — Uniswap V4 `PoolManager` swap.
const TOPIC_SWAP_V4: [u8; 32] = [
    0x40, 0xe9, 0xce, 0xcb, 0x9f, 0x5f, 0x1f, 0x1c, 0x5b, 0x9c, 0x97, 0xde, 0xc2, 0x91, 0x7b, 0x7e,
    0xe9, 0x2e, 0x57, 0xba, 0x55, 0x63, 0x70, 0x8d, 0xac, 0xa9, 0x4d, 0xd8, 0x4a, 0xd7, 0x11, 0x2f,
];

// Pool liquidity event topics.

/// `Mint(address,uint256,uint256)` — Uniswap V2 / Aerodrome add liquidity.
const TOPIC_MINT_V2: [u8; 32] = [
    0x4c, 0x20, 0x9b, 0x5f, 0xc8, 0xad, 0x50, 0x75, 0x8f, 0x13, 0xe2, 0xe1, 0x08, 0x8b, 0xa5, 0x6a,
    0x56, 0x0d, 0xff, 0x69, 0x0a, 0x1c, 0x6f, 0xef, 0x26, 0x39, 0x4f, 0x4c, 0x03, 0x82, 0x1c, 0x4f,
];

/// `Burn(address,uint256,uint256,address)` — Uniswap V2 / Aerodrome remove liquidity.
const TOPIC_BURN_V2: [u8; 32] = [
    0xdc, 0xcd, 0x41, 0x2f, 0x0b, 0x12, 0x52, 0x81, 0x9c, 0xb1, 0xfd, 0x33, 0x0b, 0x93, 0x22, 0x4c,
    0xa4, 0x26, 0x12, 0x89, 0x2b, 0xb3, 0xf4, 0xf7, 0x89, 0x97, 0x6e, 0x6d, 0x81, 0x93, 0x64, 0x96,
];

/// `Mint(address,address,int24,int24,uint128,uint256,uint256)` — Uniswap V3 add concentrated liquidity.
const TOPIC_MINT_V3: [u8; 32] = [
    0x7a, 0x53, 0x08, 0x0b, 0xa4, 0x14, 0x15, 0x8b, 0xe7, 0xec, 0x69, 0xb9, 0x87, 0xb5, 0xfb, 0x7d,
    0x07, 0xde, 0xe1, 0x01, 0xfe, 0x85, 0x48, 0x8f, 0x08, 0x53, 0xae, 0x16, 0x23, 0x9d, 0x0b, 0xde,
];

/// `Burn(address,int24,int24,uint128,uint256,uint256)` — Uniswap V3 remove concentrated liquidity.
const TOPIC_BURN_V3: [u8; 32] = [
    0x0c, 0x39, 0x6c, 0xd9, 0x89, 0xa3, 0x9f, 0x44, 0x59, 0xb5, 0xfa, 0x1a, 0xed, 0x6a, 0x9a, 0x8d,
    0xcd, 0xbc, 0x45, 0x90, 0x8a, 0xcf, 0xd6, 0x7e, 0x02, 0x8c, 0xd5, 0x68, 0xda, 0x98, 0x98, 0x2c,
];

/// `ModifyLiquidity(bytes32,address,int24,int24,int256,bytes32)` — Uniswap V4 add/remove liquidity.
const TOPIC_MODIFY_LIQUIDITY_V4: [u8; 32] = [
    0xf2, 0x08, 0xf4, 0x91, 0x27, 0x82, 0xfd, 0x25, 0xc7, 0xf1, 0x14, 0xca, 0x37, 0x23, 0xa2, 0xd5,
    0xdd, 0x6f, 0x3b, 0xcc, 0x3a, 0xc8, 0xdb, 0x5a, 0xf6, 0x3b, 0xaa, 0x85, 0xf7, 0x11, 0xd5, 0xec,
];

// Lending protocol event topics.

/// `Supply(address,address,address,uint256,uint16)` — Aave V3 / Seamless supply.
const TOPIC_SUPPLY_AAVE: [u8; 32] = [
    0x2b, 0x62, 0x77, 0x36, 0xbc, 0xa1, 0x5c, 0xd5, 0x38, 0x1d, 0xcf, 0x80, 0xb0, 0xbf, 0x11, 0xfd,
    0x19, 0x7d, 0x01, 0xa0, 0x37, 0xc5, 0x2b, 0x92, 0x7a, 0x88, 0x1a, 0x10, 0xfb, 0x73, 0xba, 0x61,
];

/// `Withdraw(address,address,address,uint256)` — Aave V3 / Seamless withdraw.
const TOPIC_WITHDRAW_AAVE: [u8; 32] = [
    0x31, 0x15, 0xd1, 0x44, 0x9a, 0x7b, 0x73, 0x2c, 0x98, 0x6c, 0xba, 0x18, 0x24, 0x4e, 0x89, 0x7a,
    0x45, 0x0f, 0x61, 0xe1, 0xbb, 0x8d, 0x58, 0x9c, 0xd2, 0xe6, 0x9e, 0x6c, 0x89, 0x24, 0xf9, 0xf7,
];

/// `Borrow(address,address,address,uint256,uint8,uint256,uint16)` — Aave V3 / Seamless borrow.
const TOPIC_BORROW_AAVE: [u8; 32] = [
    0xb3, 0xd0, 0x84, 0x82, 0x0f, 0xb1, 0xa9, 0xde, 0xcf, 0xfb, 0x17, 0x64, 0x36, 0xbd, 0x02, 0x55,
    0x8d, 0x15, 0xfa, 0xc9, 0xb0, 0xdd, 0xfe, 0xd8, 0xc4, 0x65, 0xbc, 0x73, 0x59, 0xd7, 0xdc, 0xe0,
];

/// `Repay(address,address,address,uint256,bool)` — Aave V3 / Seamless repay.
const TOPIC_REPAY_AAVE: [u8; 32] = [
    0xa5, 0x34, 0xc8, 0xdb, 0xe7, 0x1f, 0x87, 0x1f, 0x9f, 0x35, 0x30, 0xe9, 0x7a, 0x74, 0x60, 0x1f,
    0xea, 0x17, 0xb4, 0x26, 0xca, 0xe0, 0x2e, 0x1c, 0x5a, 0xee, 0x42, 0xc9, 0x6c, 0x78, 0x40, 0x51,
];

/// `Supply(address,address,uint256)` — Compound V3 / Moonwell supply.
const TOPIC_SUPPLY_COMPOUND: [u8; 32] = [
    0xd1, 0xcf, 0x3d, 0x15, 0x6d, 0x5f, 0x8f, 0x0d, 0x50, 0xf6, 0xc1, 0x22, 0xed, 0x60, 0x9c, 0xec,
    0x09, 0xd3, 0x5c, 0x9b, 0x9f, 0xb3, 0xff, 0xf6, 0xea, 0x09, 0x59, 0x13, 0x4d, 0xae, 0x42, 0x4e,
];

/// `SupplyCollateral(address,address,address,uint256)` — Compound V3 / Moonwell supply collateral.
const TOPIC_SUPPLY_COLLATERAL_COMPOUND: [u8; 32] = [
    0xfa, 0x56, 0xf7, 0xb2, 0x4f, 0x17, 0x18, 0x3d, 0x81, 0x89, 0x4d, 0x3a, 0xc2, 0xee, 0x65, 0x4e,
    0x3c, 0x26, 0x38, 0x8d, 0x17, 0xa2, 0x8d, 0xbd, 0x95, 0x49, 0xb8, 0x11, 0x43, 0x04, 0xe1, 0xf4,
];

/// `Withdraw(address,address,uint256)` — Compound V3 / Moonwell withdraw.
const TOPIC_WITHDRAW_COMPOUND: [u8; 32] = [
    0x9b, 0x1b, 0xfa, 0x7f, 0xa9, 0xee, 0x42, 0x0a, 0x16, 0xe1, 0x24, 0xf7, 0x94, 0xc3, 0x5a, 0xc9,
    0xf9, 0x04, 0x72, 0xac, 0xc9, 0x91, 0x40, 0xeb, 0x2f, 0x64, 0x47, 0xc7, 0x14, 0xca, 0xd8, 0xeb,
];

/// `WithdrawCollateral(address,address,address,uint256)` — Compound V3 / Moonwell withdraw collateral.
const TOPIC_WITHDRAW_COLLATERAL_COMPOUND: [u8; 32] = [
    0xd6, 0xd4, 0x80, 0xd5, 0xb3, 0x06, 0x8d, 0xb0, 0x03, 0x53, 0x3b, 0x17, 0x0d, 0x67, 0x56, 0x14,
    0x94, 0xd7, 0x2e, 0x3b, 0xf9, 0xfa, 0x40, 0xa2, 0x66, 0x47, 0x13, 0x51, 0xeb, 0xba, 0x9e, 0x16,
];

/// `Supply(bytes32,address,address,uint256,uint256)` — Morpho Blue supply.
const TOPIC_SUPPLY_MORPHO: [u8; 32] = [
    0xed, 0xf8, 0x87, 0x04, 0x33, 0xc8, 0x38, 0x23, 0xeb, 0x07, 0x1d, 0x3d, 0xf1, 0xca, 0xa8, 0xd0,
    0x08, 0xf1, 0x2f, 0x64, 0x40, 0x91, 0x8c, 0x20, 0xd7, 0x5a, 0x36, 0x02, 0xcd, 0xa3, 0x0f, 0xe0,
];

/// `Withdraw(bytes32,address,address,address,uint256,uint256)` — Morpho Blue withdraw.
const TOPIC_WITHDRAW_MORPHO: [u8; 32] = [
    0xa5, 0x6f, 0xc0, 0xad, 0x57, 0x02, 0xec, 0x05, 0xce, 0x63, 0x66, 0x62, 0x21, 0xf7, 0x96, 0xfb,
    0x62, 0x43, 0x7c, 0x32, 0xdb, 0x1a, 0xa1, 0xaa, 0x07, 0x5f, 0xc6, 0x48, 0x4c, 0xf5, 0x8f, 0xbf,
];

/// `Borrow(bytes32,address,address,address,uint256,uint256)` — Morpho Blue borrow.
const TOPIC_BORROW_MORPHO: [u8; 32] = [
    0x57, 0x09, 0x54, 0x54, 0x0b, 0xed, 0x6b, 0x13, 0x04, 0xa8, 0x7d, 0xfe, 0x81, 0x5a, 0x5e, 0xda,
    0x4a, 0x64, 0x8f, 0x70, 0x97, 0xa1, 0x62, 0x40, 0xdc, 0xd8, 0x5c, 0x9b, 0x5f, 0xd4, 0x2a, 0x43,
];

/// `Repay(bytes32,address,address,uint256,uint256)` — Morpho Blue repay.
const TOPIC_REPAY_MORPHO: [u8; 32] = [
    0x52, 0xac, 0xb0, 0x5c, 0xeb, 0xbd, 0x3c, 0xd3, 0x97, 0x15, 0x46, 0x9f, 0x22, 0xaf, 0xbf, 0x5a,
    0x17, 0x49, 0x62, 0x95, 0xef, 0x3b, 0xc9, 0xbb, 0x59, 0x44, 0x05, 0x6c, 0x63, 0xcc, 0xaa, 0x09,
];

/// `SupplyCollateral(bytes32,address,address,uint256)` — Morpho Blue supply collateral.
const TOPIC_SUPPLY_COLLATERAL_MORPHO: [u8; 32] = [
    0xa3, 0xb9, 0x47, 0x2a, 0x13, 0x99, 0xe1, 0x7e, 0x12, 0x3f, 0x3c, 0x2e, 0x65, 0x86, 0xc2, 0x3e,
    0x50, 0x41, 0x84, 0xd5, 0x04, 0xde, 0x59, 0xcd, 0xaa, 0x2b, 0x37, 0x5e, 0x88, 0x0c, 0x61, 0x84,
];

/// `WithdrawCollateral(bytes32,address,address,address,uint256)` — Morpho Blue withdraw collateral.
const TOPIC_WITHDRAW_COLLATERAL_MORPHO: [u8; 32] = [
    0xe8, 0x0e, 0xbd, 0x7c, 0xc9, 0x22, 0x3d, 0x73, 0x82, 0xaa, 0xb2, 0xe0, 0xd1, 0xd6, 0x15, 0x5c,
    0x65, 0x65, 0x1f, 0x83, 0xd5, 0x3c, 0x8b, 0x9b, 0x06, 0x90, 0x1d, 0x16, 0x7e, 0x32, 0x11, 0x42,
];

// Euler V2 lending event topics (ERC-4626 standard signatures).

/// `Deposit(address,address,uint256,uint256)` — Euler V2 `EVault` deposit (ERC-4626).
const TOPIC_DEPOSIT_EULER: [u8; 32] = [
    0x8b, 0x77, 0x9e, 0xad, 0xfa, 0xd1, 0x24, 0x9b, 0xa1, 0xe8, 0x78, 0x1e, 0xf5, 0xd8, 0x01, 0xac,
    0x68, 0x3e, 0x76, 0x82, 0x75, 0x88, 0x64, 0x4a, 0x54, 0xd4, 0x27, 0xe6, 0x63, 0x7c, 0x5d, 0x0b,
];

/// `Withdraw(address,address,address,uint256,uint256)` — Euler V2 `EVault` withdraw (ERC-4626).
const TOPIC_WITHDRAW_EULER: [u8; 32] = [
    0x80, 0x4c, 0x4b, 0xd3, 0xd5, 0xc8, 0x58, 0xdd, 0xf0, 0x5e, 0x93, 0xd4, 0x6d, 0x8f, 0xb1, 0x7d,
    0x45, 0x31, 0xd8, 0x11, 0x74, 0xcc, 0x2f, 0x7b, 0x7a, 0xa9, 0xef, 0xdf, 0x9c, 0x36, 0xb8, 0xc2,
];

/// `Borrow(address,uint256)` — Euler V2 `EVault` borrow.
const TOPIC_BORROW_EULER: [u8; 32] = [
    0x1b, 0xd8, 0xa1, 0xb4, 0xbb, 0x68, 0x6c, 0x06, 0x68, 0x60, 0x29, 0xaa, 0x8b, 0x8a, 0xf2, 0x07,
    0xe7, 0x85, 0x6a, 0xbd, 0x0d, 0x43, 0x66, 0x4c, 0x09, 0x93, 0xf1, 0x1a, 0x11, 0x1f, 0x13, 0xb0,
];

/// `Repay(address,uint256)` — Euler V2 `EVault` repay.
const TOPIC_REPAY_EULER: [u8; 32] = [
    0x4d, 0x4d, 0x0e, 0xf7, 0x46, 0x44, 0xa0, 0xd0, 0x18, 0x88, 0xdb, 0x2e, 0x37, 0x21, 0x3a, 0x54,
    0x8b, 0xc6, 0xf9, 0x68, 0x6f, 0xf5, 0xc9, 0xbd, 0xd7, 0x21, 0x3f, 0x6b, 0x9d, 0xa5, 0xb6, 0x92,
];

// Bridge event topics.

/// `ETHBridgeInitiated(address,address,uint256,bytes)` — OP Stack ETH withdrawal (L2 → L1).
const TOPIC_ETH_BRIDGE_INITIATED: [u8; 32] = [
    0x28, 0x49, 0xb4, 0x30, 0x74, 0x09, 0x3a, 0x05, 0x39, 0x6b, 0x6f, 0x2a, 0x93, 0x7d, 0xee, 0x85,
    0x65, 0xb1, 0x5a, 0x48, 0xa7, 0xb3, 0xd4, 0xbf, 0xfb, 0x73, 0x2a, 0x50, 0x17, 0x38, 0x0a, 0xf5,
];

/// `ETHBridgeFinalized(address,address,uint256,bytes)` — OP Stack ETH deposit (L1 → L2).
const TOPIC_ETH_BRIDGE_FINALIZED: [u8; 32] = [
    0x31, 0xb2, 0x16, 0x6f, 0xf6, 0x04, 0xfc, 0x56, 0x72, 0xea, 0x5d, 0xf0, 0x8a, 0x78, 0x08, 0x1d,
    0x2b, 0xc6, 0xd7, 0x46, 0xca, 0xdc, 0xe8, 0x80, 0x74, 0x7f, 0x36, 0x43, 0xd8, 0x19, 0xe8, 0x3d,
];

/// `ERC20BridgeInitiated(address,address,address,address,uint256,bytes)` — OP Stack ERC-20 withdrawal.
const TOPIC_ERC20_BRIDGE_INITIATED: [u8; 32] = [
    0x7f, 0xf1, 0x26, 0xdb, 0x80, 0x24, 0x42, 0x4b, 0xbf, 0xd9, 0x82, 0x6e, 0x8a, 0xb8, 0x2f, 0xf5,
    0x91, 0x36, 0x28, 0x9e, 0xa4, 0x40, 0xb0, 0x4b, 0x39, 0xa0, 0xdf, 0x1b, 0x03, 0xb9, 0xca, 0xbf,
];

/// `ERC20BridgeFinalized(address,address,address,address,uint256,bytes)` — OP Stack ERC-20 deposit.
const TOPIC_ERC20_BRIDGE_FINALIZED: [u8; 32] = [
    0xd5, 0x9c, 0x65, 0xb3, 0x54, 0x45, 0x22, 0x58, 0x35, 0xc8, 0x3f, 0x50, 0xb6, 0xed, 0xe0, 0x6a,
    0x7b, 0xe0, 0x47, 0xd2, 0x2e, 0x35, 0x70, 0x73, 0xe2, 0x50, 0xd9, 0xaf, 0x53, 0x75, 0x18, 0xcd,
];

/// `LiquidationCall(address,address,address,uint256,uint256,address,bool)` — Aave V3 / Seamless.
const TOPIC_LIQUIDATION_AAVE_V3: [u8; 32] = [
    0xe4, 0x13, 0xa3, 0x21, 0xe8, 0x68, 0x1d, 0x83, 0x1f, 0x4d, 0xbc, 0xcb, 0xca, 0x79, 0x0d, 0x29,
    0x52, 0xb5, 0x6f, 0x97, 0x79, 0x08, 0xe4, 0x5b, 0xe3, 0x73, 0x35, 0x53, 0x3e, 0x00, 0x52, 0x86,
];

/// `AbsorbCollateral(address,address,address,uint256,uint256)` — Compound V3 / Moonwell.
const TOPIC_LIQUIDATION_COMPOUND_V3: [u8; 32] = [
    0x98, 0x50, 0xab, 0x1a, 0xf7, 0x51, 0x77, 0xe4, 0xa9, 0x20, 0x1c, 0x65, 0xa2, 0xcf, 0x79, 0x76,
    0xd5, 0xd2, 0x8e, 0x40, 0xef, 0x63, 0x49, 0x4b, 0x44, 0x36, 0x6f, 0x86, 0xb2, 0xf9, 0x41, 0x2e,
];

/// `Liquidate(bytes32,address,address,uint256,uint256,uint256,uint256)` — Morpho Blue.
const TOPIC_LIQUIDATION_MORPHO: [u8; 32] = [
    0x2a, 0x95, 0x6e, 0x32, 0xed, 0x87, 0x87, 0xca, 0xc0, 0x3e, 0x68, 0x01, 0x98, 0xb6, 0xce, 0x3d,
    0xcf, 0xb8, 0x19, 0xaa, 0xd5, 0x78, 0xb3, 0xa5, 0x65, 0x8f, 0x2c, 0x11, 0x0a, 0x7d, 0x7c, 0x55,
];

/// `Liquidate(address,address,address,uint256,uint256)` — Euler V2.
const TOPIC_LIQUIDATION_EULER_V2: [u8; 32] = [
    0x82, 0x46, 0xcc, 0x71, 0xab, 0x01, 0x53, 0x3b, 0x5b, 0xeb, 0xc6, 0x72, 0xa6, 0x36, 0xdf, 0x81,
    0x2f, 0x10, 0x63, 0x7a, 0xd7, 0x20, 0x79, 0x73, 0x19, 0xd5, 0x74, 0x1d, 0x5e, 0xbb, 0x39, 0x62,
];

// =============================================================================
// Volume Tracking — Token Addresses
// =============================================================================

/// USDC contract on Base (6 decimals).
const USDC_ADDRESS: [u8; 20] = [
    0x83, 0x35, 0x89, 0xfc, 0xd6, 0xed, 0xb6, 0xe0, 0x8f, 0x4c, 0x7c, 0x32, 0xd4, 0xf7, 0x1b, 0x54,
    0xbd, 0xa0, 0x29, 0x13,
];

/// WETH contract on Base (18 decimals).
const WETH_ADDRESS: [u8; 20] = [
    0x42, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x06,
];

/// Decodes a big-endian uint256 from the first 32 bytes of ABI-encoded data.
///
/// Returns the value as `u128`; values exceeding `u128::MAX` are clamped.
fn decode_uint256(data: &[u8]) -> u128 {
    if data.len() < 32 {
        return 0;
    }
    // Top 16 bytes — if any are non-zero the value exceeds u128.
    if data[..16].iter().any(|&b| b != 0) {
        return u128::MAX;
    }
    u128::from_be_bytes(data[16..32].try_into().unwrap_or([0; 16]))
}

/// Static definition of a sub-filter within an event group.
pub(crate) struct SubFilterDef {
    /// Display label for this sub-filter.
    pub label: &'static str,
    /// Topic[0] hashes that route to this sub-filter (for topic-based matching).
    pub topics: &'static [[u8; 32]],
    /// Contract addresses that route to this sub-filter (for ERC20 address matching).
    pub addresses: &'static [[u8; 20]],
    /// If true, this is a negative/catch-all filter: matches when no sibling address matches.
    pub catch_all: bool,
}

/// Static definition of a top-level event group for the activity bar.
pub(crate) struct EventGroupDef {
    /// Full label shown when space permits.
    pub label: &'static str,
    /// Short label (≤4 chars) for tight segments.
    pub short_label: &'static str,
    /// Bar fill color for this group.
    pub color: Color,
    /// All topics that match this group (union of sub-filter topics, used for fast first-pass).
    pub topics: &'static [[u8; 32]],
    /// Sub-filter definitions within this group.
    pub sub_filters: &'static [SubFilterDef],
    /// Index of the first sub-filter in the flat `counts`/`active` arrays.
    pub sub_offset: usize,
    /// If true, sub-filters are differentiated by contract address rather than topic.
    pub match_by_address: bool,
}

/// Ordered list of all built-in event group definitions with hierarchical sub-filters.
pub(crate) const EVENT_GROUP_DEFS: [EventGroupDef; EVENT_GROUP_COUNT] = [
    // Group 0: ERC20 Transfers (sub_offset 0..3)
    EventGroupDef {
        label: "ERC20 Xfer",
        short_label: "XFER",
        color: Color::Rgb(0, 200, 150),
        topics: &[TOPIC_ERC20_TRANSFER],
        sub_filters: &[
            SubFilterDef {
                label: "USDC",
                topics: &[TOPIC_ERC20_TRANSFER],
                addresses: &[USDC_ADDRESS],
                catch_all: false,
            },
            SubFilterDef {
                label: "WETH",
                topics: &[TOPIC_ERC20_TRANSFER],
                addresses: &[WETH_ADDRESS],
                catch_all: false,
            },
            SubFilterDef {
                label: "Other",
                topics: &[TOPIC_ERC20_TRANSFER],
                addresses: &[],
                catch_all: true,
            },
        ],
        sub_offset: 0,
        match_by_address: true,
    },
    // Group 1: Swaps (sub_offset 3..6)
    EventGroupDef {
        label: "Swap",
        short_label: "SWAP",
        color: Color::Rgb(0, 150, 255),
        topics: &[TOPIC_SWAP_V2, TOPIC_SWAP_V3, TOPIC_SWAP_V4],
        sub_filters: &[
            SubFilterDef {
                label: "Uni+Aero V2",
                topics: &[TOPIC_SWAP_V2],
                addresses: &[],
                catch_all: false,
            },
            SubFilterDef {
                label: "Uni+Aero V3",
                topics: &[TOPIC_SWAP_V3],
                addresses: &[],
                catch_all: false,
            },
            SubFilterDef {
                label: "Uni V4",
                topics: &[TOPIC_SWAP_V4],
                addresses: &[],
                catch_all: false,
            },
        ],
        sub_offset: 3,
        match_by_address: false,
    },
    // Group 2: Pool Liquidity (sub_offset 6..9)
    EventGroupDef {
        label: "Pool Liquidity",
        short_label: "POOL",
        color: Color::Rgb(255, 200, 50),
        topics: &[
            TOPIC_MINT_V2,
            TOPIC_BURN_V2,
            TOPIC_MINT_V3,
            TOPIC_BURN_V3,
            TOPIC_MODIFY_LIQUIDITY_V4,
        ],
        sub_filters: &[
            SubFilterDef {
                label: "Uni+Aero V2",
                topics: &[TOPIC_MINT_V2, TOPIC_BURN_V2],
                addresses: &[],
                catch_all: false,
            },
            SubFilterDef {
                label: "Uni+Aero V3",
                topics: &[TOPIC_MINT_V3, TOPIC_BURN_V3],
                addresses: &[],
                catch_all: false,
            },
            SubFilterDef {
                label: "Uni V4",
                topics: &[TOPIC_MODIFY_LIQUIDITY_V4],
                addresses: &[],
                catch_all: false,
            },
        ],
        sub_offset: 6,
        match_by_address: false,
    },
    // Group 3: Lending (sub_offset 9..13)
    EventGroupDef {
        label: "Lending",
        short_label: "LEND",
        color: Color::Rgb(100, 200, 255),
        topics: &[
            TOPIC_SUPPLY_AAVE,
            TOPIC_WITHDRAW_AAVE,
            TOPIC_BORROW_AAVE,
            TOPIC_REPAY_AAVE,
            TOPIC_SUPPLY_COMPOUND,
            TOPIC_SUPPLY_COLLATERAL_COMPOUND,
            TOPIC_WITHDRAW_COMPOUND,
            TOPIC_WITHDRAW_COLLATERAL_COMPOUND,
            TOPIC_SUPPLY_MORPHO,
            TOPIC_WITHDRAW_MORPHO,
            TOPIC_BORROW_MORPHO,
            TOPIC_REPAY_MORPHO,
            TOPIC_SUPPLY_COLLATERAL_MORPHO,
            TOPIC_WITHDRAW_COLLATERAL_MORPHO,
            TOPIC_DEPOSIT_EULER,
            TOPIC_WITHDRAW_EULER,
            TOPIC_BORROW_EULER,
            TOPIC_REPAY_EULER,
        ],
        sub_filters: &[
            SubFilterDef {
                label: "Aave",
                topics: &[
                    TOPIC_SUPPLY_AAVE,
                    TOPIC_WITHDRAW_AAVE,
                    TOPIC_BORROW_AAVE,
                    TOPIC_REPAY_AAVE,
                ],
                addresses: &[],
                catch_all: false,
            },
            SubFilterDef {
                label: "Comp/Moon",
                topics: &[
                    TOPIC_SUPPLY_COMPOUND,
                    TOPIC_SUPPLY_COLLATERAL_COMPOUND,
                    TOPIC_WITHDRAW_COMPOUND,
                    TOPIC_WITHDRAW_COLLATERAL_COMPOUND,
                ],
                addresses: &[],
                catch_all: false,
            },
            SubFilterDef {
                label: "Morpho",
                topics: &[
                    TOPIC_SUPPLY_MORPHO,
                    TOPIC_WITHDRAW_MORPHO,
                    TOPIC_BORROW_MORPHO,
                    TOPIC_REPAY_MORPHO,
                    TOPIC_SUPPLY_COLLATERAL_MORPHO,
                    TOPIC_WITHDRAW_COLLATERAL_MORPHO,
                ],
                addresses: &[],
                catch_all: false,
            },
            SubFilterDef {
                label: "Euler",
                topics: &[
                    TOPIC_DEPOSIT_EULER,
                    TOPIC_WITHDRAW_EULER,
                    TOPIC_BORROW_EULER,
                    TOPIC_REPAY_EULER,
                ],
                addresses: &[],
                catch_all: false,
            },
        ],
        sub_offset: 9,
        match_by_address: false,
    },
    // Group 4: Liquidation (sub_offset 13..17)
    EventGroupDef {
        label: "Liquidation",
        short_label: "LIQS",
        color: Color::Rgb(255, 80, 80),
        topics: &[
            TOPIC_LIQUIDATION_AAVE_V3,
            TOPIC_LIQUIDATION_COMPOUND_V3,
            TOPIC_LIQUIDATION_MORPHO,
            TOPIC_LIQUIDATION_EULER_V2,
        ],
        sub_filters: &[
            SubFilterDef {
                label: "Aave",
                topics: &[TOPIC_LIQUIDATION_AAVE_V3],
                addresses: &[],
                catch_all: false,
            },
            SubFilterDef {
                label: "Comp/Moon",
                topics: &[TOPIC_LIQUIDATION_COMPOUND_V3],
                addresses: &[],
                catch_all: false,
            },
            SubFilterDef {
                label: "Morpho",
                topics: &[TOPIC_LIQUIDATION_MORPHO],
                addresses: &[],
                catch_all: false,
            },
            SubFilterDef {
                label: "Euler",
                topics: &[TOPIC_LIQUIDATION_EULER_V2],
                addresses: &[],
                catch_all: false,
            },
        ],
        sub_offset: 13,
        match_by_address: false,
    },
    // Group 5: Bridge (sub_offset 17..19)
    EventGroupDef {
        label: "Bridge",
        short_label: "BRDG",
        color: Color::Rgb(255, 150, 50),
        topics: &[
            TOPIC_ETH_BRIDGE_INITIATED,
            TOPIC_ETH_BRIDGE_FINALIZED,
            TOPIC_ERC20_BRIDGE_INITIATED,
            TOPIC_ERC20_BRIDGE_FINALIZED,
        ],
        sub_filters: &[
            SubFilterDef {
                label: "Deposits",
                topics: &[TOPIC_ETH_BRIDGE_FINALIZED, TOPIC_ERC20_BRIDGE_FINALIZED],
                addresses: &[],
                catch_all: false,
            },
            SubFilterDef {
                label: "Withdrawals",
                topics: &[TOPIC_ETH_BRIDGE_INITIATED, TOPIC_ERC20_BRIDGE_INITIATED],
                addresses: &[],
                catch_all: false,
            },
        ],
        sub_offset: 17,
        match_by_address: false,
    },
];

/// Returns all unique event topic0 hashes tracked by the activity bar.
///
/// Used to build an `eth_getLogs` filter that only fetches logs matching
/// events we care about, avoiding the massive response from unfiltered queries.
///
/// Computed once and cached for the process lifetime.
pub(crate) fn all_tracked_topics() -> &'static [B256] {
    static TOPICS: std::sync::LazyLock<Vec<B256>> = std::sync::LazyLock::new(|| {
        let mut seen = HashSet::new();
        let mut topics = Vec::new();
        for group in &EVENT_GROUP_DEFS {
            for &t in group.topics {
                if seen.insert(t) {
                    topics.push(B256::from(t));
                }
            }
        }
        topics
    });
    &TOPICS
}

/// Per-block event counts across all filters.
#[derive(Clone, Debug, Default)]
pub(crate) struct BlockEventCounts {
    /// L2 block number this entry belongs to.
    pub block_number: u64,
    /// Event counts indexed by sub-filter ordinal.
    pub counts: [u32; SUB_FILTER_TOTAL],
    /// Total USDC transferred in this block (raw units, 6 decimals).
    pub usdc_volume: u128,
    /// USDC volume flowing through swap pools (raw units, 6 decimals).
    /// A subset of `usdc_volume` — only USDC transfers where the sender or
    /// receiver also emitted a Swap event in this flashblock.
    pub swap_volume: u128,
    /// WETH volume flowing through swap pools (raw units, 18 decimals).
    pub weth_swap_volume: u128,
    /// Number of pool liquidity add events (`Mint` V2/V3, positive `ModifyLiquidity` V4).
    pub pool_adds: u32,
    /// Number of pool liquidity remove events (`Burn` V2/V3, negative `ModifyLiquidity` V4).
    pub pool_removes: u32,
    /// Number of lending supply/repay events (capital flowing into protocols).
    pub lend_supply: u32,
    /// Number of lending withdraw/borrow events (capital flowing out of protocols).
    pub lend_withdraw: u32,
    /// Number of bridge inflow events (L1 → L2 finalized).
    pub bridge_in: u32,
    /// Number of bridge outflow events (L2 → L1 initiated).
    pub bridge_out: u32,
}

/// Rolling-window state for the event activity bar.
#[derive(Debug)]
pub(crate) struct ActivityBarState {
    /// Per-block event counts, newest first, capped at [`ACTIVITY_WINDOW_BLOCKS`].
    pub window: VecDeque<BlockEventCounts>,
    /// Per-group high-water mark of *window totals* for normalization.
    /// Only increases; gives a "peak activity" reference for the fill ratio.
    pub rolling_max: [u32; EVENT_GROUP_COUNT],
    /// Which sub-filters are currently toggled on.
    pub active: [bool; SUB_FILTER_TOTAL],
}

impl Default for ActivityBarState {
    fn default() -> Self {
        Self::new()
    }
}

impl ActivityBarState {
    /// Creates a new state with all sub-filters active by default.
    pub(crate) fn new() -> Self {
        Self {
            window: VecDeque::with_capacity(ACTIVITY_WINDOW_BLOCKS),
            rolling_max: [0; EVENT_GROUP_COUNT],
            active: [true; SUB_FILTER_TOTAL],
        }
    }

    /// Returns true if any sub-filter is active.
    pub(crate) fn any_active(&self) -> bool {
        self.active.iter().any(|&a| a)
    }

    /// Returns true if any sub-filter in the given group is active.
    pub(crate) fn group_active(&self, group: &EventGroupDef) -> bool {
        let range = group.sub_offset..group.sub_offset + group.sub_filters.len();
        self.active[range].iter().any(|&a| a)
    }

    /// Sums counts for a group across all its active sub-filters for one block entry.
    pub(crate) fn group_count(
        entry: &BlockEventCounts,
        group: &EventGroupDef,
        active: &[bool],
    ) -> u32 {
        let mut total = 0u32;
        for (i, _sf) in group.sub_filters.iter().enumerate() {
            let idx = group.sub_offset + i;
            if active[idx] {
                total = total.saturating_add(entry.counts[idx]);
            }
        }
        total
    }

    /// Sums event counts per group across the entire window (only active sub-filters).
    pub(crate) fn window_totals(&self) -> [u32; EVENT_GROUP_COUNT] {
        let mut totals = [0u32; EVENT_GROUP_COUNT];
        for entry in &self.window {
            for (gi, group) in EVENT_GROUP_DEFS.iter().enumerate() {
                totals[gi] =
                    totals[gi].saturating_add(Self::group_count(entry, group, &self.active));
            }
        }
        totals
    }

    /// Toggles all sub-filters in a group on or off.
    ///
    /// If any sub-filter is active, turns them all off. Otherwise, turns them all on.
    pub(crate) fn toggle_group(&mut self, group_idx: usize) {
        let group = &EVENT_GROUP_DEFS[group_idx];
        let range = group.sub_offset..group.sub_offset + group.sub_filters.len();
        let any_on = self.active[range.clone()].iter().any(|&a| a);
        for idx in range {
            self.active[idx] = !any_on;
        }
    }

    /// Records logs from a flashblock into the rolling window.
    ///
    /// If the block already has an entry (from a previous flashblock for the same block),
    /// counts are accumulated. If the window is full, the oldest block is evicted.
    ///
    /// Uses a two-pass approach: first collects addresses that emitted Swap events
    /// (pool addresses), then processes Transfer events to identify swap-related USDC
    /// volume separately from total USDC transfer volume.
    pub(crate) fn record_logs(&mut self, block_number: u64, logs: &[ReceiptLog]) {
        if !self.any_active() {
            return;
        }

        // Pass 1: collect addresses that emitted Swap events (these are pool contracts).
        let swap_topics: &[&[u8; 32]] = &[&TOPIC_SWAP_V2, &TOPIC_SWAP_V3, &TOPIC_SWAP_V4];

        let swap_pools: HashSet<&[u8]> = logs
            .iter()
            .filter(|log| {
                log.topics
                    .first()
                    .is_some_and(|t0| swap_topics.iter().any(|st| st.as_ref() == t0.as_slice()))
            })
            .map(|log| log.address.as_slice())
            .collect();

        // Find or create the entry for this block.
        if self.window.front().map(|e| e.block_number) != Some(block_number) {
            self.window.push_front(BlockEventCounts { block_number, ..Default::default() });
            if self.window.len() > ACTIVITY_WINDOW_BLOCKS {
                self.window.pop_back();
            }
        }
        let entry = self.window.front_mut().unwrap();

        // Pass 2: count events and track volumes.
        for log in logs {
            let Some(topic0) = log.topics.first() else { continue };
            let t0 = topic0.as_slice();

            // Event filter counting — route to group then sub-filter.
            for group in &EVENT_GROUP_DEFS {
                if !group.topics.iter().any(|gt| gt.as_ref() == t0) {
                    continue;
                }
                // ERC20 group: differentiate sub-filters by address.
                if group.match_by_address {
                    let addr = log.address.as_slice();
                    let mut matched = false;
                    for (si, sf) in group.sub_filters.iter().enumerate() {
                        let idx = group.sub_offset + si;
                        if !self.active[idx] {
                            continue;
                        }
                        if sf.catch_all {
                            continue; // handle catch-all after specific matches
                        }
                        if sf.addresses.iter().any(|a| a.as_ref() == addr) {
                            entry.counts[idx] = entry.counts[idx].saturating_add(1);
                            matched = true;
                            break;
                        }
                    }
                    if !matched {
                        // Route to the catch-all sub-filter ("Other").
                        for (si, sf) in group.sub_filters.iter().enumerate() {
                            let idx = group.sub_offset + si;
                            if self.active[idx] && sf.catch_all {
                                entry.counts[idx] = entry.counts[idx].saturating_add(1);
                                break;
                            }
                        }
                    }
                } else {
                    // All other groups: match sub-filter by topic.
                    for (si, sf) in group.sub_filters.iter().enumerate() {
                        let idx = group.sub_offset + si;
                        if self.active[idx] && sf.topics.iter().any(|ft| ft.as_ref() == t0) {
                            entry.counts[idx] = entry.counts[idx].saturating_add(1);
                            break;
                        }
                    }
                }
                break; // Only match one group per log.
            }

            // Pool liquidity direction tracking.
            if t0 == TOPIC_MINT_V2.as_ref() || t0 == TOPIC_MINT_V3.as_ref() {
                entry.pool_adds = entry.pool_adds.saturating_add(1);
            } else if t0 == TOPIC_BURN_V2.as_ref() || t0 == TOPIC_BURN_V3.as_ref() {
                entry.pool_removes = entry.pool_removes.saturating_add(1);
            } else if t0 == TOPIC_MODIFY_LIQUIDITY_V4.as_ref() && log.data.len() >= 96 {
                // liquidityDelta is the 3rd ABI word (int256 at bytes 64..96).
                // Negative if the high bit (byte 64) is set.
                if log.data[64] & 0x80 != 0 {
                    entry.pool_removes = entry.pool_removes.saturating_add(1);
                } else {
                    entry.pool_adds = entry.pool_adds.saturating_add(1);
                }
            }

            // Lending direction tracking: supply/repay = inflow, withdraw/borrow = outflow.
            if t0 == TOPIC_SUPPLY_AAVE.as_ref()
                || t0 == TOPIC_REPAY_AAVE.as_ref()
                || t0 == TOPIC_SUPPLY_COMPOUND.as_ref()
                || t0 == TOPIC_SUPPLY_COLLATERAL_COMPOUND.as_ref()
                || t0 == TOPIC_SUPPLY_MORPHO.as_ref()
                || t0 == TOPIC_REPAY_MORPHO.as_ref()
                || t0 == TOPIC_SUPPLY_COLLATERAL_MORPHO.as_ref()
                || t0 == TOPIC_DEPOSIT_EULER.as_ref()
                || t0 == TOPIC_REPAY_EULER.as_ref()
            {
                entry.lend_supply = entry.lend_supply.saturating_add(1);
            } else if t0 == TOPIC_WITHDRAW_AAVE.as_ref()
                || t0 == TOPIC_BORROW_AAVE.as_ref()
                || t0 == TOPIC_WITHDRAW_COMPOUND.as_ref()
                || t0 == TOPIC_WITHDRAW_COLLATERAL_COMPOUND.as_ref()
                || t0 == TOPIC_WITHDRAW_MORPHO.as_ref()
                || t0 == TOPIC_BORROW_MORPHO.as_ref()
                || t0 == TOPIC_WITHDRAW_COLLATERAL_MORPHO.as_ref()
                || t0 == TOPIC_WITHDRAW_EULER.as_ref()
                || t0 == TOPIC_BORROW_EULER.as_ref()
            {
                entry.lend_withdraw = entry.lend_withdraw.saturating_add(1);
            }

            // Bridge direction tracking: finalized = inflow, initiated = outflow.
            if t0 == TOPIC_ETH_BRIDGE_FINALIZED.as_ref()
                || t0 == TOPIC_ERC20_BRIDGE_FINALIZED.as_ref()
            {
                entry.bridge_in = entry.bridge_in.saturating_add(1);
            } else if t0 == TOPIC_ETH_BRIDGE_INITIATED.as_ref()
                || t0 == TOPIC_ERC20_BRIDGE_INITIATED.as_ref()
            {
                entry.bridge_out = entry.bridge_out.saturating_add(1);
            }

            // Volume tracking: USDC and WETH Transfer events through swap pools.
            if t0 == TOPIC_ERC20_TRANSFER.as_ref() && log.data.len() >= 32 {
                let addr = log.address.as_slice();
                let is_usdc = addr == USDC_ADDRESS.as_ref();
                let is_weth = addr == WETH_ADDRESS.as_ref();

                if is_usdc || is_weth {
                    let amount = decode_uint256(&log.data);

                    if is_usdc {
                        entry.usdc_volume = entry.usdc_volume.saturating_add(amount);
                    }

                    // Check if the sender or receiver is a swap pool.
                    if log.topics.len() >= 3 {
                        let from = &log.topics[1].as_slice()[12..];
                        let to = &log.topics[2].as_slice()[12..];
                        if swap_pools.contains(from) || swap_pools.contains(to) {
                            if is_usdc {
                                entry.swap_volume = entry.swap_volume.saturating_add(amount);
                            } else {
                                entry.weth_swap_volume =
                                    entry.weth_swap_volume.saturating_add(amount);
                            }
                        }
                    }
                }
            }
        }

        // Update per-group rolling maxima from the entry we just modified.
        // rolling_max only increases, so checking the current entry suffices.
        if let Some(entry) = self.window.front() {
            for (gi, group) in EVENT_GROUP_DEFS.iter().enumerate() {
                let count = Self::group_count(entry, group, &self.active);
                if count > self.rolling_max[gi] {
                    self.rolling_max[gi] = count;
                }
            }
        }
    }

    /// Returns the total USDC transfer volume across the window in raw units (6 decimals).
    pub(crate) fn usdc_window_total(&self) -> u128 {
        self.window.iter().map(|e| e.usdc_volume).fold(0u128, u128::saturating_add)
    }

    /// Returns the USDC swap volume across the window in raw units (6 decimals).
    ///
    /// Only counts USDC transfers where the sender or receiver also emitted
    /// a Swap event in the same flashblock.
    pub(crate) fn swap_window_total(&self) -> u128 {
        self.window.iter().map(|e| e.swap_volume).fold(0u128, u128::saturating_add)
    }

    /// Returns the WETH swap volume across the window in raw units (18 decimals).
    pub(crate) fn weth_swap_window_total(&self) -> u128 {
        self.window.iter().map(|e| e.weth_swap_volume).fold(0u128, u128::saturating_add)
    }

    /// Returns the net pool liquidity events across the window (adds - removes).
    pub(crate) fn pool_net_total(&self) -> i64 {
        self.window
            .iter()
            .fold(0i64, |acc, e| acc.saturating_add(e.pool_adds as i64 - e.pool_removes as i64))
    }

    /// Returns the net lending flow across the window (supply/repay - withdraw/borrow).
    pub(crate) fn lend_net_total(&self) -> i64 {
        self.window
            .iter()
            .fold(0i64, |acc, e| acc.saturating_add(e.lend_supply as i64 - e.lend_withdraw as i64))
    }

    /// Returns the net bridge flow across the window (inflows - outflows).
    pub(crate) fn bridge_net_total(&self) -> i64 {
        self.window
            .iter()
            .fold(0i64, |acc, e| acc.saturating_add(e.bridge_in as i64 - e.bridge_out as i64))
    }
}

// =============================================================================
// Activity Bar Rendering
// =============================================================================

pub(crate) const ACTIVITY_LABEL_CHARS: usize = 5;
pub(crate) const ACTIVITY_COUNT_CHARS: usize = 5;
/// Block element characters ordered by ascending fill height (1/8 through 8/8).
const SPARK_BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Minimum sparkline width (chars) for a compact segment to be useful.
const SPARKLINE_MIN_WIDTH: usize = 10;

/// Minimum total segment width (label + sparkline + count).
const SPARKLINE_MIN_SEGMENT: usize =
    ACTIVITY_LABEL_CHARS + ACTIVITY_COUNT_CHARS + SPARKLINE_MIN_WIDTH;

/// Formats a USDC rate (raw units / second) as a compact dollar-per-second string.
fn format_usdc_rate(raw_per_sec: f64) -> String {
    if raw_per_sec >= 1_000_000.0 {
        format!("${:.1}M/s", raw_per_sec / 1_000_000.0)
    } else if raw_per_sec >= 1_000.0 {
        format!("${:.0}K/s", raw_per_sec / 1_000.0)
    } else {
        format!("${raw_per_sec:.0}/s")
    }
}

/// Formats an ETH rate (raw 18-decimal units / second) as a compact string.
fn format_eth_rate(raw_per_sec: f64) -> String {
    if raw_per_sec >= 1_000.0 {
        format!("{:.0}K/s", raw_per_sec / 1_000.0)
    } else if raw_per_sec >= 1.0 {
        format!("{raw_per_sec:.1}/s")
    } else if raw_per_sec >= 0.001 {
        format!("{raw_per_sec:.3}/s")
    } else {
        "0/s".to_string()
    }
}

/// Formats a signed net rate as a colored string.
fn format_net_rate(total: i64, secs: f64) -> (String, Color) {
    let rate = total as f64 / secs;
    if rate > 0.0 {
        (format!("+{rate:.1}/s"), Color::Green)
    } else if rate < 0.0 {
        (format!("{rate:.1}/s"), Color::Red)
    } else {
        ("0/s".to_string(), Color::DarkGray)
    }
}

/// Number of rows used by the volume/net stats header.
pub(crate) const VOLUME_STATS_ROWS: u16 = 2;

pub(crate) fn build_volume_lines(
    state: &ActivityBarState,
    window_secs: Option<f64>,
) -> [Option<Line<'static>>; VOLUME_STATS_ROWS as usize] {
    let Some(secs) = window_secs.filter(|&s| s > 0.0) else {
        return [None, None];
    };

    let sep = Span::styled("  │  ", Style::default().fg(Color::DarkGray));

    // Row 1: volume rates.
    let usdc_total = state.usdc_window_total();
    let swap_total = state.swap_window_total();
    let weth_swap_total = state.weth_swap_window_total();

    let vol_line = if usdc_total > 0 || swap_total > 0 || weth_swap_total > 0 {
        let mut spans: Vec<Span<'static>> = Vec::new();

        let usdc_dollars_per_sec = usdc_total as f64 / secs / 1_000_000.0;
        spans.push(Span::styled(" USDC xfer: ", Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(
            format_usdc_rate(usdc_dollars_per_sec),
            Style::default().fg(Color::Rgb(0, 200, 150)),
        ));

        spans.push(sep.clone());

        let swap_dollars_per_sec = swap_total as f64 / secs / 1_000_000.0;
        spans.push(Span::styled("Swap $: ", Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(
            format_usdc_rate(swap_dollars_per_sec),
            Style::default().fg(Color::Rgb(0, 150, 255)),
        ));

        spans.push(sep.clone());

        let weth_eth_per_sec = weth_swap_total as f64 / secs / 1e18;
        spans.push(Span::styled("Swap ETH: ", Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(
            format_eth_rate(weth_eth_per_sec),
            Style::default().fg(Color::Rgb(180, 100, 255)),
        ));

        Some(Line::from(spans))
    } else {
        None
    };

    // Row 2: net directional stats.
    let pool_net = state.pool_net_total();
    let lend_net = state.lend_net_total();
    let bridge_net = state.bridge_net_total();

    let net_line = if pool_net != 0 || lend_net != 0 || bridge_net != 0 {
        let mut spans: Vec<Span<'static>> = Vec::new();

        let (net_pool_str, net_pool_color) = format_net_rate(pool_net, secs);
        spans.push(Span::styled(" Net Pool: ", Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(net_pool_str, Style::default().fg(net_pool_color)));

        spans.push(sep.clone());

        let (net_lend_str, net_lend_color) = format_net_rate(lend_net, secs);
        spans.push(Span::styled("Net Lend: ", Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(net_lend_str, Style::default().fg(net_lend_color)));

        spans.push(sep);

        let (net_bridge_str, net_bridge_color) = format_net_rate(bridge_net, secs);
        spans.push(Span::styled("Net Brdg: ", Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(net_bridge_str, Style::default().fg(net_bridge_color)));

        Some(Line::from(spans))
    } else {
        None
    };

    [vol_line, net_line]
}

/// Returns the maximum number of sparkline segments that fit in the given width.
const fn max_sparklines_per_row(available_width: usize) -> usize {
    if available_width < SPARKLINE_MIN_SEGMENT {
        return 1;
    }
    // Each segment needs SPARKLINE_MIN_SEGMENT chars + 1 separator (except the first).
    (available_width + 1) / (SPARKLINE_MIN_SEGMENT + 1)
}

/// Computes a balanced number of sparklines per row so rows are evenly filled.
fn balanced_sparklines_per_row(active_count: usize, available_width: usize) -> usize {
    let max_per = max_sparklines_per_row(available_width).max(1);
    let num_rows = active_count.div_ceil(max_per);
    active_count.div_ceil(num_rows)
}

/// Computes the total height needed for the compact activity bar.
///
/// Packs multiple sparkline segments per row. Returns 0 if no groups are active.
pub(crate) fn activity_bar_height(state: &ActivityBarState, available_width: u16) -> u16 {
    let active_group_count = EVENT_GROUP_DEFS.iter().filter(|g| state.group_active(g)).count();
    if active_group_count == 0 {
        return 0;
    }
    let inner_width = available_width.saturating_sub(2) as usize; // borders
    let per_row = balanced_sparklines_per_row(active_group_count, inner_width);
    let segment_rows = active_group_count.div_ceil(per_row);
    // 2 (border) + VOLUME_STATS_ROWS + segment_rows
    2 + VOLUME_STATS_ROWS + segment_rows as u16
}

/// Renders the compact event activity bar into `area`.
///
/// Layout (top to bottom inside the border):
/// 1. Volume stats lines (USDC transfer rate, swap volume, net stats)
/// 2. Sparkline segments packed multiple-per-row with `│` separators
///
/// `window_secs` is the time span (in seconds) covered by the current window,
/// used to compute volume rates. Pass `None` if timestamps are unavailable.
pub(crate) fn render_activity_bar(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &ActivityBarState,
    window_secs: Option<f64>,
) {
    let border_block = Block::default()
        .title(" Activity [f] ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(COLOR_ACTIVE_BORDER));

    let inner = border_block.inner(area);
    frame.render_widget(border_block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // Rows 0..1: volume/net stats lines.
    let vol_lines = build_volume_lines(state, window_secs);
    for (row, line) in vol_lines.iter().enumerate() {
        let y = inner.y + row as u16;
        if y >= inner.y + inner.height {
            return;
        }
        if let Some(line) = line {
            frame.render_widget(
                Paragraph::new(line.clone()),
                Rect { x: inner.x, y, width: inner.width, height: 1 },
            );
        }
    }

    let active_group_indices: Vec<usize> =
        (0..EVENT_GROUP_COUNT).filter(|&i| state.group_active(&EVENT_GROUP_DEFS[i])).collect();

    if active_group_indices.is_empty() {
        return;
    }

    let totals = state.window_totals();
    let available = inner.width as usize;
    let per_row = balanced_sparklines_per_row(active_group_indices.len(), available);

    for (row_idx, row_groups) in active_group_indices.chunks(per_row).enumerate() {
        let y = inner.y + VOLUME_STATS_ROWS + row_idx as u16;
        if y >= inner.y + inner.height {
            break;
        }

        let n = row_groups.len();
        let separators = n.saturating_sub(1);
        let total_seg_space = available.saturating_sub(separators);
        let seg_width = if n > 0 { total_seg_space / n } else { 0 };

        let mut x = inner.x;
        for (nth, &gi) in row_groups.iter().enumerate() {
            if nth > 0 {
                frame.render_widget(
                    Paragraph::new("│").style(Style::default().fg(Color::DarkGray)),
                    Rect { x, y, width: 1, height: 1 },
                );
                x += 1;
            }

            let this_seg_width = if nth == n - 1 {
                available.saturating_sub((x - inner.x) as usize)
            } else {
                seg_width
            };

            if this_seg_width == 0 {
                break;
            }

            let spark_width =
                this_seg_width.saturating_sub(ACTIVITY_LABEL_CHARS + ACTIVITY_COUNT_CHARS);
            let line = render_sparkline_row(
                &EVENT_GROUP_DEFS[gi],
                &state.window,
                &state.active,
                totals[gi],
                spark_width,
            );

            frame.render_widget(
                Paragraph::new(line),
                Rect { x, y, width: this_seg_width as u16, height: 1 },
            );
            x += this_seg_width as u16;
        }
    }
}

/// Builds a single sparkline row for one event group.
///
/// Each character position maps to one block in the window (oldest left,
/// newest right). The block element height represents the group-level count
/// (sum of active sub-filters) relative to the per-group window maximum,
/// and each character is colored with the corresponding block's palette color.
pub(crate) fn render_sparkline_row(
    group: &EventGroupDef,
    window: &VecDeque<BlockEventCounts>,
    active: &[bool; SUB_FILTER_TOTAL],
    window_total: u32,
    spark_width: usize,
) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();

    // Label (fixed width, right-padded).
    spans.push(Span::styled(
        format!("{:<ACTIVITY_LABEL_CHARS$}", group.short_label),
        Style::default().fg(Color::DarkGray),
    ));

    // Per-group maximum count across the window for normalization.
    let max_count =
        window.iter().map(|e| ActivityBarState::group_count(e, group, active)).max().unwrap_or(0);

    if spark_width > 0 {
        // Window is stored newest-first; render oldest-left, newest-right.
        let block_count = window.len();
        let pad = spark_width.saturating_sub(block_count);

        if pad > 0 {
            spans.push(Span::raw(" ".repeat(pad)));
        }

        let visible = block_count.min(spark_width);
        for entry in window.iter().take(visible).rev() {
            let count = ActivityBarState::group_count(entry, group, active);
            if count == 0 || max_count == 0 {
                spans.push(Span::raw(" "));
            } else {
                let ratio = count as f64 / max_count as f64;
                let level = (ratio * 8.0).ceil() as usize;
                let ch = SPARK_BLOCKS[level.clamp(1, 8) - 1];
                let brightness = 0.3 + ratio * 0.7;
                let color = dim_color(block_color(entry.block_number), brightness);
                spans.push(Span::styled(ch.to_string(), Style::default().fg(color)));
            }
        }

        let rendered = pad + visible;
        if rendered < spark_width {
            spans.push(Span::raw(" ".repeat(spark_width - rendered)));
        }
    }

    // Absolute count (fixed width).
    let count_str = if window_total >= 1_000_000 {
        format!("{:>4.1}m", window_total as f64 / 1_000_000.0)
    } else if window_total >= 10_000 {
        format!("{:>4}k", window_total / 1000)
    } else {
        format!("{window_total:>ACTIVITY_COUNT_CHARS$}")
    };
    spans.push(Span::styled(count_str, Style::default().fg(group.color)));

    Line::from(spans)
}

// =============================================================================
// Event Filter Menu
// =============================================================================

/// Maps a flat cursor position to a `(group_idx, Option<sub_idx>)` pair.
///
/// Group headers return `(group_idx, None)`. Sub-filter rows return `(group_idx, Some(sub_idx))`.
pub(crate) fn cursor_to_filter(cursor: usize) -> (usize, Option<usize>) {
    let mut pos = 0;
    for (gi, group) in EVENT_GROUP_DEFS.iter().enumerate() {
        if pos == cursor {
            return (gi, None); // group header
        }
        pos += 1;
        for si in 0..group.sub_filters.len() {
            if pos == cursor {
                return (gi, Some(si));
            }
            pos += 1;
        }
    }
    // Fallback (should not happen with valid cursor)
    (0, None)
}

/// Renders the hierarchical event filter selection popup centered in `area`.
///
/// Shows a tree of groups and sub-filters; the row at `cursor` is highlighted.
pub(crate) fn render_filter_menu(
    frame: &mut Frame<'_>,
    area: Rect,
    active: &[bool; SUB_FILTER_TOTAL],
    cursor: usize,
) {
    let popup_width: u16 = 38;
    let popup_height: u16 = FILTER_MENU_ITEMS as u16 + 4;

    let x = area.x + area.width.saturating_sub(popup_width) / 2;
    let y = area.y + area.height.saturating_sub(popup_height) / 2;
    let popup_area = Rect { x, y, width: popup_width.min(area.width), height: popup_height };

    frame.render_widget(Clear, popup_area);

    let border_block = Block::default()
        .title(" Event Filters ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(COLOR_ACTIVE_BORDER));

    let inner = border_block.inner(popup_area);
    frame.render_widget(border_block, popup_area);

    let mut row_idx = 0u16;
    let mut flat_pos = 0usize;

    for group in &EVENT_GROUP_DEFS {
        let range = group.sub_offset..group.sub_offset + group.sub_filters.len();
        let all_on = active[range.clone()].iter().all(|&a| a);
        let any_on = active[range].iter().any(|&a| a);

        // Group header row.
        let row_area = Rect { x: inner.x, y: inner.y + row_idx, width: inner.width, height: 1 };
        let check = if all_on {
            "[x]"
        } else if any_on {
            "[-]"
        } else {
            "[ ]"
        };
        let row_style = if flat_pos == cursor {
            Style::default().bg(COLOR_ROW_SELECTED)
        } else {
            Style::default()
        };
        let label_style = Style::default().fg(group.color).add_modifier(Modifier::BOLD);
        let line = Line::from(vec![
            Span::styled(format!("{check} "), row_style),
            Span::styled(group.label, row_style.patch(label_style)),
        ]);
        frame.render_widget(Paragraph::new(line).style(row_style), row_area);
        row_idx += 1;
        flat_pos += 1;

        // Sub-filter rows.
        for (si, sf) in group.sub_filters.iter().enumerate() {
            let idx = group.sub_offset + si;
            let sf_area = Rect { x: inner.x, y: inner.y + row_idx, width: inner.width, height: 1 };
            let sf_check = if active[idx] { "[x]" } else { "[ ]" };
            let sf_row_style = if flat_pos == cursor {
                Style::default().bg(COLOR_ROW_SELECTED)
            } else {
                Style::default()
            };
            let sf_label_style = Style::default().fg(group.color);
            let sf_line = Line::from(vec![
                Span::styled(format!("  {sf_check} "), sf_row_style),
                Span::styled(sf.label, sf_row_style.patch(sf_label_style)),
            ]);
            frame.render_widget(Paragraph::new(sf_line).style(sf_row_style), sf_area);
            row_idx += 1;
            flat_pos += 1;
        }
    }

    let footer_area =
        Rect { x: inner.x, y: inner.y + FILTER_MENU_ITEMS as u16, width: inner.width, height: 1 };
    let footer =
        Paragraph::new("Space: toggle │ f/Esc: close").style(Style::default().fg(Color::DarkGray));
    frame.render_widget(footer, footer_area);
}
