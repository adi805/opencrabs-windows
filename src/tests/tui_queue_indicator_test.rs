//! #2237: the input box has to say that Enter queues while a turn runs, and
//! the queue row has to say what the queue does and which key edits it.
//!
//! Two layers. The pure helpers decide the wording and the truncation, so the
//! maths is checked at any terminal width without a frame. The rendered test
//! then proves the wording actually reaches the input box: the helpers being
//! right means nothing if the title is never attached to the block.

use std::sync::Arc;

use ratatui::{Terminal, backend::TestBackend};

use crate::brain::agent::QueuedUserMessage;
use crate::brain::agent::service::AgentService;
use crate::db::Database;
use crate::db::models::Session;
use crate::services::ServiceContext;
use crate::tests::agent_service_mocks::MockProvider;
use crate::tui::app::App;
use crate::tui::render::render;
use crate::tui::render::{queue_line_parts, turn_in_flight_hint};
use unicode_width::UnicodeWidthStr;

const WIDTH: u16 = 80;
const HEIGHT: u16 = 24;

async fn app_with_session() -> (App, uuid::Uuid) {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let context = ServiceContext::new(db.pool().clone());
    let provider: Arc<dyn crate::brain::provider::Provider> = Arc::new(MockProvider);
    let service = Arc::new(AgentService::new_for_test(provider, context.clone()).await);
    #[cfg(feature = "whatsapp")]
    let mut app = App::new(
        service,
        context,
        Arc::new(crate::channels::whatsapp::WhatsAppState::new()),
    );
    #[cfg(not(feature = "whatsapp"))]
    let mut app = App::new(service, context);

    let session = Session::new(Some("queue indicator".to_string()), None, None);
    let sid = session.id;
    app.current_session = Some(session);
    (app, sid)
}

fn queue(app: &App, sid: uuid::Uuid, texts: &[&str]) {
    let mut map = app.queued_messages.lock().unwrap();
    map.insert(
        sid,
        texts
            .iter()
            .map(|t| QueuedUserMessage::plain((*t).to_string()))
            .collect(),
    );
}

fn row_text(buffer: &ratatui::buffer::Buffer, y: u16) -> String {
    (0..buffer.area.width)
        .map(|x| buffer[(x, y)].symbol().to_string())
        .collect()
}

fn input_rows(buffer: &ratatui::buffer::Buffer, app: &App) -> String {
    (app.input_area_y..app.input_area_y + app.input_area_height)
        .map(|y| row_text(buffer, y))
        .collect::<Vec<_>>()
        .join("\n")
}

// --- the queue row wording -------------------------------------------------

#[test]
fn a_wide_row_says_what_the_queue_does_and_which_key_edits_it() {
    let parts = queue_line_parts(2, "hello bre", 118);
    assert_eq!(parts.label, "⏳ queued (2): ");
    assert_eq!(parts.preview, "hello bre");
    assert!(
        parts.hint.contains("next tool call"),
        "the row has to say when the queued text goes out: {:?}",
        parts.hint
    );
    assert!(
        parts.hint.contains("back to this box"),
        "and that it is not lost if no tool call comes: {:?}",
        parts.hint
    );
    assert!(
        parts.hint.contains("↑ edit"),
        "the key has to be named: {:?}",
        parts.hint
    );
}

#[test]
fn a_normal_terminal_keeps_the_timing_and_drops_the_fallback_sentence() {
    // An 80 column terminal has no room for the whole explanation beside the
    // count and a readable preview. The timing and the key survive; the "or
    // back to this box" clause is the first thing to go.
    let parts = queue_line_parts(2, "hello bre", 78);
    assert!(
        parts.hint.contains("next tool call"),
        "when it goes out is the point of the row: {:?}",
        parts.hint
    );
    assert!(
        !parts.hint.contains("back to this box"),
        "the fallback sentence yields first: {:?}",
        parts.hint
    );
    assert!(parts.hint.contains("↑ edit"));
}

