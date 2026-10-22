//! Session list rendering
//!
//! Session manager view with navigation, renaming, and status indicators.
//!
//! The list is full-bleed (#2162): no border, no app title row, one row of
//! footer at the bottom carrying the keys from the scoped keymap. Every
//! session is exactly one terminal row, whatever the width, so the scroll
//! math and the viewport can never disagree about where a session is.

use super::super::app::App;
use super::hints;
use super::palette;
use super::responsive::{self, SessionRowTier};
use super::theme::{self, Role};
use super::utils::{format_token_count_raw, format_token_count_with_label, truncate_to_width};
use crate::db::models::Session;
use crate::tui::app::dialog_keys::{DialogScope, dialog_keys};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::UnicodeWidthStr;

/// On-brand palette for project badges — variations of orange, cyan, white,
/// and blue. Each project is mapped to one entry deterministically so the same
/// project always shows the same colour and different projects stay
/// distinguishable in the session list.
const PROJECT_BADGE_COLORS: &[Color] = palette::PROJECT_BADGE_COLORS;

/// Pick a stable badge colour for `project_id` from [`PROJECT_BADGE_COLORS`].
fn project_badge_color(project_id: uuid::Uuid) -> Color {
    let idx = (project_id.as_u128() % PROJECT_BADGE_COLORS.len() as u128) as usize;
    PROJECT_BADGE_COLORS[idx]
}

/// Render the sessions list.
///
/// Content fills everything above the last two rows; the second to last is a
/// dim rule and the last is the shared command footer. Without the rule the
/// footer sat flush under the final session and read as one more row, which
/// is what made the screen hard to parse. Nothing else is drawn, which is
/// where the three rows the bordered version spent on chrome now go.
pub(super) fn render_sessions(f: &mut Frame, app: &App, area: Rect) {
    let [content_area, rule_area, footer_area] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);

    // Compute filtered session indices based on search
    let visible_indices: Vec<usize> = if app.session_search.is_empty() {
        (0..app.sessions.len()).collect()
    } else {
        let search = app.session_search.to_lowercase();
        app.sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                let name = s.title.as_deref().unwrap_or("New Chat").to_lowercase();
                name.contains(&search)
            })
            .map(|(i, _)| i)
            .collect()
    };

    let mut lines: Vec<Line<'static>> = Vec::new();

    // A pane that was just split is waiting for a session: state, one row.
    let has_unassigned = app
        .pane_manager
        .focused_pane()
        .is_some_and(|p| p.session_id.is_none());
    if has_unassigned {
        lines.push(Line::from(Span::styled(
            "  Select a session for the new pane (or n for new)",
            Style::default().fg(theme::role(Role::Success)),
        )));
    }

    // Assign mode banner, one row.
    if let Some(project_id) = app.assigning_to_project {
        let project_name = app
            .project_name_cache
            .get(&project_id)
            .map(|s| s.as_str())
            .unwrap_or("project");
        lines.push(Line::from(vec![
            Span::styled(
                format!("  Assigning to {project_name}"),
                Style::default()
                    .fg(theme::role(Role::Warning))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "  Enter: assign  Esc: done",
                Style::default().fg(theme::role(Role::TextDim)),
            ),
        ]));
    }

    // Search editor, only while the user is typing a filter.
    if app.session_search_active {
        lines.push(Line::from(vec![
            Span::styled("  🔍 ", Style::default().fg(theme::role(Role::AccentTeal))),
            Span::styled(
                format!("{}▏", app.session_search),
                Style::default()
                    .fg(theme::role(Role::TextPrimary))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {} matches", visible_indices.len()),
                Style::default().fg(theme::role(Role::GrayDim)),
            ),
        ]));
    }

    // Track header line count for viewport scroll calculation
    let header_line_count = lines.len();

    if visible_indices.is_empty() && !app.session_search.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No matching sessions",
            Style::default().fg(theme::role(Role::GrayDim)),
        )));
    }

    let tier = responsive::session_row_tier(content_area.width);

    for (display_idx, &session_idx) in visible_indices.iter().enumerate() {
        let session = &app.sessions[session_idx];
        let is_selected = display_idx == app.selected_session_index;
        let is_current = app
            .current_session
            .as_ref()
            .map(|s| s.id == session.id)
            .unwrap_or(false);

        if is_selected && app.session_renaming {
            lines.push(rename_row(app, content_area.width));
            continue;
        }

        lines.push(session_row(
            app,
            session,
            is_selected,
            is_current,
            tier,
            content_area.width,
        ));
    }

    // Viewport scroll: keep the selected session visible. One row per
    // session, so the row index is the line index.
    let visible_height = (content_area.height as usize).max(1);
    let selected_line = header_line_count + app.selected_session_index;
    let scroll = if selected_line >= visible_height {
        (selected_line.saturating_sub(visible_height) + 1) as u16
    } else {
        0u16
    };

    f.render_widget(Paragraph::new(lines).scroll((scroll, 0)), content_area);

    hints::render_separator(f, rule_area);
    hints::render_footer_with_context(
        f,
        footer_area,
        dialog_keys(DialogScope::Sessions),
        &format!("{} sessions", visible_indices.len()),
    );
}

