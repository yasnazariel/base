use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    prelude::*,
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState},
};

use crate::{
    app::{Action, Resources, View},
    commands::common::{
        ACTIVITY_COUNT_CHARS, ACTIVITY_LABEL_CHARS, ActivityBarState, COLOR_ACTIVE_BORDER,
        EVENT_GROUP_COUNT, EVENT_GROUP_DEFS, FilterMenuState, VOLUME_STATS_ROWS, block_color,
        build_volume_lines, render_filter_menu, render_sparkline_row, truncate_block_number,
    },
    tui::Keybinding,
};

const KEYBINDINGS: &[Keybinding] = &[
    Keybinding { key: "Esc", description: "Back to home" },
    Keybinding { key: "?", description: "Toggle help" },
    Keybinding { key: "Space", description: "Pause/Resume" },
    Keybinding { key: "Up/k", description: "Scroll up" },
    Keybinding { key: "Down/j", description: "Scroll down" },
    Keybinding { key: "Home/g", description: "Top (auto-scroll)" },
    Keybinding { key: "End/G", description: "Bottom" },
    Keybinding { key: "f", description: "Event filters" },
];

/// Expanded full-screen `DeFi` event activity monitor.
///
/// Each active event filter gets its own full-width sparkline row where every
/// character position maps to a block in the window. The block element height
/// represents the relative event count for that block.
#[derive(Debug)]
pub(crate) struct DefiActivityView {
    table_state: TableState,
    auto_scroll: bool,
    filter_menu: FilterMenuState,
}

impl Default for DefiActivityView {
    fn default() -> Self {
        Self::new()
    }
}

impl DefiActivityView {
    /// Creates a new `DeFi` activity view with auto-scroll enabled.
    pub(crate) fn new() -> Self {
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        Self { table_state, auto_scroll: true, filter_menu: FilterMenuState::default() }
    }
}

impl View for DefiActivityView {
    fn keybindings(&self) -> &'static [Keybinding] {
        KEYBINDINGS
    }

    fn handle_key(&mut self, key: KeyEvent, resources: &mut Resources) -> Action {
        if self.filter_menu.open {
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.filter_menu.move_up(),
                KeyCode::Down | KeyCode::Char('j') => self.filter_menu.move_down(),
                KeyCode::Char(' ') => self.filter_menu.toggle(&mut resources.flash.activity),
                KeyCode::Char('f') | KeyCode::Esc => self.filter_menu.open = false,
                _ => {}
            }
            return Action::None;
        }

        match key.code {
            KeyCode::Char('f') => {
                self.filter_menu.open = true;
                Action::None
            }
            KeyCode::Char(' ') => {
                resources.flash.paused = !resources.flash.paused;
                Action::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(selected) = self.table_state.selected() {
                    if selected > 0 {
                        self.table_state.select(Some(selected - 1));
                        self.auto_scroll = false;
                    } else {
                        self.auto_scroll = true;
                    }
                }
                Action::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(selected) = self.table_state.selected() {
                    let max = resources.flash.activity.window.len().saturating_sub(1);
                    if selected < max {
                        self.table_state.select(Some(selected + 1));
                        self.auto_scroll = false;
                    }
                }
                Action::None
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.table_state.select(Some(0));
                self.auto_scroll = true;
                Action::None
            }
            KeyCode::End | KeyCode::Char('G') => {
                let max = resources.flash.activity.window.len().saturating_sub(1);
                self.table_state.select(Some(max));
                self.auto_scroll = false;
                Action::None
            }
            _ => Action::None,
        }
    }

    fn tick(&mut self, resources: &mut Resources) -> Action {
        if self.auto_scroll && !resources.flash.activity.window.is_empty() {
            self.table_state.select(Some(0));
        }
        Action::None
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, resources: &Resources) {
        let flash = &resources.flash;
        let activity = &flash.activity;
        let window_secs = flash.window_duration_secs();

        let active_count = EVENT_GROUP_DEFS.iter().filter(|g| activity.group_active(g)).count();

        // Layout: volume stats, sparkline bars, block breakdown table.
        let vol_height = VOLUME_STATS_ROWS + 2; // + border
        let bar_height = active_count as u16 + 2; // + border

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(vol_height),
                Constraint::Length(bar_height),
                Constraint::Min(5),
            ])
            .split(area);

        let paused = flash.paused;
        let border_color = if paused { Color::Yellow } else { COLOR_ACTIVE_BORDER };

        render_volume_section(frame, chunks[0], activity, window_secs, border_color);
        render_sparkline_section(frame, chunks[1], activity, paused, border_color);
        render_block_table(frame, chunks[2], activity, border_color, &mut self.table_state);

        if self.filter_menu.open {
            render_filter_menu(frame, area, &activity.active, self.filter_menu.cursor);
        }
    }
}

