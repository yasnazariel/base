use std::collections::HashSet;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    prelude::*,
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState},
};
use tokio::sync::mpsc;

use crate::{
    app::{Action, Resources, View},
    commands::COLOR_BASE_BLUE,
    rpc::{
        EmbeddedLeadershipNodeStatus, override_embedded_leader, pause_embedded_leadership,
        resume_embedded_leadership, transfer_embedded_leadership,
    },
    tui::{Keybinding, Toast},
};

/// Keyboard bindings exposed by the embedded leadership view.
pub const KEYBINDINGS: &[Keybinding] = &[
    Keybinding { key: "←/→", description: "Select node" },
    Keybinding { key: "t", description: "Transfer (any peer)" },
    Keybinding { key: "Enter", description: "Transfer to selected" },
    Keybinding { key: "o", description: "Toggle leader override" },
    Keybinding { key: "p", description: "Pause/resume selected" },
    Keybinding { key: "Esc", description: "Back to home" },
    Keybinding { key: "?", description: "Toggle help" },
];

/// In-flight RPC operation state, collapsing the previous three-field
/// `op_pending`/`op_rx`/`pending_op` representation into a single state machine.
#[derive(Debug, Default)]
pub enum OpState {
    /// No operation in flight; the view accepts new key bindings.
    #[default]
    Idle,
    /// An operation is in flight; the view rejects new key bindings until the ack
    /// arrives on `rx` and the optimistic UI state in `kind` is applied.
    InFlight {
        /// Which operation is pending; used to update local UI state on success.
        kind: PendingOp,
        /// Single-shot ack channel from the spawned RPC task.
        rx: mpsc::Receiver<Result<String, String>>,
    },
}

impl OpState {
    /// Returns `true` if no operation is currently in flight.
    pub const fn is_idle(&self) -> bool {
        matches!(self, Self::Idle)
    }
}

/// Tracks which RPC operation is in flight so the view can update local UI state
/// (paused / overridden sets) on success without re-polling the cluster.
#[derive(Debug)]
pub enum PendingOp {
    /// A `leadership_transferLeadership` request, optionally targeted at a specific node.
    Transfer,
    /// A `leadership_pause` or `leadership_resume` request, identified by node name.
    TogglePause(String),
    /// A `leadership_overrideLeader` request, identified by node name and target value.
    ToggleOverride(String, bool),
}

/// Embedded leadership cluster status view.
///
/// Renders a fixed grid with one column per leadership node and rows for role
/// (leader / follower / offline), view, leader id, cluster snapshot, override
/// state, and the same CL/EL sync columns as the HA conductor view. The user
/// can navigate columns with `←`/`→`, transfer leadership with `t` (any peer)
/// or `Enter` (selected node), toggle the leader override with `o`, and
/// pause/resume the selected node's leadership actor with `p`. When no
/// embedded leadership configuration is present a placeholder message is shown.
#[derive(Debug, Default)]
pub struct EmbeddedLeadershipView {
    selected: usize,
    op: OpState,
    paused_nodes: HashSet<String>,
    overridden_nodes: HashSet<String>,
}

impl EmbeddedLeadershipView {
    /// Creates a new embedded leadership view.
    pub fn new() -> Self {
        Self::default()
    }

    fn start_transfer(&mut self, resources: &Resources, target_name: Option<String>) {
        let Some(ref nodes) = resources.config.embedded_leaderships else { return };
        let (tx, rx) = mpsc::channel(1);
        self.op = OpState::InFlight { kind: PendingOp::Transfer, rx };
        tokio::spawn(transfer_embedded_leadership(nodes.clone(), target_name, tx));
    }

    fn start_pause_toggle(&mut self, resources: &Resources) {
        let Some(ref nodes) = resources.config.embedded_leaderships else { return };
        let idx = self.selected.min(nodes.len().saturating_sub(1));
        let node = nodes[idx].clone();
        let (tx, rx) = mpsc::channel(1);
        let kind = PendingOp::TogglePause(node.name.clone());
        self.op = OpState::InFlight { kind, rx };
        if self.paused_nodes.contains(&node.name) {
            tokio::spawn(resume_embedded_leadership(node, tx));
        } else {
            tokio::spawn(pause_embedded_leadership(node, tx));
        }
    }

