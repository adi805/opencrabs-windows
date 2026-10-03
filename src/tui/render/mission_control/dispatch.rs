//! Mission Control top-level dispatch — paints the backdrop, computes
//! the panel layout, calls each panel renderer, then overlays the help
//! bar (and detail popup, when open).

use super::layout::{self, McLayout};
use super::theme;
use super::{activity_panel, analytics_panel, detail_popup, inbox_panel, schedule_panel};

use crate::tui::app::App;
use crate::tui::app::dialog_keys::{DialogScope, dialog_keys};
use crate::tui::app::mission_control::McPanel;
use crate::tui::render::hints;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

/// Render Mission Control over the full content area `area`. Inherits
/// the terminal background — no dark wash — to match the `Sessions`
/// and `Help` screens.
pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    // The log viewer is full-screen: it replaces the panels rather than
    // overlaying them, so nothing below it is drawn or laid out (#1528).
    if app.mc.log_viewer.is_some() {
        super::log_viewer::draw(frame, app, area);
        return;
    }
    let McLayout {
        inbox,
        analytics,
        activity,
        schedule,
        help_bar,
    } = layout::compute(area);

    let focus = app.mc.focused_panel;
    inbox_panel::draw(frame, app, inbox, focus == McPanel::Inbox);
    analytics_panel::draw(frame, app, analytics, focus == McPanel::Analytics);
    activity_panel::draw(frame, app, activity, focus == McPanel::Activity);
    schedule_panel::draw(frame, app, schedule, focus == McPanel::Schedule);

    if help_bar.height > 0 {
        draw_help_bar(frame, app, help_bar);
    }

    if app.mc.detail_open {
        detail_popup::draw(frame, app, area);
    }
}

/// Bottom bar (2 rows): the canonical dialog footer from the scoped
/// keymap on the first row, the active analytics window on the second
/// (#1775, #900). Footer and input handler both answer to
/// `dialog_keys(MissionControl)`, and the drift test in
/// `dialog_keys.rs` replays every advertised event through `decide()`
/// to prove the pair cannot drift.
fn draw_help_bar(frame: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(area);

    hints::render_footer(frame, rows[0], dialog_keys(DialogScope::MissionControl));

    let active = window_word(app.mc.analytics_window);
    let filter = Line::from(vec![
        Span::styled("active: ", theme::dim()),
        Span::styled(
            active,
            Style::default()
                .fg(theme::border_analytics_focus())
                .add_modifier(Modifier::BOLD),
        ),
    ]);

    if rows.len() > 1 {
        frame.render_widget(Paragraph::new(filter), rows[1]);
    }
}

/// Human-readable window name for the commands bar (#900).
fn window_word(window: crate::brain::mission_control::TimeWindow) -> &'static str {
    use crate::brain::mission_control::TimeWindow as W;
    match window {
        W::Day => "Day",
        W::Week => "Week",
        W::Month => "Month",
        W::All => "All",
    }
}