/// One session, exactly one terminal row.
///
/// The name takes whatever room is left after the badges the current width
/// tier allows, and is truncated with an ellipsis rather than wrapped, so a
/// long channel title can never push the next session down a row.
fn session_row(
    app: &App,
    session: &Session,
    is_selected: bool,
    is_current: bool,
    tier: SessionRowTier,
    width: u16,
) -> Line<'static> {
    let width = width as usize;
    let dim = Style::default().fg(theme::role(Role::GrayDim));
    let mid = Style::default().fg(theme::role(Role::GrayMid));
    let gray = Style::default().fg(theme::role(Role::Gray));

    // Everything to the right of the name, for one tier. Built first because
    // its width decides how much room the name gets, and built as a closure
    // because a tier whose badges do not fit is dropped for the next one
    // down rather than clipped at the edge.
    let build_right = |tier: SessionRowTier| -> Vec<Span<'static>> {
        let mut right: Vec<Span<'static>> = Vec::new();

        // Show LAST ACTIVITY, not creation. The list is ordered by
        // `updated_at DESC` (`list_sessions`), so printing `created_at` made the
        // rows look mis-sorted: a session last used minutes ago but opened in
        // March rendered "2026-03-26" above one created yesterday.
        match tier {
            SessionRowTier::Narrow => right.push(Span::styled(
                format!(" {}", session.updated_at.format("%H:%M")),
                dim,
            )),
            _ => right.push(Span::styled(
                format!(" {}", session.updated_at.format("%Y-%m-%d %H:%M")),
                dim,
            )),
        }

        if tier != SessionRowTier::Narrow && session.token_count > 0 {
            right.push(Span::styled(
                format!(
                    "  {}",
                    format_token_count_with_label(session.token_count, "total")
                ),
                mid,
            ));
        }

        if matches!(tier, SessionRowTier::Wide | SessionRowTier::ExtraWide) {
            if is_current && let Some(input_tok) = app.last_input_tokens {
                let pct = app.context_usage_percent();
                let color = if pct > 80.0 {
                    theme::role(Role::Error)
                } else if pct > 50.0 {
                    theme::role(Role::Accent)
                } else {
                    theme::role(Role::AccentTeal)
                };
                right.push(Span::styled(
                    format!(
                        "  ctx {}/{} {:.0}%",
                        format_token_count_raw(input_tok as i64),
                        format_token_count_raw(app.context_max_tokens as i64),
                        pct
                    ),
                    Style::default().fg(color),
                ));
            }
            if let Some(prov) = session.provider_name.as_ref() {
                let model = session.model.as_deref().unwrap_or("default");
                right.push(Span::styled(format!("  [{prov}/{model}]"), gray));
            }
        }

        if tier == SessionRowTier::ExtraWide {
            if let Some(pid) = session.project_id {
                let project_name = app
                    .project_name_cache
                    .get(&pid)
                    .map(|s| s.as_str())
                    .unwrap_or("?");
                right.push(Span::styled(
                    format!("  {{.{project_name}}}"),
                    Style::default().fg(project_badge_color(pid)),
                ));
            }
            if let Some(wd) = session.working_directory.as_ref() {
                let home_dir = dirs::home_dir()
                    .map(|h| h.to_string_lossy().to_string())
                    .unwrap_or_default();
                let short = if !home_dir.is_empty() && wd.starts_with(&home_dir) {
                    format!("~{}", &wd[home_dir.len()..])
                } else {
                    wd.clone()
                };
                right.push(Span::styled(
                    format!("  {short}"),
                    Style::default().fg(theme::role(Role::BlueSteel)),
                ));
                if let Some(branch) =
                    crate::utils::git_branch::current_branch(std::path::Path::new(wd))
                {
                    right.push(Span::styled(
                        format!(" ({branch})"),
                        Style::default().fg(theme::role(Role::AccentTeal)),
                    ));
                }
            }
        }

        // Status is state, not decoration, so it renders at every width.
        if app.processing_sessions.contains(&session.id) {
            const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let frame = app.animation_frame % SPINNER.len();
            right.push(Span::styled(
                format!(" {}", SPINNER[frame]),
                Style::default().fg(theme::role(Role::Accent)),
            ));
        } else if app.sessions_with_pending_approval.contains(&session.id) {
            right.push(Span::styled(
                " !",
                Style::default()
                    .fg(theme::role(Role::Accent))
                    .add_modifier(Modifier::BOLD),
            ));
        } else if app.sessions_with_unread.contains(&session.id) {
            right.push(Span::styled(
                " ●",
                Style::default().fg(theme::role(Role::AccentTeal)),
            ));
        }

        if is_current {
            right.push(Span::styled(
                " *",
                Style::default()
                    .fg(theme::role(Role::Gray))
                    .add_modifier(Modifier::BOLD),
            ));
        }

        // Pane indicator: which pane this session already occupies.
        if app.pane_manager.is_split() {
            let pane_ids = app.pane_manager.pane_ids_in_order();
            if let Some(pos) = pane_ids.iter().position(|pid| {
                app.pane_manager
                    .get(*pid)
                    .is_some_and(|p| p.session_id == Some(session.id))
            }) {
                right.push(Span::styled(
                    format!(" [pane {}]", pos + 1),
                    Style::default().fg(theme::role(Role::Success)),
                ));
            }
        }

        right
    };

    // The tier comes from the terminal width, but the badges themselves can
    // still be wider than the terminal: a long provider/model pair or a deep
    // working directory used to push the row past `width`, and the terminal
    // then clipped it mid-token (the `[oper` at the right edge). Dropping
    // whole tiers is how the row fits, so what is shown is never cut in half.
    let gutter_width = "  ".len();
    let mut tier = tier;
    let mut right = build_right(tier);
    let mut right_width: usize = right.iter().map(|s| s.content.width()).sum();
    while gutter_width + 2 + right_width > width {
        match tier.narrower() {
            Some(next) => {
                tier = next;
                right = build_right(tier);
                right_width = right.iter().map(|s| s.content.width()).sum();
            }
            None => break,
        }
    }

    // Green marks a session already assigned to the project being assigned.
    let is_assigned_to_target = app
        .assigning_to_project
        .is_some_and(|pid| session.project_id == Some(pid));
    let name_style = if is_selected {
        Style::default()
            .fg(theme::role(Role::Accent))
            .add_modifier(Modifier::BOLD)
    } else if is_assigned_to_target {
        Style::default()
            .fg(theme::role(Role::Success))
            .add_modifier(Modifier::BOLD)
    } else if is_current {
        Style::default().fg(theme::role(Role::Gray))
    } else {
        Style::default().fg(theme::role(Role::TextPrimary))
    };

    // Two-space gutter, then the name, then one blank cell, then the badges.
    let gutter = "  ";
    let name_room = width.saturating_sub(gutter.len() + right_width + 1).max(1);
    let name = truncate_to_width(session.title.as_deref().unwrap_or("New Chat"), name_room);

    let mut spans = vec![
        Span::raw(gutter.to_string()),
        Span::styled(name.clone(), name_style),
    ];
    let used = gutter.len() + name.width();
    let gap = width.saturating_sub(used + right_width);
    if gap > 0 {
        spans.push(Span::raw(" ".repeat(gap)));
    }
    spans.extend(right);

    finish_row(spans, width, is_selected)
}

