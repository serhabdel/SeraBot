//! Full-screen first-run connection flow.
//!
//! The connect card is shown until an authenticated `/v1/health` read makes
//! [`crate::state::State::connected`] true. The token field is masked and the
//! token is only persisted after that first successful reply.

use crate::{ApiError, creds, design, valid_token, validate_endpoint};
use eframe::egui::{self, Frame, Margin, RichText, Stroke};
use secrecy::ExposeSecret;
use url::Url;
use zeroize::{Zeroize, Zeroizing};

/// Connect-card state. `persisted` flips after the first authenticated Health
/// reply has been written to settings + the credential store.
pub struct ConnectState {
    pub server: String,
    pub token: Zeroizing<String>,
    pub error: Option<String>,
    pub connecting: bool,
    pub persisted: bool,
}

impl ConnectState {
    /// Load the saved origin and token, if any. The token is loaded into the
    /// masked field so an auto-connect retry does not need a paste.
    pub fn load() -> Self {
        let settings = creds::load_settings();
        let mut state = Self::new(creds::prefill_server(&settings));
        state.token = creds::load_token()
            .map(|token| Zeroizing::new(token.expose_secret().to_owned()))
            .unwrap_or_default();
        state
    }

    pub fn new(server: String) -> Self {
        Self {
            server,
            token: Zeroizing::default(),
            error: None,
            connecting: false,
            persisted: false,
        }
    }

    pub fn endpoint(&self) -> Result<Url, ApiError> {
        validate_endpoint(self.server.trim())
    }

    /// Both fields are locally valid. The server is the final judge.
    pub fn ready(&self) -> bool {
        self.endpoint().is_ok() && valid_token(&self.token)
    }

    pub fn clear_token(&mut self) {
        self.token.zeroize();
    }
}

impl Drop for ConnectState {
    fn drop(&mut self) {
        self.token.zeroize();
    }
}

