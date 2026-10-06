//! Ctrl+C expanded per-dialog command panel (#1775).
//!
//! Contract:
//! - Ctrl+C with a dialog open opens that dialog's expanded command
//!   panel (modal, scoped keymap) instead of starting the quit flow.
//! - The panel renders one `key verb` row per binding plus the dismiss
//!   hint.
//! - Esc/q/Ctrl+C dismiss it; other keys are consumed.
//! - With NO dialog open, the global Ctrl+C path is byte-for-byte the
//!   old behavior: first press clears input and shows the quit hint.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};

use crate::brain::agent::service::AgentService;
use crate::db::Database;
use crate::services::ServiceContext;
use crate::tests::agent_service_mocks::MockProvider;
use crate::tui::app::App;
use crate::tui::app::dialog_keys::DialogScope;
use crate::tui::app::events::AppMode;
use crate::tui::app::events::TuiEvent;
use crate::tui::render::render;

async fn app() -> App {
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
    app.mode = AppMode::Help;
    app
}

fn ctrl_c() -> TuiEvent {
    TuiEvent::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
}

fn esc() -> TuiEvent {
    TuiEvent::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
}

#[tokio::test]
async fn ctrl_c_in_dialog_opens_scoped_panel() {
    let mut app = app().await; // Help mode
    app.handle_event(ctrl_c()).await.unwrap();

    assert!(app.dialog_help_open, "panel must open");
    assert_eq!(app.dialog_help_scope, DialogScope::Help);
    assert!(
        app.error_message.is_none(),
        "no quit hint — the dialog path must not leak into the global flow"
    );
}

#[tokio::test]
async fn panel_renders_keymap_and_dismiss_hint() {
    let mut app = app().await;
    app.handle_event(ctrl_c()).await.unwrap();

    let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
    terminal.draw(|f| render(f, &mut app)).unwrap();
    let buf = terminal.backend().buffer().clone();
    let text: String = (0..buf.area.width * buf.area.height)
        .map(|i| buf.content()[i as usize].symbol().to_string())
        .collect();

    assert!(
        text.contains("Help commands"),
        "title missing: panel not drawn or wrong scope"
    );
    assert!(text.contains("Esc/Ctrl+C"), "dismiss hint missing");
    // A real binding from the Help table, one key per row.
    assert!(text.contains("search"), "scoped keymap rows missing");
}

#[tokio::test]
async fn esc_q_and_ctrl_c_dismiss_the_panel() {
    for dismiss in [
        esc(),
        TuiEvent::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
        ctrl_c(),
    ] {
        let mut app = app().await;
        app.handle_event(ctrl_c()).await.unwrap();
        assert!(app.dialog_help_open);
        app.handle_event(dismiss).await.unwrap();
        assert!(!app.dialog_help_open, "panel must close");
        // And the dialog itself must still be there, untouched.
        assert_eq!(app.mode, AppMode::Help);
    }
}

#[tokio::test]
async fn other_keys_are_consumed_while_panel_open() {
    let mut app = app().await;
    app.handle_event(ctrl_c()).await.unwrap();

    // '/' would arm the help search filter if it reached the dialog.
    app.handle_event(TuiEvent::Key(KeyEvent::new(
        KeyCode::Char('/'),
        KeyModifiers::NONE,
    )))
    .await
    .unwrap();
    assert!(
        !app.help_search_active && app.help_search.is_empty(),
        "keys must not reach the dialog beneath the panel"
    );
    assert!(app.dialog_help_open, "panel stays open on unrelated keys");
}

#[tokio::test]
async fn ctrl_c_without_dialog_keeps_global_quit_flow() {
    let mut app = app().await;
    app.mode = AppMode::Chat; // no dialog scope
    app.handle_event(ctrl_c()).await.unwrap();

    assert!(!app.dialog_help_open, "no panel without a dialog");
    assert!(
        app.error_message.as_deref() == Some("Press Ctrl+C again to quit"),
        "first Ctrl+C must keep showing the quit hint, got {:?}",
        app.error_message
    );
    assert!(
        app.ctrl_c_pending_at.is_some(),
        "quit confirmation must be armed"
    );
}

#[tokio::test]
async fn panel_self_heals_when_dialog_closes_underneath() {
    let mut app = app().await;
    app.handle_event(ctrl_c()).await.unwrap();
    assert!(app.dialog_help_open);

    // The dialog closes on its own (e.g. approval resolved externally).
    app.mode = AppMode::Chat;
    let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
    terminal.draw(|f| render(f, &mut app)).unwrap();

    assert!(
        !app.dialog_help_open,
        "stale panel must close at render time when its scope went away"
    );
}