#[test]
fn a_narrow_row_keeps_the_count_and_the_key_before_anything_else() {
    let parts = queue_line_parts(1, "ping bre", 40);
    assert_eq!(parts.label, "⏳ queued: ");
    assert_eq!(parts.preview, "ping bre");
    assert_eq!(
        parts.hint, " · ↑ edit",
        "when there is no room for the explanation the key survives, not the other way round"
    );
}

#[test]
fn the_row_never_exceeds_the_space_it_is_given() {
    let long = "x".repeat(400);
    for width in [24usize, 40, 60, 120] {
        let parts = queue_line_parts(3, &long, width);
        let used = UnicodeWidthStr::width(parts.label.as_str())
            + UnicodeWidthStr::width(parts.preview.as_str())
            + UnicodeWidthStr::width(parts.hint);
        assert!(
            used <= width,
            "row of {used} columns does not fit in {width}: {:?}",
            (parts.label.clone(), parts.preview.clone(), parts.hint)
        );
        assert!(
            parts.preview.ends_with("...") || parts.preview.is_empty(),
            "an over-long preview is truncated or dropped, never left whole: {:?}",
            parts.preview
        );
    }
}

#[test]
fn a_queue_with_no_room_for_a_preview_still_shows_the_count() {
    let parts = queue_line_parts(12, "ping bre", 20);
    assert_eq!(parts.label, "⏳ queued (12)");
    assert!(parts.preview.is_empty());
    assert!(parts.hint.contains("↑ edit"));
}

#[test]
fn a_multiline_queued_message_is_flattened_to_one_row() {
    let parts = queue_line_parts(1, "hello bre\nping bre", 78);
    assert_eq!(parts.preview, "hello bre ping bre");
}

// --- the in-flight hint on the input border --------------------------------

#[test]
fn the_hint_is_dropped_rather_than_overwriting_the_context_title() {
    assert!(
        turn_in_flight_hint(80, 13).is_some_and(|h| h.contains("Enter queues")),
        "a normal terminal gets the full sentence"
    );
    assert_eq!(
        turn_in_flight_hint(40, 13),
        Some(" ⏳ Enter queues "),
        "a narrow terminal gets the short form"
    );
    assert_eq!(
        turn_in_flight_hint(25, 13),
        None,
        "too narrow for both titles: the hint yields, it does not draw over the context budget"
    );
}

// --- rendered frames -------------------------------------------------------

#[tokio::test]
async fn an_idle_input_box_does_not_claim_enter_queues() {
    let (mut app, _sid) = app_with_session().await;
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();
    terminal.draw(|f| render(f, &mut app)).unwrap();
    let rows = input_rows(terminal.backend().buffer(), &app);
    assert!(
        !rows.contains("Enter queues"),
        "the hint belongs to a turn in flight only:\n{rows}"
    );
}

#[tokio::test]
async fn a_turn_in_flight_says_enter_queues_on_the_input_box() {
    let (mut app, _sid) = app_with_session().await;
    app.is_processing = true;
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();
    terminal.draw(|f| render(f, &mut app)).unwrap();
    let rows = input_rows(terminal.backend().buffer(), &app);
    assert!(
        rows.contains("Enter queues"),
        "the box looks identical to the idle state without this:\n{rows}"
    );
}

#[tokio::test]
async fn a_queued_message_names_the_count_and_the_key_in_the_frame() {
    let (mut app, sid) = app_with_session().await;
    app.is_processing = true;
    queue(&app, sid, &["hello bre", "ping bre"]);
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();
    terminal.draw(|f| render(f, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    let frame: String = (0..HEIGHT)
        .map(|y| row_text(buffer, y))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        frame.contains("queued (2)"),
        "the count has to be on screen:\n{frame}"
    );
    assert!(
        frame.contains("next tool call"),
        "the row has to say when it goes out:\n{frame}"
    );
    assert!(
        frame.contains("↑ edit"),
        "the key that pulls it back for editing has to be named:\n{frame}"
    );
}
