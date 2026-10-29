//! Read-only provider and connector status, plus per-conversation model routing.

use super::*;
use serabot_desktop::{ConnectorStatus, ProviderStatus, SecretStatus, Selection};

impl Desktop {
    pub(crate) fn providers_screen(&mut self, ui: &mut egui::Ui) {
        design::header(
            ui,
            "Providers",
            "Configured models, routing classes and egress consent",
        );
        self.status_banner(ui);
        egui::ScrollArea::vertical()
            .id_salt("providers-screen")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                design::card(ui, |ui| {
                    ui.label(
                        RichText::new("Model routing")
                            .size(15.0)
                            .strong()
                            .color(design::text()),
                    );
                    ui.label(
                        RichText::new(
                            "The selection is stored per conversation. Applying a different provider requires explicit egress consent.",
                        )
                        .size(11.0)
                        .color(design::muted()),
                    );
                    ui.add_space(6.0);
                    self.model_block(ui);
                });
                ui.add_space(10.0);
                design::card(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("Add or change a provider")
                                .size(14.0)
                                .strong()
                                .color(design::text()),
                        );
                        design::chip(
                            ui,
                            "server-side",
                            design::muted(),
                            design::panel_alt(),
                        );
                    });
                    ui.label(design::meta(
                        "The desktop selects from the server catalog. Provider blocks, keys and egress approval stay in /etc/serabot/serabot.toml and the age store.",
                    ));
                    ui.label(
                        RichText::new("serabot secrets set <api_key_secret>")
                            .size(11.0)
                            .monospace()
                            .color(design::accent()),
                    );
                    ui.label(
                        RichText::new("sudo systemctl restart serabot")
                            .size(11.0)
                            .monospace()
                            .color(design::accent()),
                    );
                });
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Configured providers")
                            .size(14.0)
                            .strong()
                            .color(design::text()),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(
                                self.ready() && self.state.connected,
                                egui::Button::new(RichText::new("Refresh").size(11.0)),
                            )
                            .clicked()
                        {
                            self.start_request(Operation::Providers);
                        }
                    });
                });
                ui.add_space(6.0);
                if !self.state.providers_loaded {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(design::meta("Loading providers…"));
                    });
                } else if self.state.providers.is_empty() {
                    design::empty_state(
                        ui,
                        "No providers configured",
                        "Add providers in the server config and restart only serabot.service.",
                    );
                }
                for provider in &self.state.providers {
                    provider_card(ui, provider);
                    ui.add_space(8.0);
                }
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "Visibility only. Provider settings, keys and egress approval stay in the server config and the age-encrypted secret store.",
                    )
                    .size(10.0)
                    .color(design::muted_dim()),
                );
            });
    }

    pub(crate) fn connectors_screen(&mut self, ui: &mut egui::Ui) {
        design::header(
            ui,
            "Connectors",
            "Google, Microsoft, Notion and AgentMail status",
        );
        self.status_banner(ui);
        egui::ScrollArea::vertical()
            .id_salt("connectors-screen")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Connectors")
                            .size(14.0)
                            .strong()
                            .color(design::text()),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(
                                self.ready() && self.state.connected,
                                egui::Button::new(RichText::new("Refresh").size(11.0)),
                            )
                            .clicked()
                        {
                            self.start_request(Operation::Connectors);
                        }
                    });
                });
                ui.add_space(6.0);
                if !self.state.connectors_loaded {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(design::meta("Loading connectors…"));
                    });
                } else if self.state.connectors.is_empty() {
                    design::empty_state(
                        ui,
                        "No connectors configured",
                        "Add connector settings in the server config.",
                    );
                }
                for connector in &self.state.connectors {
                    connector_card(ui, connector);
                    ui.add_space(8.0);
                }
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "Secret presence only; values never leave the age store. OAuth login and writes stay on the server CLI, for example `serabot secrets set NAME`.",
                    )
                    .size(10.0)
                    .color(design::muted_dim()),
                );
            });
    }

    pub(crate) fn model_block(&mut self, ui: &mut egui::Ui) {
        let ready = self.ready()
            && self.active_conversation()
            && self.conversation_accepts_turns()
            && !self.state.unknown_outcome;
        let Some(catalog) = &self.state.catalog else {
            ui.label(design::meta("Connect to load the provider catalog."));
            return;
        };
        let before = self.state.draft.clone();
        ui.add_enabled_ui(ready, |ui| {
            let provider_label = if self.state.draft.is_auto() {
                "auto".to_string()
            } else {
                self.state.draft.provider.clone()
            };
            ui.horizontal(|ui| {
                ui.label(design::meta("Provider"));
                egui::ComboBox::from_id_salt("provider")
                    .width(160.0)
                    .selected_text(provider_label)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(self.state.draft.is_auto(), "auto")
                            .clicked()
                        {
                            self.state.draft = Selection::auto();
                        }
                        for provider in &catalog.providers {
                            let label = if provider.data_egress_approved {
                                provider.id.clone()
                            } else {
                                format!("{} · blocked", provider.id)
                            };
                            if ui
                                .add_enabled(
                                    provider.data_egress_approved,
                                    egui::SelectableLabel::new(
                                        !self.state.draft.is_auto()
                                            && self.state.draft.provider == provider.id,
                                        label,
                                    ),
                                )
                                .clicked()
                            {
                                self.state.draft.provider = provider.id.clone();
                                self.state.draft.model =
                                    provider.models.first().cloned().unwrap_or_default();
                            }
                        }
                    });
            });
            let model_label = if self.state.draft.is_auto() {
                catalog
                    .effective_selection
                    .as_ref()
                    .map(|selection| format!("{} / {}", selection.provider, selection.model))
                    .unwrap_or_else(|| "automatic".into())
            } else {
                format!("{} / {}", self.state.draft.provider, self.state.draft.model)
            };
            ui.horizontal(|ui| {
                ui.label(design::meta("Model"));
                egui::ComboBox::from_id_salt("model")
                    .width(200.0)
                    .selected_text(model_label)
                    .show_ui(ui, |ui| {
                        if self.state.draft.is_auto() {
                            ui.label(
                                RichText::new("Autoselect uses bot defaults and approved models.")
                                    .size(11.0)
                                    .color(design::muted()),
                            );
                            return;
                        }
                        let entries = if catalog.entries.is_empty() {
                            catalog
                                .providers
                                .iter()
                                .flat_map(|provider| {
                                    provider.models.iter().map(|model| {
                                        (
                                            provider.id.clone(),
                                            model.clone(),
                                            provider.data_egress_approved,
                                        )
                                    })
                                })
                                .collect::<Vec<_>>()
                        } else {
                            catalog
                                .entries
                                .iter()
                                .map(|entry| {
                                    (
                                        entry.provider.clone(),
                                        entry.model.clone(),
                                        entry.data_egress_approved,
                                    )
                                })
                                .collect()
                        };
                        for (provider, model, approved) in entries {
                            ui.add_enabled_ui(approved, |ui| {
                                if ui
                                    .selectable_label(
                                        self.state.draft.provider == provider
                                            && self.state.draft.model == model,
                                        format!("{provider} / {model}"),
                                    )
                                    .clicked()
                                {
                                    self.state.draft.provider = provider;
                                    self.state.draft.model = model;
                                }
                            });
                        }
                    });
            });
        });
        if before != self.state.draft {
            self.confirmed_egress = false;
        }
        if !catalog.allows(&catalog.selection) {
            ui.label(
                RichText::new("Saved profile unavailable or egress not approved.")
                    .size(11.0)
                    .color(design::warning()),
            );
        }
        if self.state.draft == catalog.selection {
            return;
        }
        let saved = if catalog.selection.is_auto() {
            catalog
                .effective_selection
                .as_ref()
                .map(|selection| format!("auto → {} / {}", selection.provider, selection.model))
                .unwrap_or_else(|| "auto".into())
        } else {
            format!(
                "{} / {}",
                catalog.selection.provider, catalog.selection.model
            )
        };
        ui.label(design::meta(format!("Saved {saved}")));
        ui.label(
            RichText::new("Applies to the next turn; a running turn keeps its pinned route.")
                .size(10.0)
                .color(design::muted_dim()),
        );
        ui.checkbox(
            &mut self.confirmed_egress,
            RichText::new("Provider will receive this conversation history").size(11.0),
        );
        let can_apply = ready && self.confirmed_egress && catalog.allows(&self.state.draft);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(can_apply, design::primary("Apply selection"))
                .clicked()
            {
                self.state.error = None;
                self.confirmed_egress = false;
                self.start_request(Operation::Apply(self.state.draft.clone()));
            }
            if ui
                .add_enabled(
                    ready,
                    egui::Button::new(RichText::new("Discard").size(12.0)),
                )
                .clicked()
            {
                if let Some(catalog) = &self.state.catalog {
                    self.state.draft = catalog.selection.clone();
                }
                self.confirmed_egress = false;
            }
        });
    }
}

