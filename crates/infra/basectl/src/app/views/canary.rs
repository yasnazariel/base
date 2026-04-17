//! Canary view — renders in-process canary runner state from [`Resources`].
//!
//! All mutable state lives in [`crate::app::Resources::canary`] so the task
//! keeps executing across view switches. The view itself is stateless except
//! for the scroll offset.

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use alloy_primitives::{U256, utils::parse_ether};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_signer_local::PrivateKeySigner;
use base_canary::{
    BalanceCheckAction, CanaryAction, GossipSpamAction, HealthCheckAction, InvalidBatchAction,
    LoadTestAction, LoadTestConfig, ScheduleMode, Scheduler,
};
use base_load_tests::HARDHAT_TEST_KEYS;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    prelude::*,
    widgets::{Block, Borders, List, ListItem, Paragraph},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{
    app::{Action, CanaryEvent, CanaryOutcome, Resources, View},
    commands::COLOR_BASE_BLUE,
    tui::Keybinding,
};

/// L2 execution-client RPC endpoint for a local devnet.
const DEVNET_L2_RPC: &str = "http://localhost:8545";
/// L2 execution-client WebSocket endpoint for a local devnet.
const DEVNET_L2_WS: &str = "ws://localhost:8546";
/// L1 RPC endpoint for a local devnet (Reth/Anvil).
const DEVNET_L1_RPC: &str = "http://localhost:4545";
/// Consensus-layer RPC endpoint for the devnet client node.
const DEVNET_CL_RPC: &str = "http://localhost:8549";

// Action defaults — match the base-canary binary defaults.
const DEFAULT_MIN_BALANCE_ETH: &str = "0.01";
const DEFAULT_FUNDING_ETH: &str = "0.1";
const DEFAULT_MAX_BLOCK_AGE: Duration = Duration::from_secs(30);
const DEFAULT_SCHEDULE_INTERVAL: Duration = Duration::from_secs(60);
const DEFAULT_LOAD_TEST_GPS: u64 = 12_600_000;
const DEFAULT_LOAD_TEST_DURATION: Duration = Duration::from_secs(30);
const DEFAULT_LOAD_TEST_ACCOUNTS: usize = 60;
const DEFAULT_LOAD_TEST_SEED: u64 = 1;

const KEYBINDINGS: &[Keybinding] = &[
    Keybinding { key: "s", description: "Start / Stop" },
    Keybinding { key: "↑/↓", description: "Scroll" },
    Keybinding { key: "Esc", description: "Back" },
];

/// Renders the canary runner. All persistent state lives in [`Resources::canary`].
#[derive(Debug, Default)]
pub struct CanaryView {
    /// Lines scrolled back from the bottom (0 = auto-scroll to latest).
    scroll_offset: usize,
}

impl CanaryView {
    /// Creates a new canary view.
    pub const fn new() -> Self {
        Self { scroll_offset: 0 }
    }

    fn spawn_task(resources: &mut Resources) {
        let cancel = CancellationToken::new();
        let (event_tx, event_rx) = mpsc::channel(64);
        let cancel_for_task = cancel.clone();

        let handle = tokio::spawn(async move {
            let Ok(l2_rpc_url) = Url::parse(DEVNET_L2_RPC) else { return };
            let Ok(l2_ws_url) = Url::parse(DEVNET_L2_WS) else { return };

            let provider = ProviderBuilder::new().connect_http(l2_rpc_url.clone());
            let chain_id = tokio::time::timeout(Duration::from_secs(5), provider.get_chain_id())
                .await
                .ok()
                .and_then(|r| r.ok())
                .unwrap_or(84538453);

            // Pick the first Hardhat test account that holds a non-zero balance.
            // Devnets don't always fund account 0; this mirrors devnet_funder() from
            // base-load-tests so the canary always gets a usable key automatically.
            let mut signer: Option<PrivateKeySigner> = None;
            for key_str in HARDHAT_TEST_KEYS {
                let Ok(s) = key_str.parse::<PrivateKeySigner>() else { continue };
                if provider.get_balance(s.address()).await.is_ok_and(|b| !b.is_zero()) {
                    signer = Some(s);
                    break;
                }
            }
            let Some(signer) = signer else { return };

            let min_balance = parse_ether(DEFAULT_MIN_BALANCE_ETH).unwrap_or(U256::ZERO);
            let funding_amount = parse_ether(DEFAULT_FUNDING_ETH).unwrap_or(U256::ZERO);

            // HARDHAT_TEST_KEYS[1] is the "wrong" signer for the invalid batch action —
            // it is provably different from the real devnet batcher key.
            let wrong_signer =
                HARDHAT_TEST_KEYS[1].parse::<PrivateKeySigner>().expect("valid hardhat key");

            let Ok(cl_rpc_url) = Url::parse(DEVNET_CL_RPC) else { return };
            let Ok(l1_rpc_url) = Url::parse(DEVNET_L1_RPC) else { return };

            let actions: Vec<Box<dyn CanaryAction>> = vec![
                Box::new(BalanceCheckAction::new(
                    l2_rpc_url.clone(),
                    signer.address(),
                    min_balance,
                )),
                Box::new(HealthCheckAction::new(l2_rpc_url.clone(), DEFAULT_MAX_BLOCK_AGE)),
                Box::new(LoadTestAction::new(LoadTestConfig {
                    l2_rpc_url: l2_rpc_url.clone(),
                    l2_ws_url: Some(l2_ws_url),
                    chain_id,
                    funding_key: signer,
                    funding_amount_wei: funding_amount,
                    target_gps: DEFAULT_LOAD_TEST_GPS,
                    duration: DEFAULT_LOAD_TEST_DURATION,
                    account_count: DEFAULT_LOAD_TEST_ACCOUNTS,
                    seed: DEFAULT_LOAD_TEST_SEED,
                })),
                Box::new(GossipSpamAction::new(cl_rpc_url.clone(), 1000, Duration::ZERO)),
                Box::new(InvalidBatchAction::new(l1_rpc_url, cl_rpc_url, wrong_signer)),
            ];

            let scheduler = Scheduler::new(
                ScheduleMode::Deterministic,
                DEFAULT_SCHEDULE_INTERVAL,
                Duration::ZERO,
            );

            loop {
                for action in &actions {
                    if cancel_for_task.is_cancelled() {
                        return;
                    }
                    let name = action.name();
                    let _ = event_tx.send(CanaryEvent::ActionStarted { name }).await;
                    let outcome = action.execute(cancel_for_task.child_token()).await;
                    if event_tx.send(CanaryEvent::ActionCompleted { name, outcome }).await.is_err()
                    {
                        return;
                    }
                }

                if cancel_for_task.is_cancelled() {
                    return;
                }

                let delay = scheduler.next_delay();
                let deadline = Instant::now() + delay;
                loop {
                    if cancel_for_task.is_cancelled() {
                        return;
                    }
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        break;
                    }
                    let secs_left = remaining.as_secs();
                    let _ = event_tx
                        .send(CanaryEvent::WaitingForNextRun { delay_secs: secs_left })
                        .await;
                    let tick = remaining.min(Duration::from_secs(1));
                    tokio::select! {
                        () = cancel_for_task.cancelled() => return,
                        () = tokio::time::sleep(tick) => {}
                    }
                }
            }
        });

