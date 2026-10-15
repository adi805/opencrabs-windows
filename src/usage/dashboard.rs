//! Usage dashboard layout and state
//!
//! Centered overlay panel (~75% of screen), responsive to different sizes.

use super::cards;
use super::data::{DashboardData, Period};
use crate::tui::app::dialog_keys::{DialogKey, DialogScope, dialog_keys};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Span,
    widgets::{Block, Borders, Clear},
};

/// Number of focusable cards
pub(crate) const CARD_COUNT: usize = 6;

/// Grid index of the Cache Efficiency card: 0=daily, 1=project, 2=model,
/// 3=tools, 4=activity, 5=cache.
pub(crate) const CACHE_CARD: usize = 5;

/// Dashboard UI state (lives in App)
#[derive(Debug, Clone)]
pub struct DashboardState {
    pub period: Period,
    pub focused_card: usize,
    pub data: DashboardData,
    /// The cache price breakdown is open over the grid (#2027).
    pub cache_detail_open: bool,
}

impl Default for DashboardState {
    fn default() -> Self {
        Self {
            period: Period::AllTime,
            focused_card: 0,
            data: DashboardData::default(),
            cache_detail_open: false,
        }
    }
}

impl DashboardState {
    pub fn focus_next(&mut self) {
        self.focused_card = (self.focused_card + 1) % CARD_COUNT;
    }

    pub fn focus_prev(&mut self) {
        self.focused_card = if self.focused_card == 0 {
            CARD_COUNT - 1
        } else {
            self.focused_card - 1
        };
    }

    /// Set period and return true if changed (caller should re-fetch data)
    pub fn set_period(&mut self, period: Period) -> bool {
        if self.period != period {
            self.period = period;
            true
        } else {
            false
        }
    }

    /// Is the Cache Efficiency card the focused one? Only that card has a
    /// price breakdown to open (#2027).
    pub fn cache_card_focused(&self) -> bool {
        self.focused_card == CACHE_CARD
    }
}

/// What the dashboard does with a key press.
///
/// The dashboard's own state changes (focus moves, opening and closing the
/// breakdown) happen inside [`decide`]; the variants carry only what needs
/// the App: re-fetching after a period change, and leaving the mode. Splitting
/// it this way makes every key decision testable with no App, no database and
/// no terminal (#2027).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageKeyOutcome {
    /// Handled inside the dashboard.
    Consumed,
    /// Switch the reporting period and re-fetch.
    Period(Period),
    /// Leave the dashboard.
    Close,
    /// Not a dashboard key; the caller keeps it.
    NotConsumed,
}

/// The dashboard's answer to `event`, applying the state changes that belong
/// to the decision itself.
///
/// The render side asks [`advertises`] which keys are live, so the footer a
/// user reads and the keys the handler honours come from the same rules and
/// cannot drift (#2027).
pub fn decide(state: &mut DashboardState, event: KeyEvent) -> UsageKeyOutcome {
    if state.cache_detail_open {
        // The breakdown sits over the grid: Esc backs out of it, and every
        // other key is deliberately inert so the dashboard behind it cannot
        // move while the prices are being read.
        if event.code == KeyCode::Esc {
            state.cache_detail_open = false;
        }
        return UsageKeyOutcome::Consumed;
    }

    match event.code {
        KeyCode::Esc => UsageKeyOutcome::Close,
        KeyCode::Tab => {
            state.focus_next();
            UsageKeyOutcome::Consumed
        }
        KeyCode::BackTab => {
            state.focus_prev();
            UsageKeyOutcome::Consumed
        }
        KeyCode::Enter if state.cache_card_focused() => {
            state.cache_detail_open = true;
            UsageKeyOutcome::Consumed
        }
        KeyCode::Char('t') | KeyCode::Char('T') => UsageKeyOutcome::Period(Period::Today),
        KeyCode::Char('w') | KeyCode::Char('W') => UsageKeyOutcome::Period(Period::Week),
        KeyCode::Char('m') | KeyCode::Char('M') => UsageKeyOutcome::Period(Period::Month),
        KeyCode::Char('a') | KeyCode::Char('A') => UsageKeyOutcome::Period(Period::AllTime),
        _ => UsageKeyOutcome::NotConsumed,
    }
}

/// Does `code` do, in this state, what the footer says it does?
///
/// Deliberately narrower than [`decide`]: with the breakdown open every key is
/// consumed but only Esc performs an action, and a key that performs nothing
/// must not be advertised.
pub fn advertises(state: &DashboardState, code: KeyCode) -> bool {
    if state.cache_detail_open {
        return code == KeyCode::Esc;
    }
    match code {
        KeyCode::Esc | KeyCode::Tab | KeyCode::BackTab => true,
        KeyCode::Char('t' | 'T' | 'w' | 'W' | 'm' | 'M' | 'a' | 'A') => true,
        // Enter opens the breakdown, so it is live only where there is one
        // to open (#2027).
        KeyCode::Enter => state.cache_card_focused(),
        _ => false,
    }
}