/// Renders the volume and net flow stats section.
fn render_volume_section(
    frame: &mut Frame<'_>,
    area: Rect,
    activity: &crate::commands::common::ActivityBarState,
    window_secs: Option<f64>,
    border_color: Color,
) {
    let border = Block::default()
        .title(" Volume & Flow Stats (5m) ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    let inner = border.inner(area);
    frame.render_widget(border, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let vol_lines = build_volume_lines(activity, window_secs);
    for (row, line) in vol_lines.iter().enumerate() {
        let y = inner.y + row as u16;
        if y >= inner.y + inner.height {
            break;
        }
        if let Some(line) = line {
            frame.render_widget(
                Paragraph::new(line.clone()),
                Rect { x: inner.x, y, width: inner.width, height: 1 },
            );
        }
    }
}

/// Renders each active filter as a sparkline row using the shared renderer.
fn render_sparkline_section(
    frame: &mut Frame<'_>,
    area: Rect,
    activity: &crate::commands::common::ActivityBarState,
    paused: bool,
    border_color: Color,
) {
    let title = if paused { " DeFi Activity ~5m [PAUSED] " } else { " DeFi Activity ~5m [f] " };

    let border = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    let inner = border.inner(area);
    frame.render_widget(border, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let totals = activity.window_totals();
    let active_group_indices: Vec<usize> =
        (0..EVENT_GROUP_COUNT).filter(|&i| activity.group_active(&EVENT_GROUP_DEFS[i])).collect();

    for (row, &gi) in active_group_indices.iter().enumerate() {
        let y = inner.y + row as u16;
        if y >= inner.y + inner.height {
            break;
        }

        let spark_width =
            (inner.width as usize).saturating_sub(ACTIVITY_LABEL_CHARS + ACTIVITY_COUNT_CHARS);
        let line = render_sparkline_row(
            &EVENT_GROUP_DEFS[gi],
            &activity.window,
            &activity.active,
            totals[gi],
            spark_width,
        );

        frame.render_widget(
            Paragraph::new(line),
            Rect { x: inner.x, y, width: inner.width, height: 1 },
        );
    }
}

/// Renders the per-block event breakdown table.
fn render_block_table(
    frame: &mut Frame<'_>,
    area: Rect,
    activity: &crate::commands::common::ActivityBarState,
    border_color: Color,
    table_state: &mut TableState,
) {
    let active_group_indices: Vec<usize> =
        (0..EVENT_GROUP_COUNT).filter(|&i| activity.group_active(&EVENT_GROUP_DEFS[i])).collect();

    let border = Block::default()
        .title(format!(" Block Breakdown ({} blocks, ~5m) ", activity.window.len()))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    let inner = border.inner(area);
    frame.render_widget(border, area);

    if inner.width == 0 || inner.height == 0 || active_group_indices.is_empty() {
        return;
    }

    let header_style = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    let mut header_cells = vec![Cell::from("Block").style(header_style)];
    for &gi in &active_group_indices {
        header_cells.push(Cell::from(EVENT_GROUP_DEFS[gi].short_label).style(header_style));
    }
    header_cells.push(Cell::from("Tot").style(header_style));
    let header = Row::new(header_cells);

    let mut widths = vec![Constraint::Max(10)];
    for _ in &active_group_indices {
        widths.push(Constraint::Length(5));
    }
    widths.push(Constraint::Length(5));

    let rows: Vec<Row<'_>> = activity
        .window
        .iter()
        .enumerate()
        .map(|(idx, entry)| {
            let is_selected = table_state.selected() == Some(idx);
            let row_style = if is_selected {
                Style::default().bg(crate::commands::common::COLOR_ROW_SELECTED)
            } else {
                Style::default()
            };

            let block_style =
                Style::default().fg(block_color(entry.block_number)).add_modifier(Modifier::BOLD);

            let mut cells =
                vec![Cell::from(truncate_block_number(entry.block_number, 10)).style(block_style)];

            let mut row_total: u32 = 0;
            for &gi in &active_group_indices {
                let group = &EVENT_GROUP_DEFS[gi];
                let count = ActivityBarState::group_count(entry, group, &activity.active);
                row_total = row_total.saturating_add(count);
                let color = group.color;
                let text = if count > 0 { count.to_string() } else { String::new() };
                cells.push(Cell::from(text).style(Style::default().fg(color)));
            }

            cells.push(
                Cell::from(row_total.to_string())
                    .style(Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
            );

            Row::new(cells).style(row_style)
        })
        .collect();

    let table = Table::new(rows, widths).header(header);
    frame.render_stateful_widget(table, inner, table_state);
}
