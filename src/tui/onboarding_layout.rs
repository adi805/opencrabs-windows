//! Screen geometry for the onboarding wizard (#1973).
//!
//! The wizard owns the whole terminal: a pinned header, a scrolling content
//! column and a pinned key-hint footer. Everything here is pure so the sizing
//! and scroll rules can be tested without a terminal.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Widest the content column grows. Forms stretched across an ultrawide
/// terminal are harder to scan than a column, so past this the extra width
/// becomes margin.
pub const MAX_CONTENT_WIDTH: u16 = 100;

/// Gutter kept on each side of the content column on narrow terminals.
const SIDE_GUTTER: u16 = 2;

/// Fewest rows a windowed list (providers, models) shrinks to.
pub const MIN_LIST_ROWS: usize = 3;

/// Most rows a windowed list shows when the terminal has room.
pub const MAX_LIST_ROWS: usize = 8;

/// The logo only renders when the terminal has this much room, so a small
/// window spends its rows on the form instead.
pub const LOGO_MIN_WIDTH: u16 = 60;
pub const LOGO_MIN_HEIGHT: u16 = 30;

/// Width of the content column for a terminal `area_width` columns wide.
pub fn content_width(area_width: u16) -> u16 {
    area_width
        .saturating_sub(SIDE_GUTTER * 2)
        .min(MAX_CONTENT_WIDTH)
        .max(area_width.min(20))
}

/// Whether the ASCII logo fits on a terminal of this size.
pub fn shows_logo(area_width: u16, area_height: u16) -> bool {
    area_width >= LOGO_MIN_WIDTH && area_height >= LOGO_MIN_HEIGHT
}

/// Columns the left-side step timeline takes, gutter included (#1979).
pub const TIMELINE_WIDTH: u16 = 28;
/// Narrowest terminal that gets the timeline. Below this the content column
/// needs every column and the progress dots stay in the header instead.
pub const TIMELINE_MIN_AREA_WIDTH: u16 = 110;

/// Whether the left-side timeline fits a terminal this wide.
pub fn shows_timeline(area_width: u16) -> bool {
    area_width >= TIMELINE_MIN_AREA_WIDTH
}

/// How a timeline node reads relative to the step the user is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeState {
    Done,
    Current,
    Upcoming,
}

/// State of the `index`-th step (0-based) when the user is on the 1-based
/// `current` step. `current` past the last step (Complete) marks all done.
pub fn node_state(index: usize, current: usize) -> NodeState {
    match (index + 1).cmp(&current) {
        std::cmp::Ordering::Less => NodeState::Done,
        std::cmp::Ordering::Equal => NodeState::Current,
        std::cmp::Ordering::Greater => NodeState::Upcoming,
    }
}

/// How the timeline spends the rows it has: a connector row between nodes
/// when there is room, nodes only when not, nothing when even that is too
/// tall (the header dots take over). Three rows are kept for the heading:
/// the brand line, the step counter and a blank row (#1980).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineFit {
    Spacious,
    Compact,
    Hidden,
}

/// Rows above the first timeline node: brand, step counter, blank.
const TIMELINE_HEADING_ROWS: usize = 3;

pub fn timeline_fit(steps: usize, rows: u16) -> TimelineFit {
    let rows = rows as usize;
    if steps == 0 {
        TimelineFit::Hidden
    } else if TIMELINE_HEADING_ROWS + steps * 2 - 1 <= rows {
        TimelineFit::Spacious
    } else if TIMELINE_HEADING_ROWS + steps <= rows {
        TimelineFit::Compact
    } else {
        TimelineFit::Hidden
    }
}

/// Blank rows around the header and footer text (#1975). `outer` sits
/// between the text and the screen edge, `inner` between the text and the
/// content. Small terminals give the rows back to the form: inner padding
/// goes first, then outer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BandPadding {
    pub outer: u16,
    pub inner: u16,
}

