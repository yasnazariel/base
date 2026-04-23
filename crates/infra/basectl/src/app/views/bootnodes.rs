use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    prelude::*,
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState},
};

use base_bootnode_monitor::BootnodeSnapshot;

use crate::{
    app::{Action, Resources, View},
    commands::COLOR_BASE_BLUE,
    tui::Keybinding,
};

const KEYBINDINGS: &[Keybinding] = &[
    Keybinding { key: "↑/↓ j/k", description: "Scroll peers" },
    Keybinding { key: "Esc", description: "Back to home" },
    Keybinding { key: "?", description: "Toggle help" },
];

/// Display order for network tags in the breakdown row.
const NETWORK_ORDER: &[&str] = &[
    "base-sepolia/azul",
    "base-sepolia/jovian",
    "base-mainnet/azul",
    "base-mainnet/jovian",
    "base-zeronet/azul",
    "base-zeronet/jovian",
    "opstack-unknown",
    "eth-mainnet",
    "eth-sepolia",
    "eth-holesky",
    "eth-hoodi",
    "eth-unknown",
    "no-fork-id",
];

/// Live bootnode and Kademlia DHT peer stats view.
///
/// Displays a summary header with connected-peer counts, a per-bootnode
/// reachability table, and a scrollable Kademlia DHT peer list with
/// network-tag coloring.
#[derive(Debug)]
pub struct BootnodesView {
    scroll: usize,
}

impl BootnodesView {
    /// Creates a new bootnode view.
    pub fn new() -> Self {
        Self { scroll: 0 }
    }
}

impl Default for BootnodesView {
    fn default() -> Self {
        Self::new()
    }
}

impl View for BootnodesView {
    fn keybindings(&self) -> &'static [Keybinding] {
        KEYBINDINGS
    }

    fn handle_key(&mut self, key: KeyEvent, resources: &mut Resources) -> Action {
        let peer_count = resources.bootnodes.snapshot.as_ref().map_or(0, |s| s.peers.len());
        match key.code {
            KeyCode::Up | KeyCode::Char('k') if self.scroll > 0 => {
                self.scroll -= 1;
            }
            KeyCode::Down | KeyCode::Char('j') if self.scroll + 1 < peer_count => {
                self.scroll += 1;
            }
            _ => {}
        }
        Action::None
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, resources: &Resources) {
        match &resources.bootnodes.snapshot {
            None if !resources.bootnodes.configured => render_unconfigured(frame, area),
            None => render_loading(frame, area),
            Some(snapshot) => {
                let peer_count = snapshot.peers.len();
                if self.scroll >= peer_count && peer_count > 0 {
                    self.scroll = peer_count - 1;
                }
                render_snapshot(frame, area, snapshot, self.scroll);
            }
        }
    }
}

fn tag_color(tag: &str) -> Color {
    match tag {
        "base-sepolia/azul" | "base-sepolia/jovian" => Color::Cyan,
        "base-mainnet/azul" | "base-mainnet/jovian" => Color::Blue,
        "base-zeronet/azul" | "base-zeronet/jovian" => Color::LightMagenta,
        "eth-mainnet" => Color::Green,
        "eth-sepolia" => Color::Yellow,
        "eth-holesky" | "eth-hoodi" => Color::Yellow,
        "opstack-unknown" => Color::Magenta,
        "eth-unknown" => Color::Green,
        _ => Color::DarkGray,
    }
}

fn elapsed_str(queried_at: Instant) -> String {
    let secs = queried_at.elapsed().as_secs_f64();
    if secs < 60.0 {
        format!("{secs:.1}s ago")
    } else {
        format!("{:.0}m ago", secs / 60.0)
    }
}

fn render_unconfigured(f: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .title(" Bootnodes ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(COLOR_BASE_BLUE));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1), Constraint::Min(0)])
        .split(inner);

    let msg = Paragraph::new("No bootnodes configured for this network.")
        .alignment(Alignment::Center)
        .style(Style::default().fg(Color::DarkGray));

    f.render_widget(msg, chunks[1]);
}

fn render_loading(f: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .title(" Bootnodes ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(COLOR_BASE_BLUE));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1), Constraint::Min(0)])
        .split(inner);

    let msg = Paragraph::new("Connecting to bootnodes...")
        .alignment(Alignment::Center)
        .style(Style::default().fg(Color::DarkGray));

    f.render_widget(msg, chunks[1]);
}

fn render_snapshot(f: &mut Frame<'_>, area: Rect, snapshot: &BootnodeSnapshot, scroll: usize) {
    let title = format!(" Bootnodes: {} ", snapshot.network_name);
    let block = Block::default()
        .title(title.as_str())
        .borders(Borders::ALL)
        .border_style(Style::default().fg(COLOR_BASE_BLUE));

    let inner = block.inner(area);
    f.render_widget(block, area);

    // Summary block height:
    //   1 header stats line
    //   1 blank
    //   1 network breakdown
    //   1 blank
    //   1 "── Bootnodes ──" separator
    //   N bootnode rows
    //   1 blank
    let bootnode_rows = snapshot.bootnodes.len() as u16;
    let summary_height = 5 + bootnode_rows + 1;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(summary_height), Constraint::Min(0)])
        .split(inner);

    render_summary(f, chunks[0], snapshot);
    render_peers_table(f, chunks[1], snapshot, scroll);
}