fn provider_card(ui: &mut egui::Ui, provider: &ProviderStatus) {
    design::card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(&provider.id)
                    .size(14.0)
                    .strong()
                    .color(design::text()),
            );
            design::chip(ui, &provider.kind, design::muted(), design::panel_alt());
            if provider.data_egress_approved {
                design::chip(ui, "egress approved", design::accent(), design::ok_bg());
            } else {
                design::chip(
                    ui,
                    "egress blocked",
                    design::warning(),
                    design::warning_bg(),
                );
            }
            if provider.catalog_verified {
                design::chip(ui, "catalog verified", design::accent(), design::ok_bg());
            } else if provider.catalog_fetch {
                design::chip(ui, "catalog fetched", design::muted(), design::panel_alt());
            }
        });
        ui.add_space(4.0);
        ui.label(
            RichText::new(format!("Models · {}", join_models(&provider.models)))
                .size(11.0)
                .color(design::muted()),
        );
        if !provider.routing_classes.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label(design::meta("Routing classes"));
                for class in &provider.routing_classes {
                    design::chip(ui, class, design::muted(), design::panel_alt());
                }
            });
        }
        ui.add_space(4.0);
        secret_rows(ui, &provider.secrets);
    });
}

fn connector_card(ui: &mut egui::Ui, connector: &ConnectorStatus) {
    design::card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(&connector.id)
                    .size(14.0)
                    .strong()
                    .color(design::text()),
            );
            design::chip(ui, &connector.kind, design::muted(), design::panel_alt());
            if connector.enabled {
                design::chip(ui, "enabled", design::accent(), design::ok_bg());
            } else {
                design::chip(ui, "disabled", design::muted(), design::panel_alt());
            }
        });
        if let Some(uri) = &connector.redirect_uri {
            ui.horizontal(|ui| {
                ui.label(design::meta("Redirect"));
                ui.label(
                    RichText::new(uri)
                        .size(10.5)
                        .monospace()
                        .color(design::text()),
                );
                design::copy_button(ui, uri, "Copy");
            });
        }
        if let Some(base) = &connector.api_base {
            ui.label(design::meta(format!("API base · {base}")));
        }
        if let Some(inbox) = &connector.default_inbox_id {
            ui.label(design::meta(format!("Default inbox · {inbox}")));
        }
        ui.add_space(4.0);
        secret_rows(ui, &connector.secrets);
    });
}

fn secret_rows(ui: &mut egui::Ui, secrets: &std::collections::BTreeMap<String, SecretStatus>) {
    if secrets.is_empty() {
        ui.label(design::meta("No secret references configured."));
        return;
    }
    for (role, secret) in secrets {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::new(8.0, 8.0), Sense::hover());
            ui.painter().circle_filled(
                rect.center(),
                3.5,
                if secret.configured {
                    design::accent()
                } else {
                    design::warning()
                },
            );
            let name = secret.name.as_deref().unwrap_or("not configured");
            ui.label(RichText::new(format!("{role} · {name}")).size(10.5).color(
                if secret.configured {
                    design::text()
                } else {
                    design::muted()
                },
            ));
            if let Some(name) = secret.name.as_deref() {
                let command = format!("serabot secrets set {name}");
                design::copy_button(ui, &command, "Copy command");
            }
        });
    }
}

fn join_models(models: &[String]) -> String {
    if models.is_empty() {
        return "none".into();
    }
    let shown = models
        .iter()
        .take(6)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if models.len() > 6 {
        format!("{shown} +{}", models.len() - 6)
    } else {
        shown
    }
}