/// Terminal height at which the bands get padding on both sides.
pub const BAND_FULL_PADDING_HEIGHT: u16 = 30;
/// Terminal height below which the bands get no padding at all.
pub const BAND_EDGE_PADDING_HEIGHT: u16 = 20;

/// Whether the header puts a blank row between its lines (#1980). Same
/// threshold as the full band padding: below it every row goes to the form.
pub fn header_spacing(area_height: u16) -> bool {
    area_height >= BAND_FULL_PADDING_HEIGHT
}

pub fn band_padding(area_height: u16) -> BandPadding {
    if area_height >= BAND_FULL_PADDING_HEIGHT {
        BandPadding { outer: 1, inner: 1 }
    } else if area_height >= BAND_EDGE_PADDING_HEIGHT {
        BandPadding { outer: 1, inner: 0 }
    } else {
        BandPadding { outer: 0, inner: 0 }
    }
}

/// Scroll offset that keeps `focus_row` in view with two rows of context
/// above it, plus whatever the user added with Page Up / Page Down.
pub fn scroll_offset(
    focus_row: usize,
    total_rows: usize,
    visible: usize,
    user_extra: usize,
) -> usize {
    let max_scroll = total_rows.saturating_sub(visible);
    let focus = if focus_row > 2 && total_rows > visible {
        focus_row.saturating_sub(2)
    } else {
        0
    };
    focus.saturating_add(user_extra).min(max_scroll)
}

/// Hard-wrap a styled line to `width` display columns, preferring to break
/// after a space. Styles survive the split, so a wrapped field label keeps
/// its color. Wrapping here instead of in the `Paragraph` is what lets the
/// scroll math count real rows: a line that wraps to three rows is three
/// rows, so the fields under it stay reachable.
pub fn wrap_line(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    let total: usize = line.spans.iter().map(|s| s.content.width()).sum();
    if width == 0 || total <= width {
        return vec![line];
    }
    let cells: Vec<(char, Style)> = line
        .spans
        .iter()
        .flat_map(|s| s.content.chars().map(move |c| (c, s.style)))
        .collect();

    let mut out = Vec::new();
    let mut start = 0;
    while start < cells.len() {
        let mut used = 0;
        let mut end = start;
        while end < cells.len() {
            let w = cells[end].0.width().unwrap_or(0);
            if used + w > width {
                break;
            }
            used += w;
            end += 1;
        }
        if end == start {
            // A single glyph wider than the column: emit it alone.
            end = start + 1;
        }
        if end < cells.len()
            && let Some(sp) = cells[start..end].iter().rposition(|(c, _)| *c == ' ')
            && sp > 0
        {
            end = start + sp + 1;
        }
        let mut piece = cells_to_line(&cells[start..end]);
        piece.style = line.style;
        piece.alignment = line.alignment;
        out.push(piece);
        start = end;
        // The space a row broke on would otherwise lead the next row.
        while start < cells.len() && cells[start].0 == ' ' {
            start += 1;
        }
    }
    out
}

/// Wrap every line to `width`, returning the rows and, for each input line,
/// the row it starts on.
pub fn wrap_lines(lines: Vec<Line<'static>>, width: usize) -> (Vec<Line<'static>>, Vec<usize>) {
    let mut rows = Vec::with_capacity(lines.len());
    let mut starts = Vec::with_capacity(lines.len());
    for line in lines {
        starts.push(rows.len());
        rows.extend(wrap_line(line, width));
    }
    (rows, starts)
}

fn cells_to_line(cells: &[(char, Style)]) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut cur: Option<Style> = None;
    for &(c, style) in cells {
        if cur.is_some_and(|s| s != style) {
            spans.push(Span::styled(
                std::mem::take(&mut buf),
                cur.unwrap_or_default(),
            ));
        }
        cur = Some(style);
        buf.push(c);
    }
    if !buf.is_empty() {
        spans.push(Span::styled(buf, cur.unwrap_or_default()));
    }
    Line::from(spans)
}