/// The dashboard's footer entries for this state: the scoped keymap filtered
/// to the keys that are live.
///
/// The rendered footer and the drift tests both come through here, so what a
/// user is told about and what the handler honours are decided by
/// [`advertises`] alone (#2027). Entries are handed back by value
/// ([`DialogKey`] is `Copy`) so the footer takes them as a plain slice.
pub fn advertised_keys(state: &DashboardState) -> Vec<DialogKey> {
    dialog_keys(DialogScope::UsageDashboard)
        .iter()
        .filter(|key| key.events.iter().any(|(code, _)| advertises(state, *code)))
        .copied()
        .collect()
}

/// Compute a centered rect that takes ~75% of the terminal, clamped to min/max.
pub(crate) fn centered_rect(area: Rect) -> Rect {
    // Target ~75% but at least 60 cols / 20 rows, at most area - 4 margin each side
    let w = (area.width * 3 / 4)
        .max(60.min(area.width))
        .min(area.width.saturating_sub(4));
    let h = (area.height * 3 / 4)
        .max(20.min(area.height))
        .min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}

/// Render the usage dashboard as a centered overlay.
///
/// Layout inside the panel:
/// ```text
/// [Summary Bar ─────────────────── full width]
/// [Daily Activity] [By Project]
/// [By Model      ] [Core Tools]
/// [By Activity ────────────────] [Cache Efficiency]
/// [Footer ────────────────────── full width]
/// ```
pub fn render(f: &mut Frame, state: &DashboardState, area: Rect) {
    let panel = centered_rect(area);

    // Clear the area behind the panel
    f.render_widget(Clear, panel);

    // Outer border
    let border = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(
            " Usage Dashboard ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    let border_inner = border.inner(panel);
    f.render_widget(border, panel);

    // 1-cell horizontal padding inside the border
    let inner = Rect {
        x: border_inner.x + 1,
        y: border_inner.y,
        width: border_inner.width.saturating_sub(2),
        height: border_inner.height,
    };

    // Activity + Cache row scales with the panel height (responsive to terminal
    // resize / zoom), like the grid cards above — so the per-model Cache
    // Efficiency list (and the activity list) reveal more rows as you zoom out
    // instead of being pinned to two. Clamped so it never starves the 2x2 grid
    // on a small panel or balloon on a huge one.
    let activity_height = (inner.height / 4).clamp(5, 16);
    let grid_min = if inner.height > 30 { 10 } else { 7 };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),               // summary bar
            Constraint::Min(grid_min),           // middle grid (2x2)
            Constraint::Length(activity_height), // activity + cache row
            Constraint::Length(1),               // footer
        ])
        .split(inner);

    // Summary bar
    cards::render_summary(f, &state.data, chunks[0], state.period.label());

    // Middle grid: 2x2
    // Row 1: Daily Activity | By Project
    // Row 2: By Model | Core Tools
    let mid_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(chunks[1]);

    let top_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(mid_rows[0]);

    let bot_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(mid_rows[1]);

    // Card index: 0=daily, 1=project, 2=model, 3=tools, 4=activity, 5=cache
    cards::render_daily(f, &state.data.daily, top_cols[0], state.focused_card == 0);
    cards::render_projects(
        f,
        &state.data.projects,
        top_cols[1],
        state.focused_card == 1,
    );
    cards::render_models(f, &state.data.models, bot_cols[0], state.focused_card == 2);
    cards::render_tools(f, &state.data.tools, bot_cols[1], state.focused_card == 3);

    // Bottom row: Activity (75%) | Cache Efficiency (25%)
    let bottom_row = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(75), Constraint::Percentage(25)])
        .split(chunks[2]);

    cards::render_activities(
        f,
        &state.data.activities,
        bottom_row[0],
        state.focused_card == 4,
    );
    cards::render_cache_efficiency(
        f,
        &state.data.cache,
        bottom_row[1],
        state.cache_card_focused(),
    );

    // Footer: the keys that are live in this state (#2027).
    cards::render_footer(f, chunks[3], state);

    // The price breakdown, if Enter opened it. Drawn last so it covers the
    // grid, and sized to stop one row above the footer so the key that
    // dismisses it stays on screen.
    if state.cache_detail_open {
        let overlay = Rect {
            x: panel.x,
            y: panel.y,
            width: panel.width,
            height: chunks[3].y.saturating_sub(panel.y),
        };
        cards::render_cache_breakdown(f, &state.data.cache, overlay);
    }
}