fn render_summary(f: &mut Frame<'_>, area: Rect, snapshot: &BootnodeSnapshot) {
    let mut lines: Vec<Line<'_>> = Vec::new();

    // ── Stats header line ────────────────────────────────────────────────────
    let elapsed = elapsed_str(snapshot.queried_at);
    let header = Line::from(vec![
        Span::styled("  Updated: ", Style::default().fg(Color::DarkGray)),
        Span::styled(elapsed, Style::default().fg(Color::White)),
        Span::styled("  │  Connected: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            snapshot.connected_peers.to_string(),
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  │  DHT peers: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            snapshot.peers.len().to_string(),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
    ]);
    lines.push(header);
    lines.push(Line::from(""));

    // ── Network breakdown ────────────────────────────────────────────────────
    let mut breakdown_spans: Vec<Span<'_>> = vec![Span::styled(
        "  Network Breakdown  ",
        Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
    )];
    for &tag in NETWORK_ORDER {
        if let Some(&count) = snapshot.network_counts.get(tag) {
            if count > 0 {
                breakdown_spans.push(Span::styled(
                    tag,
                    Style::default().fg(tag_color(tag)).add_modifier(Modifier::BOLD),
                ));
                breakdown_spans.push(Span::styled(
                    format!(": {count}   "),
                    Style::default().fg(Color::White),
                ));
            }
        }
    }
    lines.push(Line::from(breakdown_spans));
    lines.push(Line::from(""));

    // ── Bootnodes section separator ──────────────────────────────────────────
    lines.push(Line::from(vec![Span::styled(
        "  ── Bootnodes ──────────────────────────────────────────────────────────────",
        Style::default().fg(Color::DarkGray),
    )]));

    // ── Per-bootnode rows ────────────────────────────────────────────────────
    for bn in &snapshot.bootnodes {
        let (icon, icon_style) = if bn.reachable {
            ("  ✓  ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD))
        } else {
            ("  ✗  ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
        };

        let mut spans = vec![
            Span::styled(icon, icon_style),
            Span::styled(
                format!("{:<26}", bn.label),
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            ),
        ];

        if bn.reachable {
            spans.push(Span::styled(
                format!("{} peers", bn.peer_count),
                Style::default().fg(Color::Cyan),
            ));
            spans.push(Span::styled(
                format!("   {}ms", bn.query_ms),
                Style::default().fg(Color::DarkGray),
            ));
        } else {
            let err = bn.error.as_deref().unwrap_or("unreachable");
            spans.push(Span::styled(err, Style::default().fg(Color::Red)));
            spans.push(Span::styled("   —", Style::default().fg(Color::DarkGray)));
        }

        lines.push(Line::from(spans));
    }

    f.render_widget(Paragraph::new(lines), area);
}

fn render_peers_table(f: &mut Frame<'_>, area: Rect, snapshot: &BootnodeSnapshot, scroll: usize) {
    let peer_count = snapshot.peers.len();

    // ── Section header line ──────────────────────────────────────────────────
    let header_line = Line::from(vec![
        Span::styled(
            "  ── Kademlia DHT Peers ── ",
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!("{peer_count} total"),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "  [↑↓ j/k scroll]",
            Style::default().fg(Color::DarkGray),
        ),
    ]);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);

    f.render_widget(Paragraph::new(header_line), chunks[0]);

    // ── Peers table ──────────────────────────────────────────────────────────
    let header = Row::new([
        Cell::from("  NODE ID")
            .style(Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Cell::from("ADDRESS")
            .style(Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Cell::from("FORK")
            .style(Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
    ])
    .height(1);

    let rows: Vec<Row<'_>> = snapshot
        .peers
        .iter()
        .map(|p| {
            Row::new([
                Cell::from(format!("  {}", p.node_id_prefix))
                    .style(Style::default().fg(Color::DarkGray)),
                Cell::from(p.address.as_str()).style(Style::default().fg(Color::White)),
                Cell::from(p.network_tag).style(
                    Style::default()
                        .fg(tag_color(p.network_tag))
                        .add_modifier(Modifier::BOLD),
                ),
            ])
            .height(1)
        })
        .collect();

    let widths = [Constraint::Length(14), Constraint::Length(27), Constraint::Min(0)];

    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));

    let mut state = TableState::default().with_selected(Some(scroll));
    f.render_stateful_widget(table, chunks[1], &mut state);
}

#[cfg(test)]
mod tests {
    use super::{elapsed_str, tag_color};
    use ratatui::style::Color;

    #[test]
    fn tag_color_known_tags() {
        assert_eq!(tag_color("base-sepolia/azul"), Color::Cyan);
        assert_eq!(tag_color("base-mainnet/azul"), Color::Blue);
        assert_eq!(tag_color("eth-mainnet"), Color::Green);
        assert_eq!(tag_color("unknown-tag"), Color::DarkGray);
    }

    #[test]
    fn elapsed_str_seconds() {
        // Use an instant slightly in the past — just verify the format compiles
        // and does not panic; exact values depend on runtime timing.
        let now = std::time::Instant::now();
        let s = elapsed_str(now);
        assert!(s.ends_with("s ago") || s.ends_with("m ago"));
    }
}