/// Draw the centered full-screen connect card.
///
/// Returns true when the owner pressed Connect or Enter. The caller owns
/// validation messages and the actual network dispatch.
pub fn connect_screen(
    ctx: &egui::Context,
    state: &mut ConnectState,
    theme: &mut design::ThemeMode,
    footer: &str,
) -> bool {
    let mut submitted = false;
    egui::CentralPanel::default()
        .frame(Frame::new().fill(design::bg()).inner_margin(Margin::symmetric(24, 24)))
        .show(ctx, |ui| {
            design::paint_backdrop(ui);
            let top = ((ui.available_height() - 470.0) * 0.5).max(16.0);
            ui.add_space(top);
            let card_width = (ui.available_width() - 80.0).clamp(280.0, 392.0);
            ui.vertical_centered(|ui| {
                Frame::new()
                    .fill(design::card_color())
                    .stroke(Stroke::new(1.0_f32, design::stroke_color()))
                    .corner_radius(14.0)
                    .inner_margin(Margin::symmetric(28, 24))
                    .show(ui, |ui| {
                        ui.set_width(card_width);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("SeraBot").size(22.0).strong().color(design::text()));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if state.connecting {
                                        ui.spinner();
                                    }
                                },
                            );
                        });
                        ui.label(
                            RichText::new("Owner console · connect to your host")
                                .size(12.0)
                                .color(design::muted()),
                        );
                        ui.add_space(16.0);

                        ui.add_enabled_ui(!state.connecting, |ui| {
                            ui.label(RichText::new("Server").size(11.0).color(design::muted()));
                            ui.add(
                                egui::TextEdit::singleline(&mut state.server)
                                    .desired_width(f32::INFINITY)
                                    .char_limit(2048)
                                    .hint_text("https://bot.example  ·  http://127.0.0.1:8787"),
                            );
                            ui.label(
                                RichText::new(
                                    "HTTPS origin, or exact loopback HTTP for an SSH tunnel.",
                                )
                                .size(11.0)
                                .color(design::muted()),
                            );
                            ui.add_space(8.0);
                            ui.label(RichText::new("Token").size(11.0).color(design::muted()));
                            ui.add(
                                egui::TextEdit::singleline(&mut *state.token)
                                    .password(true)
                                    .desired_width(f32::INFINITY)
                                    .char_limit(512)
                                    .hint_text("token minted on the server"),
                            );
                        });

                        if let Some(error) = &state.error {
                            ui.add_space(8.0);
                            Frame::new()
                                .fill(design::danger_bg())
                                .stroke(Stroke::new(1.0_f32, design::danger()))
                                .corner_radius(10.0)
                                .inner_margin(Margin::symmetric(10, 8))
                                .show(ui, |ui| {
                                    ui.label(RichText::new(error).size(12.0).color(design::danger()));
                                });
                        }

                        ui.add_space(16.0);
                        let label = if state.connecting {
                            "Connecting…"
                        } else {
                            "Connect"
                        };
                        if ui
                            .add_enabled(
                                !state.connecting,
                                egui::Button::new(
                                    RichText::new(label).size(14.0).strong().color(design::accent()),
                                ),
                            )
                            .clicked()
                        {
                            submitted = true;
                        }
                        let enter = ui.ctx().input(|input| {
                            input.key_pressed(egui::Key::Enter) && !input.modifiers.any()
                        });
                        if enter && !state.connecting {
                            submitted = true;
                        }

                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Appearance").size(11.0).color(design::muted()));
                            for mode in [design::ThemeMode::Dark, design::ThemeMode::Light] {
                                if ui
                                    .selectable_label(*theme == mode, mode.label())
                                    .clicked()
                                {
                                    *theme = mode;
                                    design::apply_theme(ui.ctx(), mode);
                                    let _ = creds::save_settings(&creds::Settings {
                                        server: state.server.trim().to_owned(),
                                        theme: Some(mode.as_str().into()),
                                    });
                                }
                            }
                        });

                        ui.add_space(16.0);
                        ui.separator();
                        ui.add_space(12.0);
                        ui.label(
                            RichText::new("Mint a token on the server")
                                .size(11.0)
                                .strong()
                                .color(design::text()),
                        );
                        ui.label(
                            RichText::new("serabot token mint --label desktop-win")
                                .size(11.0)
                                .monospace()
                                .color(design::accent()),
                        );
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(
                                "The token is kept in Windows Credential Manager; only the server URL is written to settings.json. No token is embedded in this build.",
                            )
                            .size(11.0)
                            .color(design::muted()),
                        );
                    });
                ui.add_space(16.0);
                ui.label(RichText::new(footer).size(11.0).color(design::muted()));
                ui.add_space(8.0);
            });
        });
    submitted
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Rect;

    fn state() -> ConnectState {
        ConnectState {
            server: "https://owner.example".into(),
            token: Zeroizing::new("synthetic-desktop-connect-token-0000".into()),
            error: None,
            connecting: false,
            persisted: false,
        }
    }

    #[test]
    fn connect_state_validates_origin_and_token() {
        let mut state = state();
        assert!(state.ready());
        state.server = "http://owner.example".into();
        assert!(!state.ready());
        state.server = "http://127.0.0.1:8787".into();
        assert!(state.ready());
        state.token = Zeroizing::new("short".into());
        assert!(!state.ready());
        state.clear_token();
        assert!(state.token.is_empty());
    }

    #[test]
    fn connect_screen_renders_with_error_and_connecting_states() {
        for (connecting, error) in [(false, None), (false, Some("Rejected.")), (true, None)] {
            let mut state = state();
            state.connecting = connecting;
            state.error = error.map(str::to_owned);
            let ctx = egui::Context::default();
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(860.0, 560.0),
                )),
                ..Default::default()
            };
            let output = ctx.run(input, |ctx| {
                let mut theme = design::ThemeMode::Dark;
                connect_screen(
                    ctx,
                    &mut state,
                    &mut theme,
                    &format!("v{} · developer build", env!("CARGO_PKG_VERSION")),
                );
            });
            assert!(!output.shapes.is_empty());
            assert!(!state.token.is_empty());
        }
    }
}