        resources.canary.set_task(cancel, event_rx, handle);
    }
}

impl View for CanaryView {
    fn keybindings(&self) -> &'static [Keybinding] {
        KEYBINDINGS
    }

    fn handle_key(&mut self, key: KeyEvent, resources: &mut Resources) -> Action {
        if resources.config.name != "devnet" {
            return Action::None;
        }

        match key.code {
            KeyCode::Char('s') => {
                if resources.canary.is_running() {
                    resources.canary.stop();
                } else {
                    Self::spawn_task(resources);
                }
                Action::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.scroll_offset += 1;
                Action::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.scroll_offset = self.scroll_offset.saturating_sub(1);
                Action::None
            }
            _ => Action::None,
        }
    }

    fn tick(&mut self, resources: &mut Resources) -> Action {
        resources.canary.drain_events();
        Action::None
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, resources: &Resources) {
        if resources.config.name != "devnet" {
            let msg = Paragraph::new(
                "Canary is only available on devnet.\n\nStart basectl with -c devnet.",
            )
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center);
            frame.render_widget(msg, area);
            return;
        }

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(0)])
            .split(area);

        render_header(
            frame,
            chunks[0],
            resources.canary.is_running(),
            resources.canary.current_action,
            resources.canary.next_run_secs,
        );
        render_outcomes(
            frame,
            chunks[1],
            &resources.canary.outcomes,
            self.scroll_offset,
            resources.canary.is_running(),
        );
    }
}

fn render_header(
    frame: &mut Frame<'_>,
    area: Rect,
    running: bool,
    current_action: Option<&str>,
    next_run_secs: Option<u64>,
) {
    let state_span = if running {
        Span::styled("Running", Style::default().fg(Color::Green))
    } else {
        Span::styled("Idle", Style::default().fg(Color::DarkGray))
    };

    let mut spans = vec![Span::raw("Status: "), state_span];

    if let Some(action) = current_action {
        spans.push(Span::raw("   Executing: "));
        spans.push(Span::styled(action, Style::default().fg(Color::Cyan)));
    } else if let Some(secs) = next_run_secs {
        spans.push(Span::raw("   Next run in: "));
        spans.push(Span::styled(format!("{secs}s"), Style::default().fg(Color::DarkGray)));
    }

    let header = Paragraph::new(Line::from(spans)).block(
        Block::default()
            .title(" Canary ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(COLOR_BASE_BLUE)),
    );

    frame.render_widget(header, area);
}

fn render_outcomes(
    frame: &mut Frame<'_>,
    area: Rect,
    outcomes: &VecDeque<CanaryOutcome>,
    scroll_offset: usize,
    running: bool,
) {
    let block = Block::default()
        .title(" Results ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if outcomes.is_empty() {
        let hint = if running {
            "Waiting for first action to complete..."
        } else {
            "Press [s] to start the canary."
        };
        let para = Paragraph::new(hint)
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center);
        frame.render_widget(para, inner);
        return;
    }

    let visible = inner.height as usize;
    let total = outcomes.len();
    let scroll = scroll_offset.min(total.saturating_sub(visible));
    let start = total.saturating_sub(visible + scroll);

    let items: Vec<ListItem<'_>> = outcomes
        .iter()
        .skip(start)
        .take(visible)
        .map(|row| {
            let (icon, icon_style) = if row.outcome.succeeded {
                ("✓", Style::default().fg(Color::Green))
            } else {
                ("✗", Style::default().fg(Color::Red))
            };
            let msg_style = if row.outcome.succeeded {
                Style::default().fg(Color::Gray)
            } else {
                Style::default().fg(Color::Red)
            };
            let duration_ms = row.outcome.duration.as_millis();
            let line = Line::from(vec![
                Span::styled(icon, icon_style),
                Span::raw(" "),
                Span::styled(
                    format!("{:<16}", row.name),
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ),
                Span::styled(row.outcome.message.as_str(), msg_style),
                Span::styled(format!("  {duration_ms}ms"), Style::default().fg(Color::DarkGray)),
            ]);
            ListItem::new(line)
        })
        .collect();

    frame.render_widget(List::new(items), inner);
}