/// The rename editor, on the selected row, one row high.
fn rename_row(app: &App, width: u16) -> Line<'static> {
    let width = width as usize;
    let gutter = "  ";
    let room = width.saturating_sub(gutter.len() + 1).max(1);
    let text = truncate_to_width(&format!("{}▏", app.session_rename_buffer), room);
    let spans = vec![
        Span::raw(gutter.to_string()),
        Span::styled(
            text,
            Style::default()
                .fg(theme::role(Role::TextPrimary))
                .add_modifier(Modifier::BOLD),
        ),
    ];
    finish_row(spans, width, true)
}

/// Give the selected row a full-width selection bar.
///
/// The background has to reach the last column, so the row is padded to
/// `width` first: a bar that stops at the end of the text reads as a
/// highlight of the text rather than of the row.
fn finish_row(mut spans: Vec<Span<'static>>, width: usize, is_selected: bool) -> Line<'static> {
    if !is_selected {
        return Line::from(spans);
    }
    let bg = theme::role(Role::SelectionBg);
    let used: usize = spans.iter().map(|s| s.content.width()).sum();
    if used < width {
        spans.push(Span::styled(
            " ".repeat(width - used),
            Style::default().bg(bg),
        ));
    }
    Line::from(
        spans
            .into_iter()
            .map(|s| Span::styled(s.content, s.style.bg(bg)))
            .collect::<Vec<_>>(),
    )
}
