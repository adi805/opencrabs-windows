//! Shared terminal breakpoints.
//!
//! Every screen that reflows has to agree on where the breaks are. If two
//! screens each invent their own numbers, the same terminal width produces
//! a two column grid on one and a three column grid on the other, and a
//! user resizing the window sees the app disagree with itself.
//!
//! So there is one table, here, and every responsive screen reads it.

/// Width at which a card grid gets three columns.
pub const GRID_THREE_COL_MIN: u16 = 90;

/// Width at which a card grid gets two columns.
pub const GRID_TWO_COL_MIN: u16 = 56;

/// Columns a card grid should use at this width: three when there is room
/// for them, two when there is some, one when there is not.
pub fn grid_cols(width: u16) -> usize {
    if width >= GRID_THREE_COL_MIN {
        3
    } else if width >= GRID_TWO_COL_MIN {
        2
    } else {
        1
    }
}
