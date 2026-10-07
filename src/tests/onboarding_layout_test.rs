//! Full-screen onboarding layout (#1973): the wizard is a screen, not a
//! centered dialog, and the provider step's key and model fields stay on
//! screen on small terminals.

use crate::tui::onboarding::{AuthField, OnboardingStep, OnboardingWizard, PROVIDERS};
use crate::tui::onboarding_layout::{
    MAX_CONTENT_WIDTH, content_width, scroll_offset, wrap_line, wrap_lines,
};
use crate::tui::onboarding_render::render_onboarding;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

fn screen_text(wizard: &OnboardingWizard, w: u16, h: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| render_onboarding(f, wizard)).unwrap();
    let buf = terminal.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..h {
        for x in 0..w {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

/// OpenAI on the provider step with the API key field focused and a long
/// model catalogue, the shape that pushed the key off a small screen.
fn provider_step_wizard() -> OnboardingWizard {
    let mut w = OnboardingWizard::new();
    w.step = OnboardingStep::ProviderAuth;
    w.ps.selected_provider = PROVIDERS
        .iter()
        .position(|p| p.name == "OpenAI")
        .expect("OpenAI provider");
    w.ps.api_key_input = "sk-test".to_string();
    w.ps.models.clear();
    w.ps.config_models = (0..40).map(|i| format!("gpt-model-{i}")).collect();
    w.auth_field = AuthField::ApiKey;
    w.error_message = None;
    w.resume_notice = None;
    w
}

#[test]
fn provider_step_key_and_model_fit_small_terminals() {
    let wizard = provider_step_wizard();
    for (w, h) in [(80, 24), (60, 20), (200, 60)] {
        let text = screen_text(&wizard, w, h);
        assert!(
            text.contains("API Key:"),
            "{w}x{h}: key field off screen\n{text}"
        );
        assert!(
            text.contains("Model:"),
            "{w}x{h}: model field off screen\n{text}"
        );
    }
}

#[test]
fn wizard_is_a_screen_not_a_boxed_dialog() {
    let wizard = provider_step_wizard();
    let text = screen_text(&wizard, 120, 40);
    // The old dialog drew a four-cornered box; the screen has none.
    for corner in ['┌', '┐', '└', '┘', '╭', '╮', '╰', '╯'] {
        assert!(!text.contains(corner), "found box corner {corner}\n{text}");
    }
    assert!(text.contains("OpenCrabs Setup"));
}

#[test]
fn error_stays_visible_in_the_footer() {
    let mut wizard = provider_step_wizard();
    wizard.error_message = Some("API key is required".to_string());
    let text = screen_text(&wizard, 60, 16);
    assert!(text.contains("API key is required"), "{text}");
}

#[test]
fn first_run_step_one_footer_says_quit() {
    let mut wizard = OnboardingWizard::new();
    wizard.is_first_time = true;
    wizard.resume_notice = None;
    let text = screen_text(&wizard, 100, 30);
    assert!(text.contains("[Esc] Quit"), "{text}");
}

#[test]
fn content_width_caps_and_keeps_a_gutter() {
    assert_eq!(content_width(80), 76);
    assert_eq!(content_width(300), MAX_CONTENT_WIDTH);
    assert_eq!(content_width(10), 10);
}

#[test]
fn wrap_line_respects_width_and_keeps_text_and_style() {
    let red = Style::default().fg(Color::Red);
    let line = Line::from(vec![
        Span::styled("  Label: ", red),
        Span::raw("alpha beta gamma delta epsilon zeta eta theta"),
    ]);
    let rows = wrap_line(line, 16);
    assert!(rows.len() > 1);
    for r in &rows {
        let w: usize = r.spans.iter().map(|s| s.content.width()).sum();
        assert!(w <= 16, "row too wide: {w}");
    }
    assert_eq!(rows[0].spans[0].style, red);
    let joined: String = rows
        .iter()
        .flat_map(|r| r.spans.iter().map(|s| s.content.to_string()))
        .collect::<Vec<_>>()
        .join(" ");
    for word in ["alpha", "epsilon", "theta", "Label:"] {
        assert!(joined.contains(word), "lost {word}: {joined}");
    }
}

#[test]
fn wrap_lines_maps_each_line_to_its_first_row() {
    let lines = vec![
        Line::from("short"),
        Line::from("this one is long enough to wrap twice over"),
        Line::from("last"),
    ];
    let (rows, starts) = wrap_lines(lines, 12);
    assert_eq!(starts[0], 0);
    assert_eq!(starts[1], 1);
    assert!(starts[2] > 2);
    assert_eq!(rows.len(), starts[2] + 1);
}

#[test]
fn scroll_offset_follows_focus_and_clamps() {
    // Everything fits: never scroll.
    assert_eq!(scroll_offset(10, 12, 20, 0), 0);
    // Focus below the fold: scroll to it with two rows of context.
    assert_eq!(scroll_offset(30, 50, 20, 0), 28);
    // Never past the end.
    assert_eq!(scroll_offset(49, 50, 20, 0), 30);
    // Page Down adds on top, still clamped.
    assert_eq!(scroll_offset(0, 50, 20, 100), 30);
}