    fn start_override_toggle(&mut self, resources: &Resources) {
        let Some(ref nodes) = resources.config.embedded_leaderships else { return };
        let idx = self.selected.min(nodes.len().saturating_sub(1));
        let node = nodes[idx].clone();
        let new_value = !self.overridden_nodes.contains(&node.name);
        let (tx, rx) = mpsc::channel(1);
        let kind = PendingOp::ToggleOverride(node.name.clone(), new_value);
        self.op = OpState::InFlight { kind, rx };
        tokio::spawn(override_embedded_leader(node, new_value, tx));
    }

    /// Applies the optimistic UI state change for a successful operation.
    fn apply_success(&mut self, kind: PendingOp) {
        match kind {
            PendingOp::Transfer => {}
            PendingOp::TogglePause(name) => {
                if !self.paused_nodes.remove(&name) {
                    self.paused_nodes.insert(name);
                }
            }
            PendingOp::ToggleOverride(name, new_value) => {
                if new_value {
                    self.overridden_nodes.insert(name);
                } else {
                    self.overridden_nodes.remove(&name);
                }
            }
        }
    }
}

impl View for EmbeddedLeadershipView {
    fn keybindings(&self) -> &'static [Keybinding] {
        KEYBINDINGS
    }

    fn tick(&mut self, resources: &mut Resources) -> Action {
        if let OpState::InFlight { rx, .. } = &mut self.op
            && let Ok(result) = rx.try_recv()
        {
            // Replace InFlight with Idle, taking ownership of the kind for the success path.
            let prev = std::mem::take(&mut self.op);
            let kind = match prev {
                OpState::InFlight { kind, .. } => Some(kind),
                OpState::Idle => None,
            };
            match result {
                Ok(msg) => {
                    if let Some(kind) = kind {
                        self.apply_success(kind);
                    }
                    resources.toasts.push(Toast::info(msg));
                }
                Err(msg) => resources.toasts.push(Toast::warning(msg)),
            }
        }

        Action::None
    }

    fn handle_key(&mut self, key: KeyEvent, resources: &mut Resources) -> Action {
        let node_count = resources.embedded_leadership.nodes.len();
        let idle = self.op.is_idle();

        match key.code {
            KeyCode::Left | KeyCode::Char('h') if node_count > 0 => {
                self.selected = (self.selected + node_count - 1) % node_count;
            }
            KeyCode::Right | KeyCode::Char('l') if node_count > 0 => {
                self.selected = (self.selected + 1) % node_count;
            }
            KeyCode::Char('t') if idle => {
                self.start_transfer(resources, None);
            }
            KeyCode::Enter if idle && node_count > 0 => {
                let idx = self.selected.min(node_count - 1);
                let target = resources.embedded_leadership.nodes[idx].name.clone();
                self.start_transfer(resources, Some(target));
            }
            KeyCode::Char('o') if idle && node_count > 0 => {
                self.start_override_toggle(resources);
            }
            KeyCode::Char('p') if idle && node_count > 0 => {
                self.start_pause_toggle(resources);
            }
            _ => {}
        }

        Action::None
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, resources: &Resources) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .split(area);

        let content_area = chunks[0];
        let footer_area = chunks[1];

        let nodes = &resources.embedded_leadership.nodes;
        let configured =
            resources.config.embedded_leaderships.as_ref().is_some_and(|n| !n.is_empty());

        if !configured || nodes.is_empty() {
            Self::render_unconfigured(frame, content_area);
        } else {
            let selected = self.selected.min(nodes.len().saturating_sub(1));
            Self::render_cluster_table(
                frame,
                content_area,
                nodes,
                selected,
                !self.op.is_idle(),
                &self.paused_nodes,
                &self.overridden_nodes,
            );
        }

        Self::render_footer(frame, footer_area, !self.op.is_idle());
    }
}

impl EmbeddedLeadershipView {
    fn render_unconfigured(f: &mut Frame<'_>, area: Rect) {
        let block = Block::default()
            .title(" Embedded Leadership ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(COLOR_BASE_BLUE));

        let inner = block.inner(area);
        f.render_widget(block, area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1), Constraint::Min(0)])
            .split(inner);

        let msg = Paragraph::new(
            "Embedded leadership monitoring requires a config with leadership endpoints.",
        )
        .alignment(Alignment::Center)
        .style(Style::default().fg(Color::DarkGray));

