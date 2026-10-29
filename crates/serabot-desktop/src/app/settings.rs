//! Settings: connection, credential storage, updates, live stream and shortcuts.

use super::*;

fn format_size(bytes: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    if bytes as f64 >= MIB {
        format!("{:.1} MB", bytes as f64 / MIB)
    } else {
        format!("{} KB", bytes.div_ceil(1024))
    }
}

impl Desktop {
    pub(crate) fn update_banner(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if self.update.dismissed {
            return;
        }
        let Some(info) = self.update.info.clone() else {
            return;
        };
        Frame::new()
            .fill(design::OK_BG)
            .stroke(Stroke::new(1.0_f32, design::ACCENT_DIM))
            .corner_radius(12.0)
            .inner_margin(Margin::symmetric(14, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("Update v{} available", info.version))
                            .size(13.0)
                            .strong()
                            .color(design::ACCENT),
                    );
                    ui.label(design::meta(format_size(info.size)));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(
                                !self.update.checking,
                                egui::Button::new(RichText::new("Check now").size(11.0)),
                            )
                            .clicked()
                        {
                            self.check_updates();
                        }
                        if ui
                            .add(egui::Button::new(RichText::new("Later").size(11.0)))
                            .clicked()
                        {
                            self.update.dismissed = true;
                        }
                        if ui.add(design::primary("Install and restart")).clicked() {
                            match updater::spawn_apply(&info) {
                                Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                                Err(error) => self.update.error = Some(error),
                            }
                        }
                    });
                });
                ui.add_space(2.0);
                egui::CollapsingHeader::new(
                    RichText::new("Release notes")
                        .size(10.0)
                        .color(design::MUTED),
                )
                .id_salt("update-notes")
                .show(ui, |ui| {
                    ui.label(RichText::new(&info.notes).size(12.0).color(design::TEXT));
                });
            });
        ui.add_space(8.0);
    }

    pub(crate) fn settings_screen(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        design::header(ui, "Settings", "Connection, updates and shortcuts");
        self.status_banner(ui);
        egui::ScrollArea::vertical()
            .id_salt("settings-screen")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.connection_card(ui, ctx);
                ui.add_space(10.0);
                self.updates_card(ui, ctx);
                ui.add_space(10.0);
                self.transport_card(ui);
                ui.add_space(10.0);
                self.shortcuts_card(ui);
            });
    }

    fn connection_card(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let origin = self
            .credentials
            .as_ref()
            .map(|credentials| credentials.endpoint.origin().ascii_serialization())
            .unwrap_or_else(|| self.connect.server.clone());
        let persisted = self.connect.persisted;
        design::card(ui, |ui| {
            ui.label(
                RichText::new("Connection")
                    .size(15.0)
                    .strong()
                    .color(design::TEXT),
            );
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.label(design::meta("Server"));
                ui.label(
                    RichText::new(&origin)
                        .size(12.0)
                        .monospace()
                        .color(design::TEXT),
                );
                design::copy_button(ui, &origin, "Copy");
            });
            if !self.state.version.is_empty() {
                ui.label(design::meta(format!(
                    "Server build {} · API token gated",
                    self.state.version
                )));
            }
            ui.add_space(4.0);
            ui.label(
                RichText::new(if cfg!(windows) {
                    "The token is stored in Windows Credential Manager. Only the server origin is written to %APPDATA%\\SeraBot\\settings.json."
                } else {
                    "This build keeps the token in memory. The server origin is written to the settings file."
                })
                .size(11.0)
                .color(design::MUTED),
            );
            if !persisted {
                ui.label(
                    RichText::new(
                        "This session is not persisted yet (waiting for a saved credential).",
                    )
                    .size(10.0)
                    .color(design::WARNING),
                );
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("Forget token").clicked() {
                    self.forget_token(ctx);
                }
                if ui.add(design::danger("Disconnect")).clicked() {
                    self.disconnect(ctx);
                }
                ui.label(
                    RichText::new("Revoking a device token on the server returns this app to the connect screen.")
                        .size(10.0)
                        .color(design::MUTED_DIM),
                );
            });
        });
    }

    fn updates_card(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let info = self.update.info.clone();
        let error = self.update.error.clone();
        let configured = updater::configured_repo();
        let version = env!("CARGO_PKG_VERSION");
        let sha = updater::build_sha();
        design::card(ui, |ui| {
            ui.label(
                RichText::new("Updates")
                    .size(15.0)
                    .strong()
                    .color(design::TEXT),
            );
            ui.label(design::meta(format!("Desktop v{version}")));
            if let Some(sha) = sha {
                ui.label(design::meta(format!("Build {sha}")));
            }
            match configured {
                Some(repo) => {
                    ui.label(design::meta(format!("Release channel · github.com/{repo}")));
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                !self.update.checking,
                                egui::Button::new(
                                    RichText::new(if self.update.checking {
                                        "Checking…"
                                    } else {
                                        "Check for updates"
                                    })
                                    .size(11.0),
                                ),
                            )
                            .clicked()
                        {
                            self.check_updates();
                        }
                        if let Some(info) = &info {
                            ui.label(
                                RichText::new(format!(
                                    "v{} ready · {}",
                                    info.version,
                                    format_size(info.size)
                                ))
                                .size(11.0)
                                .color(design::ACCENT),
                            );
                            if ui.add(design::primary("Install and restart")).clicked() {
                                match updater::spawn_apply(info) {
                                    Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                                    Err(error) => self.update.error = Some(error),
                                }
                            }
                        } else if !self.update.checking {
                            ui.label(design::meta("Up to date"));
                        }
                    });
                    if let Some(info) = &info {
                        egui::CollapsingHeader::new(
                            RichText::new("Release notes")
                                .size(10.0)
                                .color(design::MUTED),
                        )
                        .id_salt("settings-update-notes")
                        .show(ui, |ui| {
                            ui.label(RichText::new(&info.notes).size(12.0).color(design::TEXT));
                        });
                    }
                }
                None => {
                    ui.label(
                        RichText::new(
                            "Developer build · updates are disabled. Release builds set SERABOT_RELEASES_REPO.",
                        )
                        .size(11.0)
                        .color(design::MUTED),
                    );
                }
            }
            if let Some(error) = &error {
                ui.label(RichText::new(error).size(11.0).color(design::DANGER));
            }
            ui.label(
                RichText::new(
                    "Downloads are SHA-256 verified before installation. No silent installs.",
                )
                .size(10.0)
                .color(design::MUTED_DIM),
            );
        });
    }

    fn transport_card(&self, ui: &mut egui::Ui) {
        design::card(ui, |ui| {
            ui.label(
                RichText::new("Live updates")
                    .size(15.0)
                    .strong()
                    .color(design::TEXT),
            );
            ui.horizontal(|ui| {
                design::status_dot(
                    ui,
                    if self.events_active {
                        design::ACCENT
                    } else {
                        design::WARNING
                    },
                );
                ui.label(design::meta(if self.events_active {
                    "Server-Sent Events stream connected"
                } else {
                    "Reconnecting · polling fallback active"
                }));
            });
            if self.last_event_id > 0 {
                ui.label(design::meta(format!(
                    "Resume cursor · {}",
                    self.last_event_id
                )));
            }
            ui.label(
                RichText::new(
                    "The stream reconnects with backoff and Last-Event-ID. Task polling still runs as a stuck-stream guard.",
                )
                .size(10.0)
                .color(design::MUTED_DIM),
            );
        });
    }

    fn shortcuts_card(&self, ui: &mut egui::Ui) {
        design::card(ui, |ui| {
            ui.label(
                RichText::new("Keyboard")
                    .size(15.0)
                    .strong()
                    .color(design::TEXT),
            );
            for (keys, action) in [
                ("Ctrl+K", "Command palette"),
                ("Ctrl+1…7", "Jump to a section"),
                ("Ctrl+R", "Refresh reads"),
                ("Ctrl+Enter", "Send message"),
                ("Esc", "Close the palette or model window"),
            ] {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(keys)
                            .size(11.0)
                            .monospace()
                            .color(design::ACCENT),
                    );
                    ui.label(design::meta(action));
                });
            }
            ui.add_space(4.0);
            ui.label(
                RichText::new(
                    "Tokens are per device. Mint with `serabot token mint --label desktop-win` and revoke with `serabot token revoke <id>` on the server.",
                )
                .size(10.0)
                .color(design::MUTED_DIM),
            );
        });
    }
}
