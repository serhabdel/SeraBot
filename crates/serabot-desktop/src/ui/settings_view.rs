use super::*;

#[component]
pub(super) fn SettingsView(session: Signal<Session>) -> Element {
    let signed_in_user = session.read().state.user.clone();
    let signing_out = session.read().sign_out.in_flight;
    let server = session.read().server.clone();
    let version = session.read().state.version.clone();
    let update = session.read().update.info.clone();
    let update_error = session.read().update.error.clone();
    let update_notice = session.read().update.notice.clone();
    let checking = session.read().update.checking;
    let audio_enabled = session.read().state.audio_enabled;
    let transcription = session
        .read()
        .state
        .audio_transcription_model
        .clone()
        .unwrap_or_default();
    let understanding = session
        .read()
        .state
        .audio_understanding_model
        .clone()
        .unwrap_or_default();
    let firecrawl_enabled = session.read().state.firecrawl_enabled;
    let telegram_enabled = session.read().state.telegram_enabled;
    let catalog_fetch = session.read().state.catalog_fetch;
    let audio_on = session.read().audio.enabled;
    let audio_transcription = session.read().audio.transcription_model.clone();
    let audio_understanding = session.read().audio.understanding_model.clone();
    let can_admin = session.read().can_admin();
    // Voice/OCR surface. Read-only status comes from the server; the editable
    // fields are the draft the Save button writes.
    let voice_loaded = session.read().voice.loaded;
    let voice_loading = session.read().voice.loading;
    let voice_stt = session.read().voice.stt_model.clone();
    let voice_stt_ready = session.read().voice.stt_ready;
    let voice_stt_detail = session.read().voice.stt_detail.clone();
    let voice_tts = session.read().voice.tts_model.clone();
    let voice_tts_ready = session.read().voice.tts_ready;
    let voice_tts_detail = session.read().voice.tts_detail.clone();
    let voice_id = session.read().voice.voice_id.clone();
    let voice_ocr = session.read().voice.ocr_model.clone();
    let voice_ocr_ready = session.read().voice.ocr_ready;
    let voice_ocr_detail = session.read().voice.ocr_detail.clone();
    let voice_ocr_enabled = session.read().voice.ocr_enabled;
    let voice_reply_mode = session.read().voice.reply_mode.clone();
    let voice_max_chars = session.read().voice.max_speech_chars;
    // The live sandbox posture. Every value the server does not report stays
    // "unknown"; nothing is guessed.
    let status = session.read().status.status.clone();
    let status_error = session.read().status.error.clone();
    let status_loading = session.read().status.loading;
    let restart_pending = session.read().restart_pending;
    let telegram_status = session.read().telegram.status.clone();
    let telegram_loaded = session.read().telegram.loaded;
    let telegram_loading = session.read().telegram.loading;
    let telegram_error = session.read().telegram.error.clone();
    let telegram_link = session.read().telegram.link.clone();
    let telegram_linking = session.read().telegram.linking;
    let telegram_unlinking = session.read().telegram.unlinking;
    let telegram_confirm = session.read().telegram.confirm_unlink;
    let policy_version = status
        .as_ref()
        .and_then(|status| status.policy_version.clone())
        .unwrap_or_else(|| "unknown".into());
    let pending_tasks = match status.as_ref().and_then(|status| status.pending_tasks) {
        Some(count) if count >= 0 => count.to_string(),
        _ => "unknown".to_string(),
    };
    let pending_approvals = match status.as_ref().and_then(|status| status.pending_approvals) {
        Some(count) if count >= 0 => count.to_string(),
        _ => "unknown".to_string(),
    };
    let pending_tools = status
        .as_ref()
        .map(|status| {
            status
                .approvals
                .iter()
                .map(|approval| approval.tool.clone())
                .filter(|tool| !tool.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let pending_tools_empty = pending_tools.is_empty();
    let (bots_loaded, gated_tools) = {
        let state = session.read();
        let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for bot in &state.state.listed_bots {
            for tool in &bot.require_approval {
                map.entry(tool.clone()).or_default().push(bot.id.clone());
            }
        }
        let gated = map
            .into_iter()
            .map(|(tool, bots)| {
                let label = format!("{} · {}", humanize_label(&tool), bots.join(", "));
                (tool, label)
            })
            .collect::<Vec<_>>();
        (state.state.bots_loaded, gated)
    };
    let (providers_loaded, egress) = {
        let state = session.read();
        (
            state.state.providers_loaded,
            state
                .state
                .providers
                .iter()
                .map(|provider| {
                    (
                        provider.id.clone(),
                        if provider.data_egress_approved {
                            "allowed".to_string()
                        } else {
                            "not allowed".to_string()
                        },
                        provider.data_egress_approved,
                    )
                })
                .collect::<Vec<_>>(),
        )
    };
    let gated_empty = gated_tools.is_empty();
    let egress_empty = egress.is_empty();
    let sel = session.read().settings_section;
    let settings_new_cards = rsx! {
        if sel == SettingsSection::Appearance {
            AppearanceCard { session }
        }
        if sel == SettingsSection::Notifications {
            NotificationsCard { session }
        }
        if sel == SettingsSection::Chat {
            ChatPrefsCard { session }
            TurnLimitsCard { session }
        }
        if sel == SettingsSection::WebSearch {
            WebSearchCard { session }
        }
        if sel == SettingsSection::Memory {
            Mem0RecallCard { session }
        }
        if sel == SettingsSection::LocalFolders {
            LocalFoldersCard { session }
        }
        if sel == SettingsSection::About {
            AboutCard { session }
        }
    };
    rsx! {
        div { class: "pane",
            div { class: "pane-head",
                div {
                    h1 { class: "pane-title", "Settings" }
                    p { class: "muted", "Your account, how SeraBot looks and behaves, and what is connected." }
                }
            }
            div { class: "pane-body settings-layout",
                SettingsNav { session }
                div { class: "settings-content",
                    {settings_new_cards}
                    if sel == SettingsSection::Account {
                    div { class: "card",
                        div { class: "card-head",
                            h3 { class: "card-title", "Your account" }
                        }
                        p { class: "card-sub", "{server}" }
                        if !version.is_empty() {
                            p { class: "tiny", "Server {version}" }
                        }
                        if let Some(user) = &signed_in_user {
                            div { class: "kv-row",
                                span { class: "kv-key", "Signed in as" }
                                span { class: "kv-val", "{user.label()}" }
                            }
                            if !user.email.is_empty() && user.email != user.label() {
                                div { class: "kv-row",
                                    span { class: "kv-key", "Email" }
                                    span { class: "kv-val", "{user.email}" }
                                }
                            }
                            if !user.role.is_empty() {
                                div { class: "kv-row",
                                    span { class: "kv-key", "Role" }
                                    span { class: "kv-val", "{user.role}" }
                                }
                            }
                        } else {
                            p { class: "tiny", "Signed in with a saved session." }
                        }
                        div { class: "card-actions",
                            button {
                                class: "btn danger",
                                disabled: signing_out,
                                title: "End this session on the server and remove it from this device",
                                onclick: move |_| session.write().sign_out(),
                                if signing_out { "Signing out…" } else { "Sign out" }
                            }
                            if signing_out {
                                span { class: "spinner", title: "Signing out" }
                            }
                        }
                    }
                    }
                    if sel == SettingsSection::Telegram {
                    div { class: "card",
                        div { class: "card-head",
                            h3 { class: "card-title", "Telegram" }
                            div { class: "row",
                                if telegram_loading {
                                    span { class: "spinner", title: "Reading Telegram status" }
                                }
                                button {
                                    class: "btn ghost sm",
                                    disabled: telegram_loading || telegram_linking || telegram_unlinking,
                                    onclick: move |_| session.write().refresh_telegram(),
                                    Icon { name: "refresh", size: 14 }
                                    "Refresh status"
                                }
                            }
                        }
                        p { class: "card-sub",
                            "Link Telegram to chat with your bots from your phone. They use the same memory and ask you first, just like here."
                        }
                        if let Some(status) = telegram_status {
                            div { class: "chips",
                                span {
                                    class: if status.enabled { "chip ok" } else { "chip muted" },
                                    if status.enabled { "Telegram on" } else { "Telegram off" }
                                }
                                span {
                                    class: if status.linked { "chip ok" } else { "chip muted" },
                                    if status.linked { "linked" } else { "not linked" }
                                }
                                if let Some(chat) = status.chat_label() {
                                    span { class: "chip mono", "{chat}" }
                                }
                                if let Some(since) = status.linked_at.as_deref() {
                                    span { class: "chip", "since {stamp(since)}" }
                                }
                            }
                            if !status.enabled {
                                p { class: "tiny",
                                    "Telegram is turned off on the server. Ask your admin to turn it on."
                                }
                            }
                            if status.linked {
                                if telegram_confirm {
                                    div { class: "row",
                                        span { class: "tiny",
                                            "Unlink this chat? Messages from it will be ignored until you link again."
                                        }
                                        button {
                                            class: "btn danger sm",
                                            disabled: telegram_unlinking || telegram_loading,
                                            onclick: move |_| session.write().confirm_unlink_telegram(),
                                            if telegram_unlinking { "Unlinking…" } else { "Unlink" }
                                        }
                                        button {
                                            class: "btn ghost sm",
                                            disabled: telegram_unlinking,
                                            onclick: move |_| session.write().cancel_unlink_telegram(),
                                            "Cancel"
                                        }
                                    }
                                } else {
                                    div { class: "card-actions",
                                        button {
                                            class: "btn danger sm",
                                            onclick: move |_| session.write().request_unlink_telegram(),
                                            "Unlink Telegram"
                                        }
                                    }
                                }
                            } else if status.enabled {
                                div { class: "card-actions",
                                    button {
                                        class: "btn primary",
                                        disabled: telegram_linking || telegram_loading,
                                        onclick: move |_| session.write().link_telegram(),
                                        if telegram_linking { "Requesting code…" } else { "Link Telegram" }
                                    }
                                }
                            }
                        } else if telegram_loading {
                            div { class: "skeleton block" }
                        } else if telegram_loaded && telegram_error.is_none() {
                            p { class: "tiny",
                                "Telegram status is unavailable. Press Refresh status to retry."
                            }
                        } else if !telegram_loaded && telegram_error.is_none() {
                            p { class: "tiny", "Reading Telegram status…" }
                        }
                        if let Some(link) = telegram_link {
                            div { class: "field",
                                label { class: "field-label", "Send this command to the SeraBot bot" }
                                code { class: "link-code", "{link.command()}" }
                                p { class: "tiny",
                                    "The code works once and expires 10 minutes after it was requested. After sending it, press Refresh status."
                                }
                                if let Some(expires) = link.expires_at.as_deref() {
                                    p { class: "tiny", "Expires {expires}." }
                                }
                            }
                        }
                        if let Some(message) = telegram_error {
                            div { class: "banner err", span { "{message}" } }
                        }
                    }
                    }
                    if sel == SettingsSection::Admin && can_admin {
                    div { class: "card",
                        div { class: "card-head",
                            h3 { class: "card-title", "Safety checks" }
                            div { class: "row",
                                if status_loading {
                                    span { class: "spinner", title: "Reading the server posture" }
                                }
                                button {
                                    class: "btn ghost sm",
                                    onclick: move |_| session.write().refresh_status(),
                                    Icon { name: "refresh", size: 14 }
                                    "Refresh"
                                }
                            }
                        }
                        p { class: "card-sub", "What the server checks before your bots act." }
                        div { class: "chips",
                            span { class: "chip", "Safety rules: {policy_version}" }
                            span { class: "chip", "Tasks waiting: {pending_tasks}" }
                            span { class: "chip", "Waiting for your OK: {pending_approvals}" }
                            span {
                                class: if restart_pending { "chip warn" } else { "chip ok" },
                                if restart_pending { "Restart needed" } else { "Up to date" }
                            }
                        }
                        if let Some(error) = status_error {
                            div { class: "banner err", span { "{error}" } }
                        }
                        div { class: "field",
                            label { class: "field-label", "Bots ask you first before using" }
                            if bots_loaded {
                                div { class: "chips",
                                    for (_tool, label) in gated_tools {
                                        span { class: "chip warn", "{label}" }
                                    }
                                    if gated_empty {
                                        span { class: "chip muted", "Nothing needs your OK" }
                                    }
                                }
                            } else {
                                span { class: "hint", "unknown" }
                            }
                        }
                        if !pending_tools_empty {
                            div { class: "field",
                                label { class: "field-label", "Waiting for your decision" }
                                div { class: "chips",
                                    for tool in pending_tools {
                                        span { class: "chip accent", "{tool}" }
                                    }
                                }
                            }
                        }
                        div { class: "field",
                            label { class: "field-label", "Sending data to AI providers" }
                            if providers_loaded {
                                div { class: "chips",
                                    for (provider_id, egress_state, approved) in egress {
                                        span { class: if approved { "chip ok" } else { "chip warn" },
                                            "{provider_id}: {egress_state}"
                                        }
                                    }
                                    if egress_empty {
                                        span { class: "chip muted", "No AI provider yet" }
                                    }
                                }
                            } else {
                                span { class: "hint", "unknown" }
                            }
                        }
                    }
                    }
                    if sel == SettingsSection::Admin && can_admin {
                    div { class: "card",
                        div { class: "card-head",
                            h3 { class: "card-title", "What's turned on" }
                        }
                        div { class: "chips",
                            span {
                                class: match audio_enabled { Some(true) => "chip ok", Some(false) => "chip warn", None => "chip muted" },
                                match audio_enabled { Some(true) => "Audio on", Some(false) => "Audio off", None => "Audio unknown" }
                            }
                            span {
                                class: match firecrawl_enabled { Some(true) => "chip ok", Some(false) => "chip muted", None => "chip muted" },
                                match firecrawl_enabled { Some(true) => "Firecrawl on", Some(false) => "Firecrawl off", None => "Firecrawl unknown" }
                            }
                            span {
                                class: match telegram_enabled { Some(true) => "chip ok", Some(false) => "chip muted", None => "chip muted" },
                                match telegram_enabled { Some(true) => "Telegram on", Some(false) => "Telegram off", None => "Telegram unknown" }
                            }
                            span {
                                class: match catalog_fetch { Some(true) => "chip ok", Some(false) => "chip warn", None => "chip muted" },
                                match catalog_fetch { Some(true) => "Model lists update themselves", Some(false) => "Model lists are fixed", None => "Model lists unknown" }
                            }
                        }
                        if audio_enabled == Some(true) {
                            p { class: "tiny",
                                "Speech-to-text: {transcription}. Audio understanding: {understanding}."
                            }
                            p { class: "tiny",
                                "Voice notes you send on Telegram are turned into text before the bot answers."
                            }
                        } else if audio_enabled == Some(false) {
                            p { class: "tiny", "Audio is turned off on the server." }
                        } else {
                            p { class: "tiny",
                                "This server is too old to report what is turned on. Update it to see live status."
                            }
                        }
                        if catalog_fetch == Some(false) {
                            p { class: "tiny",
                                "Ask your admin to turn on live model lists, then use Fetch models on the AI keys page."
                            }
                        }
                        div { class: "form-grid",
                            div { class: "field",
                                label { class: "field-label", "Speech-to-text model" }
                                input {
                                    class: "input",
                                    value: "{audio_transcription}",
                                    placeholder: "speech-to-text model",
                                    oninput: move |event| session.write().audio.transcription_model = event.value(),
                                }
                            }
                            div { class: "field",
                                label { class: "field-label", "Audio understanding model" }
                                input {
                                    class: "input",
                                    value: "{audio_understanding}",
                                    placeholder: "audio understanding model",
                                    oninput: move |event| session.write().audio.understanding_model = event.value(),
                                }
                            }
                        }
                        p { class: "tiny",
                            "Audio uses the Mistral key you already saved under AI keys."
                        }
                        label { class: "check",
                            input {
                                "type": "checkbox",
                                checked: audio_on,
                                onchange: move |event| session.write().audio.enabled = event.checked(),
                            }
                            span { "Turn on audio transcription and understanding" }
                        }
                        button {
                            class: "btn primary",
                            disabled: !can_admin,
                            onclick: move |_| session.write().save_audio(),
                            "Save audio settings"
                        }
                    }
                    }
                    if sel == SettingsSection::Voice {
                    div { class: "card",
                        div { class: "card-head",
                            h3 { class: "card-title", "Voice and scanned pages" }
                            if voice_loading {
                                span { class: "chip muted", "Loading" }
                            }
                        }
                        div { class: "card-actions",
                            span {
                                class: if voice_stt_ready { "chip ok" } else { "chip warn" },
                                if voice_stt_ready { "Speech-to-text ready" } else { "Speech-to-text off" }
                            }
                            span {
                                class: if voice_tts_ready { "chip ok" } else { "chip warn" },
                                if voice_tts_ready { "Voice replies ready" } else { "Voice replies off" }
                            }
                            span {
                                class: if voice_ocr_ready { "chip ok" } else { "chip warn" },
                                if voice_ocr_ready { "Scanned pages ready" } else { "Scanned pages off" }
                            }
                        }
                        if !voice_loaded {
                            p { class: "tiny",
                                "Load the settings to see whether speech-to-text, voice replies and scanned-page reading can run on this server."
                            }
                            button {
                                class: "btn ghost",
                                disabled: voice_loading,
                                onclick: move |_| session.write().load_voice(),
                                "Load voice settings"
                            }
                        } else {
                            p { class: "tiny",
                                "Scanned PDFs and images are read with OCR when they have no text. Voice replies send a Telegram voice note instead of text."
                            }
                            if let Some(detail) = voice_stt_detail.clone() {
                                p { class: "tiny", "Speech to text: {detail}" }
                            }
                            if let Some(detail) = voice_tts_detail.clone() {
                                p { class: "tiny", "Voice replies: {detail}" }
                            }
                            if let Some(detail) = voice_ocr_detail.clone() {
                                p { class: "tiny", "Scanned pages: {detail}" }
                            }
                            div { class: "form-grid",
                                div { class: "field",
                                    label { class: "field-label", "Speech-to-text model" }
                                    input {
                                        class: "input",
                                        value: "{voice_stt}",
                                        disabled: true,
                                        placeholder: "speech-to-text model",
                                    }
                                }
                                div { class: "field",
                                    label { class: "field-label", "Voice reply model" }
                                    input {
                                        class: "input",
                                        value: "{voice_tts}",
                                        placeholder: "voxtral-mini-tts-2603",
                                        oninput: move |event| session.write().voice.tts_model = event.value(),
                                    }
                                }
                                div { class: "field",
                                    label { class: "field-label", "Voice" }
                                    input {
                                        class: "input",
                                        value: "{voice_id}",
                                        placeholder: "voice name from your provider",
                                        oninput: move |event| session.write().voice.voice_id = event.value(),
                                    }
                                }
                                div { class: "field",
                                    label { class: "field-label", "Scanned-page model" }
                                    input {
                                        class: "input",
                                        value: "{voice_ocr}",
                                        placeholder: "mistral-ocr-latest",
                                        oninput: move |event| session.write().voice.ocr_model = event.value(),
                                    }
                                }
                            }
                            label { class: "check",
                                input {
                                    "type": "checkbox",
                                    checked: voice_ocr_enabled,
                                    onchange: move |event| session.write().voice.ocr_enabled = event.checked(),
                                }
                                span { "Read scanned PDFs and images when they have no text" }
                            }
                            div { class: "card-actions",
                                for mode in ["text", "voice"] {
                                    button {
                                        class: if voice_reply_mode == mode { "btn primary" } else { "btn ghost" },
                                        onclick: {
                                            let mode = mode.to_string();
                                            move |_| session.write().voice.reply_mode = mode.clone()
                                        },
                                        if mode == "voice" { "Reply with voice" } else { "Reply with text" }
                                    }
                                }
                            }
                            p { class: "tiny",
                                "Voice replies stop at {voice_max_chars} characters. In Telegram, /voice and /text switch the style for that chat."
                            }
                            button {
                                class: "btn primary",
                                disabled: !can_admin,
                                onclick: move |_| session.write().save_voice(),
                                "Save voice settings"
                            }
                        }
                    }
                    }
                    if sel == SettingsSection::About {
                    div { class: "card",
                        div { class: "card-head",
                            h3 { class: "card-title", "Updates" }
                        }
                        button {
                            class: "btn ghost",
                            disabled: checking,
                            onclick: move |_| session.write().check_updates_manual(),
                            if checking { "Checking…" } else { "Check for updates" }
                        }
                        if let Some(message) = update_notice {
                            p { class: "tiny", "{message}" }
                        }
                        if let Some(info) = update {
                            p { class: "accent", "v{info.version} available" }
                            button {
                                class: "btn primary",
                                onclick: {
                                    let info = info.clone();
                                    move |_| {
                                        let result = session.write().apply_update(&info);
                                        match result {
                                            Ok(()) => std::process::exit(0),
                                            Err(error) => session.write().update.error = Some(error),
                                        }
                                    }
                                },
                                "Install and restart"
                            }
                        }
                        if let Some(error) = update_error {
                            p { class: "error-text", "{error}" }
                        }
                    }
                    }
                    if sel == SettingsSection::Chat {
                    div { class: "card",
                        div { class: "card-head",
                            h3 { class: "card-title", "Keyboard" }
                        }
                        p { class: "tiny", "Enter sends, Shift+Enter adds a line, Ctrl/Cmd+K focuses search, Esc closes the topmost sheet or menu." }
                    }
                    }
                }
            }
        }
    }
}