        f.render_widget(msg, chunks[1]);
    }

    fn render_footer(f: &mut Frame<'_>, area: Rect, op_pending: bool) {
        let key_style = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
        let desc_style = Style::default().fg(Color::DarkGray);
        let sep_style = Style::default().fg(Color::DarkGray);

        let sep = Span::styled("  │  ", sep_style);

        let mut spans = vec![
            Span::styled("[Esc]", key_style),
            Span::raw(" "),
            Span::styled("back", desc_style),
            sep.clone(),
            Span::styled("[←/→]", key_style),
            Span::raw(" "),
            Span::styled("select node", desc_style),
        ];

        spans.push(sep.clone());
        if op_pending {
            spans.push(Span::styled("working…", Style::default().fg(Color::Yellow)));
        } else {
            spans.push(Span::styled("[t]", key_style));
            spans.push(Span::raw(" "));
            spans.push(Span::styled("transfer to any peer", desc_style));
            spans.push(sep.clone());
            spans.push(Span::styled("[Enter]", key_style));
            spans.push(Span::raw(" "));
            spans.push(Span::styled("transfer to selected", desc_style));
            spans.push(sep.clone());
            spans.push(Span::styled("[o]", key_style));
            spans.push(Span::raw(" "));
            spans.push(Span::styled("toggle override", desc_style));
            spans.push(sep.clone());
            spans.push(Span::styled("[p]", key_style));
            spans.push(Span::raw(" "));
            spans.push(Span::styled("pause/resume selected", desc_style));
        }

        spans.push(sep);
        spans.push(Span::styled("[?]", key_style));
        spans.push(Span::raw(" "));
        spans.push(Span::styled("help", desc_style));

        let footer = Paragraph::new(Line::from(spans));
        f.render_widget(footer, area);
    }

    /// Builds a single labelled row of the cluster table by mapping each node into a `Cell`
    /// via `cell_for`. Centralizes the label-cell + per-node-cell construction so the
    /// table-rendering function stays a flat list of (label, mapper) pairs instead of N
    /// near-identical `vec!`/`for`/`Row::new` triples.
    fn build_row<F>(
        label: &'static str,
        nodes: &[EmbeddedLeadershipNodeStatus],
        cell_for: F,
    ) -> Row<'static>
    where
        F: Fn(&EmbeddedLeadershipNodeStatus) -> Cell<'static>,
    {
        let label_style = Style::default().fg(Color::White).add_modifier(Modifier::BOLD);
        let mut cells = vec![Cell::from(format!("  {label}")).style(label_style)];
        cells.extend(nodes.iter().map(cell_for));
        Row::new(cells).height(1)
    }

    /// Formats a number-or-fallback cell: yellow for the leader, white for followers, dark
    /// gray for missing data.
    fn number_cell(value: Option<u64>, is_leader: bool, prefix: &str) -> Cell<'static> {
        match value {
            Some(n) if is_leader => {
                Cell::from(format!("   {prefix}{n}")).style(Style::default().fg(Color::Yellow))
            }
            Some(n) => {
                Cell::from(format!("   {prefix}{n}")).style(Style::default().fg(Color::White))
            }
            None => Cell::from("   ?").style(Style::default().fg(Color::DarkGray)),
        }
    }

    /// Formats a hash cell, marking forks (same block number, different hash from leader).
    fn hash_cell(
        value: Option<alloy_primitives::B256>,
        is_leader: bool,
        block: Option<u64>,
        leader_pin: Option<(u64, alloy_primitives::B256)>,
    ) -> Cell<'static> {
        match value {
            Some(h) if is_leader => {
                let hex = format!("{h:x}");
                Cell::from(format!("   0x{}…", &hex[..8])).style(Style::default().fg(Color::Yellow))
            }
            Some(h) => {
                let hex = format!("{h:x}");
                let is_fork =
                    leader_pin.is_some_and(|(lnum, lhash)| block == Some(lnum) && h != lhash);
                if is_fork {
                    Cell::from(format!("   ⚠ 0x{}…", &hex[..8]))
                        .style(Style::default().fg(Color::Red))
                } else {
                    Cell::from(format!("   0x{}…", &hex[..8]))
                        .style(Style::default().fg(Color::White))
                }
            }
            None => Cell::from("   ?").style(Style::default().fg(Color::DarkGray)),
        }
    }

    fn render_cluster_table(
        f: &mut Frame<'_>,
        area: Rect,
        nodes: &[EmbeddedLeadershipNodeStatus],
        selected: usize,
        op_pending: bool,
        paused_nodes: &HashSet<String>,
        overridden_nodes: &HashSet<String>,
    ) {
        let title =
            if op_pending { " Embedded Leadership [working…] " } else { " Embedded Leadership " };

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(COLOR_BASE_BLUE));

        let inner = block.inner(area);
        f.render_widget(block, area);

        let node_count = nodes.len();
        let label_pct = 15u16;
        let node_pct = (100u16 - label_pct) / node_count as u16;

        let mut constraints = vec![Constraint::Percentage(label_pct)];
        constraints.extend(std::iter::repeat_n(Constraint::Percentage(node_pct), node_count));

        // ── Fork detection: pin the leader's unsafe and safe (block, hash) pairs so
        // every other node's hash cell can flag forks against them.
        let leader_unsafe: Option<(u64, alloy_primitives::B256)> = nodes.iter().find_map(|n| {
            if n.is_leader == Some(true) { n.unsafe_l2_block.zip(n.unsafe_l2_hash) } else { None }
        });
        let leader_safe: Option<(u64, alloy_primitives::B256)> = nodes.iter().find_map(|n| {
            if n.is_leader == Some(true) { n.safe_l2_block.zip(n.safe_l2_hash) } else { None }
        });

        // Header row: the first cell is empty (matches the label column); the rest are node names.
        let mut header_cells = vec![Cell::from("")];
        for (i, node) in nodes.iter().enumerate() {
            let is_selected = i == selected;
            let role_color = match node.is_leader {
                Some(true) => Color::Yellow,
                Some(false) => Color::DarkGray,
                None => Color::Red,
            };
            let mut mods = Modifier::BOLD;
            if is_selected {
                mods |= Modifier::UNDERLINED;
            }
            let style = Style::default().fg(role_color).add_modifier(mods);
            header_cells.push(Cell::from(node.name.as_str()).style(style));
        }
        let header = Row::new(header_cells)
            .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
            .height(1);

        let role_row = Self::build_row("Role", nodes, |node| {
            let is_paused = paused_nodes.contains(&node.name);
            let (label, style) = match node.is_leader {
                Some(true) => {
                    let label = if is_paused { "★  LEADER + paused" } else { "★  LEADER" };
                    (label, Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
                }
                Some(false) => {
                    let label = if is_paused { "   follower + paused" } else { "   follower" };
                    (label, Style::default().fg(Color::DarkGray))
                }
                None => {
                    let label = if is_paused { "   offline + paused" } else { "   offline" };
                    (label, Style::default().fg(Color::Red))
                }
            };
            Cell::from(label).style(style)
        });

        let view_row = Self::build_row("Term", nodes, |node| match node.view {
            Some(v) if node.is_leader == Some(true) => {
                Cell::from(format!("   {v}")).style(Style::default().fg(Color::Yellow))
            }
            Some(v) => Cell::from(format!("   {v}")).style(Style::default().fg(Color::White)),
            None => Cell::from("   ?").style(Style::default().fg(Color::DarkGray)),
        });

        let leader_row = Self::build_row("Leader", nodes, |node| {
            let label = format!("   {}", node.leader_id.as_deref().unwrap_or("?"));
            let style = if node.leader_id.is_some() {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            Cell::from(label).style(style)
        });

        let cluster_row = Self::build_row("Cluster", nodes, |node| {
            match (node.cluster_size, node.cluster_version) {
                (Some(size), Some(version)) => Cell::from(format!("   {size} voters, v{version}"))
                    .style(Style::default().fg(Color::White)),
                _ => Cell::from("   ?").style(Style::default().fg(Color::DarkGray)),
            }
        });

        // Detect partition-time double-overrides: collect every override generation reported
        // by a currently-overridden node. If we see more than one distinct value, at least
        // one of those overrides is operating on stale cluster information and the row turns
        // red as a fence prompt.
        let override_generations: std::collections::BTreeSet<u64> = nodes
            .iter()
            .filter(|n| n.overridden == Some(true))
            .filter_map(|n| n.override_generation)
            .collect();
        let conflicting_overrides = override_generations.len() > 1;

        let override_row = Self::build_row("Override", nodes, |node| {
            let local_pending = overridden_nodes.contains(&node.name);
            let wire_overridden = node.overridden == Some(true);
            if !local_pending && !wire_overridden {
                return Cell::from("   off").style(Style::default().fg(Color::DarkGray));
            }
            let label = match (wire_overridden, node.override_generation) {
                (true, Some(g)) => format!("   enabled · gen {g}"),
                (true, None) => "   enabled".to_owned(),
                (false, _) => "   pending".to_owned(),
            };
            let color = if conflicting_overrides && wire_overridden {
                // Two or more overrides claim leadership at incompatible generations —
                // operator must intervene before sequencing diverges.
                Color::Red
            } else if wire_overridden {
                Color::Yellow
            } else {
                Color::DarkGray
            };
            Cell::from(label).style(Style::default().fg(color).add_modifier(Modifier::BOLD))
        });

        let cl_section = Self::section_row("CL", node_count);

        let l2_row = Self::build_row("Unsafe L2", nodes, |node| {
            Self::number_cell(node.unsafe_l2_block, node.is_leader == Some(true), "#")
        });
        let l2_hash_row = Self::build_row("Unsafe Hash", nodes, |node| {
            Self::hash_cell(
                node.unsafe_l2_hash,
                node.is_leader == Some(true),
                node.unsafe_l2_block,
                leader_unsafe,
            )
        });
        let safe_l2_row = Self::build_row("Safe L2", nodes, |node| {
            Self::number_cell(node.safe_l2_block, node.is_leader == Some(true), "#")
        });
        let safe_hash_row = Self::build_row("Safe Hash", nodes, |node| {
            Self::hash_cell(
                node.safe_l2_hash,
                node.is_leader == Some(true),
                node.safe_l2_block,
                leader_safe,
            )
        });
        let finalized_l2_row = Self::build_row("Finalized L2", nodes, |node| {
            Self::number_cell(node.finalized_l2_block, node.is_leader == Some(true), "#")
        });

        let l1_row = Self::build_row("L1 Derived", nodes, |node| {
            match (node.current_l1_block, node.head_l1_block) {
                (Some(cur), Some(head)) => {
                    let lag = head.saturating_sub(cur);
                    let color = if lag > 10 { Color::Yellow } else { Color::Green };
                    Cell::from(format!("   #{cur} / #{head}")).style(Style::default().fg(color))
                }
                _ => Cell::from("   ? / ?").style(Style::default().fg(Color::DarkGray)),
            }
        });

        let cl_peers_row = Self::build_row("CL Peers", nodes, |node| match node.cl_peer_count {
            Some(0) => Cell::from("   0").style(Style::default().fg(Color::Red)),
            Some(n) => Cell::from(format!("   {n}")).style(Style::default().fg(Color::Green)),
            None => Cell::from("   ?").style(Style::default().fg(Color::DarkGray)),
        });

        let el_section = Self::section_row("EL", node_count);

        let el_block_row = Self::build_row("Block", nodes, |node| match node.el_block {
            Some(n) if node.is_leader == Some(true) => {
                Cell::from(format!("   #{n}")).style(Style::default().fg(Color::Yellow))
            }
            Some(n) => Cell::from(format!("   #{n}")).style(Style::default().fg(Color::White)),
            None => Cell::from("   -").style(Style::default().fg(Color::DarkGray)),
        });
        let el_syncing_row = Self::build_row("Syncing", nodes, |node| match node.el_syncing {
            Some(true) => Cell::from("   yes").style(Style::default().fg(Color::Yellow)),
            Some(false) => Cell::from("   no").style(Style::default().fg(Color::Green)),
            None => Cell::from("   -").style(Style::default().fg(Color::DarkGray)),
        });
        let el_peers_row = Self::build_row("EL Peers", nodes, |node| match node.el_peer_count {
            Some(0) => Cell::from("   0").style(Style::default().fg(Color::Red)),
            Some(n) => Cell::from(format!("   {n}")).style(Style::default().fg(Color::Green)),
            None => Cell::from("   -").style(Style::default().fg(Color::DarkGray)),
        });

        let spacer = Row::new(vec![Cell::from("")]).height(1);

        let rows = vec![
            // Leadership
            role_row,
            view_row,
            leader_row,
            cluster_row,
            override_row,
            // CL
            spacer.clone(),
            cl_section,
            l2_row,
            l2_hash_row,
            safe_l2_row,
            safe_hash_row,
            finalized_l2_row,
            l1_row,
            cl_peers_row,
            // EL
            spacer,
            el_section,
            el_block_row,
            el_syncing_row,
            el_peers_row,
        ];
        let table =
            Table::new(rows, constraints).header(header).row_highlight_style(Style::default());

        f.render_stateful_widget(table, inner, &mut TableState::default());
    }

    /// Creates a styled section-separator row for the cluster table.
    pub fn section_row(label: &str, node_count: usize) -> Row<'static> {
        let sep_style = Style::default().fg(Color::DarkGray);
        let heading = format!("── {label} ──────────────");
        let mut cells = vec![Cell::from(heading).style(sep_style)];
        for _ in 0..node_count {
            cells.push(Cell::from("──────────────").style(sep_style));
        }
        Row::new(cells).height(1)
    }
}
