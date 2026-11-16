//! Plugin accounts, per-tool switches and the native GitHub sign-in, on the
//! console session (v0.51).
//!
//! A sign-in runs on its own thread (like the provider sign-in) so the screen
//! stays alive and the member can cancel at once. The short reads and writes
//! (the tool list, a tool switch, signing an account out, the GitHub status)
//! run on one-shot threads that answer through a channel the UI timer drains,
//! so none of them waits behind the worker's single queue.
//!
//! Nothing secret lives here. Tokens stay on the server; this state holds the
//! words the server reports (account states, tool hints) and the one-time
//! device code GitHub asks the member to type.

use super::*;
use crate::marketplace::{GithubView, PluginTool};
use crate::marketplace_flow;
use crate::provider_flow::{FlowEnd, FlowKind, FlowMsg, ProviderFlow};
use std::sync::mpsc::{Receiver, Sender, channel};

/// What a sign-in on the screen is for.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SignInTarget {
    Plugin { slug: String, account_id: String },
    Github,
}

/// Where the tool list of the open plugin stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolsState {
    Loading,
    Ready,
    Failed(String),
}

/// The open plugin's tools and their switches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolsView {
    pub slug: String,
    pub state: ToolsState,
    pub rows: Vec<PluginTool>,
    /// The last switch that did not land, as a sentence.
    pub error: Option<String>,
}

/// An answer from a one-shot thread.
enum McpMsg {
    Tools {
        slug: String,
        result: Result<Vec<PluginTool>, ApiError>,
    },
    ToolMode {
        slug: String,
        before: PluginTool,
        result: Result<(), ApiError>,
    },
    AccountRemoved {
        slug: String,
        result: Result<(), ApiError>,
    },
    Github(Result<GithubView, ApiError>),
    GithubDisconnected(Result<Option<String>, ApiError>),
}

/// The Integrations screen's account state: one sign-in at a time, the open
/// plugin's tools, and the GitHub card.
pub struct McpUi {
    tx: Sender<McpMsg>,
    rx: Receiver<McpMsg>,
    /// What the running or finished sign-in is for.
    pub target: Option<SignInTarget>,
    pub flow: Option<ProviderFlow>,
    flow_since: Option<Instant>,
    flow_cancel: Option<Arc<AtomicBool>>,
    flow_rx: Option<Receiver<FlowMsg>>,
    pub tools: Option<ToolsView>,
    /// An account whose sign-out waits for its confirm step.
    pub confirm_remove: Option<(String, String)>,
    /// A sign-out in flight.
    pub removing: Option<(String, String)>,
    pub notice: Option<String>,
    pub error: Option<String>,
    pub github: Option<GithubView>,
    pub github_error: Option<String>,
    pub github_busy: bool,
    pub confirm_disconnect: bool,
}

impl Default for McpUi {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            tx,
            rx,
            target: None,
            flow: None,
            flow_since: None,
            flow_cancel: None,
            flow_rx: None,
            tools: None,
            confirm_remove: None,
            removing: None,
            notice: None,
            error: None,
            github: None,
            github_error: None,
            github_busy: false,
            confirm_disconnect: false,
        }
    }
}

impl McpUi {
    pub fn flow_running(&self) -> bool {
        self.flow
            .as_ref()
            .is_some_and(|flow| !flow.phase.is_final())
    }

    /// Time since the page was issued; zero before that.
    pub fn flow_elapsed(&self) -> Duration {
        self.flow_since.map(|at| at.elapsed()).unwrap_or_default()
    }

    fn stop_flow(&mut self) {
        if let Some(cancel) = self.flow_cancel.take() {
            cancel.store(true, Ordering::SeqCst);
        }
        self.flow_rx = None;
        self.flow_since = None;
    }

    /// The sign-in panel for one plugin, while it is the open sign-in.
    pub fn plugin_flow(&self, slug: &str) -> Option<&ProviderFlow> {
        match &self.target {
            Some(SignInTarget::Plugin { slug: open, .. }) if open == slug => self.flow.as_ref(),
            _ => None,
        }
    }

    /// The sign-in panel for GitHub, while it is the open sign-in.
    pub fn github_flow(&self) -> Option<&ProviderFlow> {
        match &self.target {
            Some(SignInTarget::Github) => self.flow.as_ref(),
            _ => None,
        }
    }

    /// The account a plugin sign-in is for, while one is open.
    pub fn flow_account(&self) -> Option<&str> {
        match &self.target {
            Some(SignInTarget::Plugin { account_id, .. }) => Some(account_id),
            _ => None,
        }
    }
}

impl Drop for McpUi {
    fn drop(&mut self) {
        self.stop_flow();
    }
}

/// A sentence for a failed marketplace call on this screen.
fn plain(error: &ApiError, what: &str) -> String {
    match error {
        ApiError::Http(404) => {
            format!("{what}: that is not installed, or this server cannot do it. Refresh the page.")
        }
        ApiError::Rejected(message) => crate::provider_accounts::plain_error(message),
        ApiError::Transport => "Could not reach the server. Check the connection.".to_owned(),
        other => format!("{what}: {other}"),
    }
}

impl Session {
    // ---- Sign-in ----

    fn mcp_ready_to_start(&mut self) -> bool {
        if !self.state.connected {
            self.mcp_ui.error = Some("Connect to the server first.".into());
            return false;
        }
        if self.mcp_ui.flow_running() {
            self.mcp_ui.error = Some("Finish or cancel the sign-in in progress first.".into());
            return false;
        }
        true
    }

    /// Start the browser sign-in of a plugin account. `renew` signs an
    /// existing account in again.
    pub fn start_plugin_signin(
        &mut self,
        slug: String,
        account_id: String,
        label: Option<String>,
        renew: bool,
    ) {
        if !crate::valid_id(&slug) || !crate::marketplace::valid_account_id(&account_id) {
            return;
        }
        if !self.mcp_ready_to_start() {
            return;
        }
        // The same rule as Install: no sign-in for a plugin that cannot run.
        if let Some(reason) = self.plugin_activation_block(&slug) {
            self.mcp_ui.error = Some(reason.message().to_owned());
            return;
        }
        let Some(credentials) = self.credentials.clone() else {
            self.mcp_ui.error = Some("Connect to the server first.".into());
            return;
        };
        self.mcp_ui.stop_flow();
        self.mcp_ui.error = None;
        self.mcp_ui.notice = None;
        let (sender, receiver) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let thread_cancel = Arc::clone(&cancel);
        let (thread_slug, thread_account) = (slug.clone(), account_id.clone());
        let spawned = std::thread::Builder::new()
            .name("serabot-plugin-signin".into())
            .spawn(move || {
                let report = |msg: FlowMsg| {
                    let _ = sender.send(msg);
                };
                let open = |url: &str| webbrowser::open(url).is_ok();
                let end =
                    match ApiClient::new(credentials.endpoint.clone(), credentials.token.clone()) {
                        Err(error) => FlowEnd::Failed(error.to_string()),
                        Ok(client) => marketplace_flow::run_plugin_signin(
                            &client,
                            &thread_slug,
                            &thread_account,
                            label.as_deref(),
                            renew,
                            &thread_cancel,
                            &report,
                            &open,
                        ),
                    };
                let _ = sender.send(FlowMsg::Ended(end));
            });
        if spawned.is_err() {
            self.mcp_ui.error = Some("Could not start the sign-in.".into());
            return;
        }
        self.mcp_ui.target = Some(SignInTarget::Plugin { slug, account_id });
        self.mcp_ui.flow = Some(ProviderFlow::new("plugin", FlowKind::Browser));
        self.mcp_ui.flow_since = None;
        self.mcp_ui.flow_cancel = Some(cancel);
        self.mcp_ui.flow_rx = Some(receiver);
    }

    /// Start GitHub's device sign-in: a one-time code to type on GitHub.
    pub fn start_github_signin(&mut self) {
        if !self.mcp_ready_to_start() {
            return;
        }
        let Some(credentials) = self.credentials.clone() else {
            self.mcp_ui.error = Some("Connect to the server first.".into());
            return;
        };
        self.mcp_ui.stop_flow();
        self.mcp_ui.error = None;
        self.mcp_ui.notice = None;
        self.mcp_ui.github_error = None;
        let (sender, receiver) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let thread_cancel = Arc::clone(&cancel);
        let spawned = std::thread::Builder::new()
            .name("serabot-github-signin".into())
            .spawn(move || {
                let report = |msg: FlowMsg| {
                    let _ = sender.send(msg);
                };
                let open = |url: &str| webbrowser::open(url).is_ok();
                let end =
                    match ApiClient::new(credentials.endpoint.clone(), credentials.token.clone()) {
                        Err(error) => FlowEnd::Failed(error.to_string()),
                        Ok(client) => marketplace_flow::run_github_device(
                            &client,
                            &thread_cancel,
                            &report,
                            &open,
                        ),
                    };
                let _ = sender.send(FlowMsg::Ended(end));
            });
        if spawned.is_err() {
            self.mcp_ui.github_error = Some("Could not start the sign-in.".into());
            return;
        }
        self.mcp_ui.target = Some(SignInTarget::Github);
        self.mcp_ui.flow = Some(ProviderFlow::new("github", FlowKind::Device));
        self.mcp_ui.flow_since = None;
        self.mcp_ui.flow_cancel = Some(cancel);
        self.mcp_ui.flow_rx = Some(receiver);
    }

    /// Stop a sign-in the member backed out of. The listener closes at once.
    pub fn cancel_mcp_signin(&mut self) {
        let was_running = self.mcp_ui.flow_running();
        if let Some(cancel) = self.mcp_ui.flow_cancel.take() {
            cancel.store(true, Ordering::SeqCst);
        }
        self.mcp_ui.flow_rx = None;
        self.mcp_ui.flow_since = None;
        if was_running && let Some(flow) = self.mcp_ui.flow.as_mut() {
            flow.cancel();
        }
    }

    /// Clear a finished sign-in's panel (a final state stays until dismissed).
    pub fn dismiss_mcp_flow(&mut self) {
        if self.mcp_ui.flow_running() {
            return;
        }
        self.mcp_ui.flow = None;
        self.mcp_ui.target = None;
    }

    /// A sign-in for a plugin that is no longer open is stopped. Called when
    /// the plugin page closes or another opens.
    pub(super) fn drop_plugin_signin_unless(&mut self, keep: Option<&str>) {
        let stale = match &self.mcp_ui.target {
            Some(SignInTarget::Plugin { slug, .. }) => Some(slug.as_str()) != keep,
            _ => false,
        };
        if stale {
            self.cancel_mcp_signin();
            self.mcp_ui.flow = None;
            self.mcp_ui.target = None;
        }
        let keep_tools = self
            .mcp_ui
            .tools
            .as_ref()
            .is_some_and(|tools| Some(tools.slug.as_str()) == keep);
        if !keep_tools {
            self.mcp_ui.tools = None;
        }
        self.mcp_ui.confirm_remove = None;
        self.mcp_ui.error = None;
    }

    // ---- Short calls ----

    /// Run one short call on its own thread and answer through the channel.
    fn spawn_mcp(&mut self, job: impl FnOnce(&ApiClient) -> McpMsg + Send + 'static) -> bool {
        let Some(credentials) = self.credentials.clone() else {
            return false;
        };
        let tx = self.mcp_ui.tx.clone();
        std::thread::Builder::new()
            .name("serabot-marketplace".into())
            .spawn(move || {
                let message =
                    match ApiClient::new(credentials.endpoint.clone(), credentials.token.clone()) {
                        Ok(client) => job(&client),
                        Err(_) => return,
                    };
                let _ = tx.send(message);
            })
            .is_ok()
    }

    /// Read the plugin's tools for the switches. Keeps the rows already shown
    /// while it reloads, so the list does not blink.
    pub fn load_plugin_tools(&mut self, slug: String) {
        if !self.state.connected || !crate::valid_id(&slug) {
            return;
        }
        let keep = self
            .mcp_ui
            .tools
            .take()
            .filter(|tools| tools.slug == slug)
            .map(|tools| tools.rows)
            .unwrap_or_default();
        self.mcp_ui.tools = Some(ToolsView {
            slug: slug.clone(),
            state: ToolsState::Loading,
            rows: keep,
            error: None,
        });
        let asked = slug.clone();
        let started = self.spawn_mcp(move |client| McpMsg::Tools {
            result: client.marketplace_tools(&asked),
            slug: asked,
        });
        if !started && let Some(tools) = self.mcp_ui.tools.as_mut() {
            tools.state = ToolsState::Failed("Connect to the server first.".into());
        }
    }

    /// Flip one tool's switch. The row changes at once; if the server refuses,
    /// it goes back and the reason is shown.
    pub fn flip_plugin_tool(&mut self, slug: &str, tool: &str) {
        let Some(view) = self.mcp_ui.tools.as_mut().filter(|view| view.slug == slug) else {
            return;
        };
        let Some(at) = view.rows.iter().position(|row| row.name == tool) else {
            return;
        };
        let before = view.rows[at].clone();
        let Some(mode) = before.flip() else {
            view.error = Some(format!(
                "{} can change or delete data, so it always asks first.",
                before.name
            ));
            return;
        };
        view.error = None;
        view.rows[at] = before.with_mode(mode);
        let slug_owned = slug.to_owned();
        let tool_owned = tool.to_owned();
        let restore = before.clone();
        let sent = self.spawn_mcp(move |client| McpMsg::ToolMode {
            result: client.marketplace_tool_mode(&slug_owned, &tool_owned, mode),
            slug: slug_owned,
            before,
        });
        if !sent && let Some(view) = self.mcp_ui.tools.as_mut() {
            if let Some(row) = view.rows.get_mut(at) {
                *row = restore;
            }
            view.error = Some("Connect to the server first.".into());
        }
    }

    // ---- Account rows ----

    /// Ask for a confirm step before signing an account out.
    pub fn ask_remove_plugin_account(&mut self, slug: &str, account: &str) {
        self.mcp_ui.confirm_remove = Some((slug.to_owned(), account.to_owned()));
    }

    pub fn cancel_remove_plugin_account(&mut self) {
        self.mcp_ui.confirm_remove = None;
    }

    /// Sign an account out. The server revokes the token and deletes it.
    pub fn remove_plugin_account(&mut self, slug: &str, account: &str) {
        self.mcp_ui.confirm_remove = None;
        if !self.state.connected
            || !crate::valid_id(slug)
            || !crate::marketplace::valid_account_id(account)
            || self.mcp_ui.removing.is_some()
        {
            return;
        }
        self.mcp_ui.error = None;
        self.mcp_ui.notice = None;
        self.mcp_ui.removing = Some((slug.to_owned(), account.to_owned()));
        let (slug_owned, account_owned) = (slug.to_owned(), account.to_owned());
        let sent = self.spawn_mcp(move |client| McpMsg::AccountRemoved {
            result: client.marketplace_remove_account(&slug_owned, &account_owned),
            slug: slug_owned,
        });
        if !sent {
            self.mcp_ui.removing = None;
            self.mcp_ui.error = Some("Connect to the server first.".into());
        }
    }

    /// Read the plugin page, its accounts and its tools again (for "Check again").
    pub fn recheck_plugin(&mut self, slug: &str) {
        if !self.state.connected || !crate::valid_id(slug) {
            return;
        }
        self.mcp_ui.error = None;
        self.mcp_ui.notice = None;
        self.reload_plugin(slug);
    }

    // ---- GitHub ----

    /// Read the member's native GitHub sign-in.
    pub fn load_github(&mut self) {
        if !self.state.connected || self.mcp_ui.github_busy {
            return;
        }
        self.mcp_ui.github_busy = true;
        self.mcp_ui.github_error = None;
        if !self.spawn_mcp(|client| McpMsg::Github(client.github_native())) {
            self.mcp_ui.github_busy = false;
        }
    }

    pub fn ask_disconnect_github(&mut self) {
        self.mcp_ui.confirm_disconnect = true;
    }

    pub fn cancel_disconnect_github(&mut self) {
        self.mcp_ui.confirm_disconnect = false;
    }

    /// Forget the GitHub sign-in. GitHub's own page for revoking the app opens
    /// afterwards, where the member can finish the job.
    pub fn disconnect_github(&mut self) {
        self.mcp_ui.confirm_disconnect = false;
        if !self.state.connected || self.mcp_ui.github_busy {
            return;
        }
        self.mcp_ui.github_busy = true;
        self.mcp_ui.github_error = None;
        if !self.spawn_mcp(|client| McpMsg::GithubDisconnected(client.github_disconnect())) {
            self.mcp_ui.github_busy = false;
        }
    }

    // ---- Timer ----

    /// Drain the sign-in thread and the one-shot answers. Called from the UI timer.
    pub(super) fn poll_mcp(&mut self) {
        self.poll_mcp_flow();
        while let Ok(message) = self.mcp_ui.rx.try_recv() {
            self.apply_mcp(message);
        }
    }

    fn poll_mcp_flow(&mut self) {
        let Some(receiver) = &self.mcp_ui.flow_rx else {
            return;
        };
        let mut messages = Vec::new();
        while let Ok(message) = receiver.try_recv() {
            messages.push(message);
        }
        let mut finished = false;
        for message in messages {
            if let FlowMsg::Started { .. } = &message {
                self.mcp_ui.flow_since = Some(Instant::now());
            }
            if let FlowMsg::Ended(end) = &message {
                finished = true;
                if matches!(end, FlowEnd::Done { .. }) {
                    self.mcp_flow_done();
                }
            }
            if let Some(flow) = self.mcp_ui.flow.as_mut() {
                flow.apply(message);
            }
        }
        if finished {
            self.mcp_ui.flow_rx = None;
            self.mcp_ui.flow_cancel = None;
        }
    }

    /// A sign-in finished: show the new state at once.
    fn mcp_flow_done(&mut self) {
        match self.mcp_ui.target.clone() {
            Some(SignInTarget::Plugin { slug, .. }) => {
                self.mcp_ui.notice = Some("Signed in.".into());
                self.reload_plugin(&slug);
            }
            Some(SignInTarget::Github) => {
                self.mcp_ui.notice = Some("GitHub is connected.".into());
                self.mcp_ui.github_busy = false;
                self.load_github();
                self.refresh_connectors();
            }
            None => {}
        }
    }

    /// Read the plugin page, the catalogue and the tools again.
    fn reload_plugin(&mut self, slug: &str) {
        if self.marketplace_open.as_deref() == Some(slug) {
            self.start_request(Operation::MarketplaceDetail {
                slug: slug.to_owned(),
            });
        }
        self.refresh_marketplace();
        if self
            .mcp_ui
            .tools
            .as_ref()
            .is_some_and(|tools| tools.slug == slug)
        {
            self.load_plugin_tools(slug.to_owned());
        }
    }

    fn apply_mcp(&mut self, message: McpMsg) {
        match message {
            McpMsg::Tools { slug, result } => {
                let Some(view) = self.mcp_ui.tools.as_mut().filter(|view| view.slug == slug) else {
                    return;
                };
                match result {
                    Ok(rows) => {
                        view.rows = rows;
                        view.state = ToolsState::Ready;
                    }
                    Err(error) => {
                        view.state = ToolsState::Failed(plain(&error, "Tools could not be listed"));
                    }
                }
            }
            McpMsg::ToolMode {
                slug,
                before,
                result,
            } => {
                let Some(view) = self.mcp_ui.tools.as_mut().filter(|view| view.slug == slug) else {
                    return;
                };
                if let Err(error) = result {
                    if let Some(row) = view.rows.iter_mut().find(|row| row.name == before.name) {
                        *row = before;
                    }
                    view.error = Some(plain(&error, "That switch did not save"));
                }
            }
            McpMsg::AccountRemoved { slug, result } => {
                self.mcp_ui.removing = None;
                match result {
                    Ok(()) => {
                        self.mcp_ui.notice = Some("Signed out.".into());
                        self.reload_plugin(&slug);
                    }
                    Err(error) => {
                        self.mcp_ui.error = Some(plain(&error, "Sign-out failed"));
                    }
                }
            }
            McpMsg::Github(result) => {
                self.mcp_ui.github_busy = false;
                match result {
                    Ok(view) => {
                        self.mcp_ui.github = Some(view);
                        self.mcp_ui.github_error = None;
                    }
                    Err(ApiError::Http(404)) => {
                        self.mcp_ui.github_error = Some(
                            "This server has no native GitHub sign-in yet. Update the server."
                                .into(),
                        );
                    }
                    Err(error) => {
                        self.mcp_ui.github_error = Some(plain(&error, "GitHub status"));
                    }
                }
            }
            McpMsg::GithubDisconnected(result) => {
                self.mcp_ui.github_busy = false;
                match result {
                    Ok(manage_url) => {
                        self.mcp_ui.notice = Some("GitHub is disconnected.".into());
                        if self.mcp_ui.target == Some(SignInTarget::Github) {
                            self.mcp_ui.flow = None;
                            self.mcp_ui.target = None;
                        }
                        self.load_github();
                        self.refresh_connectors();
                        if let Some(url) = manage_url {
                            self.open_in_browser(url);
                        }
                    }
                    Err(error) => {
                        self.mcp_ui.github_error = Some(plain(&error, "Disconnect failed"));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marketplace::{GithubView, PluginDetail, ToolMode};

    fn connected() -> Session {
        let mut session = Session::new();
        session.state.connected = true;
        session
    }

    fn tool(name: &str, read_only: bool, destructive: bool) -> PluginTool {
        PluginTool {
            name: name.into(),
            description: String::new(),
            read_only,
            destructive,
            mode: None,
        }
    }

    fn with_tools(session: &mut Session, rows: Vec<PluginTool>) {
        session.mcp_ui.tools = Some(ToolsView {
            slug: "canva".into(),
            state: ToolsState::Ready,
            rows,
            error: None,
        });
    }

    fn oauth_detail(status: &str) -> PluginDetail {
        serde_json::from_value(serde_json::json!({"plugin": {
            "slug": "canva", "name": "Canva", "installed": true, "status": status,
            "apps": [{"id": "mcp", "name": "Canva", "transport": "http",
                      "auth": [{"id": "o", "kind": "oauth", "fields": []}]}],
            "permissions": {"egress": ["mcp.canva.com"]},
            "accounts": []
        }}))
        .unwrap()
    }

    #[test]
    fn a_sign_in_needs_a_connection_and_a_plugin_that_can_run() {
        let mut offline = Session::new();
        offline.start_plugin_signin("canva".into(), "default".into(), None, false);
        assert_eq!(
            offline.mcp_ui.error.as_deref(),
            Some("Connect to the server first.")
        );
        assert!(offline.mcp_ui.flow.is_none());

        // Connected, but the page is not loaded: nothing starts.
        let mut session = connected();
        session.start_plugin_signin("canva".into(), "default".into(), None, false);
        assert!(session.mcp_ui.flow.is_none());
        assert!(session.mcp_ui.error.is_some());

        // A placeholder host never starts a sign-in.
        let mut placeholder = oauth_detail("needs_sign_in");
        placeholder.plugin.permissions.egress = vec!["mcp.example.invalid".into()];
        session.state.marketplace_detail = Some(placeholder);
        session.start_plugin_signin("canva".into(), "default".into(), None, false);
        assert!(session.mcp_ui.flow.is_none());
        assert!(
            session
                .mcp_ui
                .error
                .as_deref()
                .unwrap()
                .contains("live service")
        );

        // A bad id is ignored outright.
        session.mcp_ui.error = None;
        session.start_plugin_signin("canva".into(), "Bad Id".into(), None, false);
        assert!(session.mcp_ui.error.is_none() && session.mcp_ui.flow.is_none());
    }

    #[test]
    fn only_one_sign_in_runs_and_cancel_is_always_available() {
        let mut session = connected();
        let cancel = Arc::new(AtomicBool::new(false));
        session.mcp_ui.flow_cancel = Some(Arc::clone(&cancel));
        session.mcp_ui.flow = Some(ProviderFlow::new("github", FlowKind::Device));
        session.mcp_ui.target = Some(SignInTarget::Github);
        // A second start is refused while one waits.
        session.start_github_signin();
        assert!(
            session
                .mcp_ui
                .error
                .as_deref()
                .unwrap()
                .contains("in progress")
        );
        // Dismiss does nothing to a running sign-in.
        session.dismiss_mcp_flow();
        assert!(session.mcp_ui.flow.is_some());
        // Cancel tells the runner to stop and the panel says so.
        session.cancel_mcp_signin();
        assert!(cancel.load(Ordering::SeqCst));
        assert_eq!(
            session.mcp_ui.flow.as_ref().unwrap().phase,
            crate::provider_flow::FlowPhase::Cancelled
        );
        // A finished panel can be dismissed.
        session.dismiss_mcp_flow();
        assert!(session.mcp_ui.flow.is_none() && session.mcp_ui.target.is_none());
    }

    #[test]
    fn leaving_a_plugin_page_stops_its_sign_in_and_clears_its_tools() {
        let mut session = connected();
        let cancel = Arc::new(AtomicBool::new(false));
        session.mcp_ui.flow_cancel = Some(Arc::clone(&cancel));
        session.mcp_ui.flow = Some(ProviderFlow::new("plugin", FlowKind::Browser));
        session.mcp_ui.target = Some(SignInTarget::Plugin {
            slug: "canva".into(),
            account_id: "default".into(),
        });
        with_tools(&mut session, vec![tool("lookup", true, false)]);
        // The same plugin staying open changes nothing.
        session.drop_plugin_signin_unless(Some("canva"));
        assert!(!cancel.load(Ordering::SeqCst));
        assert!(session.mcp_ui.tools.is_some());
        // Another plugin (or none) stops the sign-in and drops the tools.
        session.drop_plugin_signin_unless(None);
        assert!(cancel.load(Ordering::SeqCst));
        assert!(session.mcp_ui.flow.is_none() && session.mcp_ui.tools.is_none());
        // A GitHub sign-in is not tied to a plugin page.
        session.mcp_ui.flow = Some(ProviderFlow::new("github", FlowKind::Device));
        session.mcp_ui.target = Some(SignInTarget::Github);
        session.drop_plugin_signin_unless(None);
        assert!(session.mcp_ui.flow.is_some());
    }

    #[test]
    fn a_tool_switch_shows_at_once_and_rolls_back_when_the_server_refuses() {
        let mut session = connected();
        with_tools(
            &mut session,
            vec![tool("lookup", true, false), tool("delete", false, true)],
        );
        // No credentials in a unit test, so nothing is sent; the failure is reported.
        session.flip_plugin_tool("canva", "lookup");
        let view = session.mcp_ui.tools.as_ref().unwrap();
        assert_eq!(view.error.as_deref(), Some("Connect to the server first."));
        session.mcp_ui.tools.as_mut().unwrap().error = None;

        // A refused switch comes back from the thread: the row is restored.
        let before = tool("lookup", true, false);
        session.mcp_ui.tools.as_mut().unwrap().rows[0] = before.with_mode(ToolMode::Ask);
        session.apply_mcp(McpMsg::ToolMode {
            slug: "canva".into(),
            before: before.clone(),
            result: Err(ApiError::Http(500)),
        });
        let view = session.mcp_ui.tools.as_ref().unwrap();
        assert_eq!(view.rows[0], before);
        assert!(view.error.as_deref().unwrap().contains("did not save"));

        // A saved switch leaves the row as flipped.
        session.mcp_ui.tools.as_mut().unwrap().rows[0] = before.with_mode(ToolMode::Ask);
        session.apply_mcp(McpMsg::ToolMode {
            slug: "canva".into(),
            before,
            result: Ok(()),
        });
        assert_eq!(
            session.mcp_ui.tools.as_ref().unwrap().rows[0].override_mode(),
            ToolMode::Ask
        );

        // A destructive tool does not flip; the reason is shown instead.
        session.mcp_ui.tools.as_mut().unwrap().error = None;
        session.flip_plugin_tool("canva", "delete");
        let view = session.mcp_ui.tools.as_ref().unwrap();
        assert!(view.error.as_deref().unwrap().contains("always asks"));
        assert_eq!(view.rows[1].override_mode(), ToolMode::Default);

        // An answer for a plugin that is no longer open is dropped.
        session.apply_mcp(McpMsg::Tools {
            slug: "other".into(),
            result: Ok(vec![tool("x", true, false)]),
        });
        assert_eq!(session.mcp_ui.tools.as_ref().unwrap().rows.len(), 2);
    }

    #[test]
    fn the_tool_list_loads_or_says_why_not() {
        let mut session = connected();
        with_tools(&mut session, vec![tool("old", true, false)]);
        session.apply_mcp(McpMsg::Tools {
            slug: "canva".into(),
            result: Ok(vec![tool("a", true, false), tool("b", false, false)]),
        });
        let view = session.mcp_ui.tools.as_ref().unwrap();
        assert_eq!(
            (view.state.clone(), view.rows.len()),
            (ToolsState::Ready, 2)
        );
        session.apply_mcp(McpMsg::Tools {
            slug: "canva".into(),
            result: Err(ApiError::Rejected("plugin canva needs sign-in".into())),
        });
        let view = session.mcp_ui.tools.as_ref().unwrap();
        assert_eq!(
            view.state,
            ToolsState::Failed("plugin canva needs sign-in".into())
        );
    }

    #[test]
    fn signing_an_account_out_confirms_first_and_reports_the_result() {
        let mut session = connected();
        session.ask_remove_plugin_account("canva", "default");
        assert_eq!(
            session.mcp_ui.confirm_remove,
            Some(("canva".into(), "default".into()))
        );
        session.cancel_remove_plugin_account();
        assert!(session.mcp_ui.confirm_remove.is_none());
        // Without credentials the call cannot go out, and the state is not stuck.
        session.remove_plugin_account("canva", "default");
        assert!(session.mcp_ui.removing.is_none());
        assert!(session.mcp_ui.error.is_some());
        // A failed sign-out is said; a good one is noted.
        session.mcp_ui.removing = Some(("canva".into(), "default".into()));
        session.apply_mcp(McpMsg::AccountRemoved {
            slug: "canva".into(),
            result: Err(ApiError::Transport),
        });
        assert!(session.mcp_ui.removing.is_none());
        assert!(
            session
                .mcp_ui
                .error
                .as_deref()
                .unwrap()
                .contains("Could not reach")
        );
        session.apply_mcp(McpMsg::AccountRemoved {
            slug: "canva".into(),
            result: Ok(()),
        });
        assert_eq!(session.mcp_ui.notice.as_deref(), Some("Signed out."));
        // An id the server would refuse is never sent.
        session.mcp_ui.error = None;
        session.remove_plugin_account("canva", "Bad Id");
        assert!(session.mcp_ui.error.is_none() && session.mcp_ui.removing.is_none());
    }

    #[test]
    fn the_github_card_reads_its_state_and_disconnects_with_a_confirm() {
        let mut session = connected();
        let view = |state: &str| GithubView {
            state: state.into(),
            configured: true,
            login: Some("octo-cat".into()),
            pending: false,
            manage_url: None,
        };
        session.mcp_ui.github_busy = true;
        session.apply_mcp(McpMsg::Github(Ok(view("connected"))));
        assert!(!session.mcp_ui.github_busy);
        assert_eq!(session.mcp_ui.github.as_ref().unwrap().badge(), "Connected");
        // An older server without the route says to update it.
        session.apply_mcp(McpMsg::Github(Err(ApiError::Http(404))));
        assert!(
            session
                .mcp_ui
                .github_error
                .as_deref()
                .unwrap()
                .contains("Update the server")
        );
        // Disconnect asks first.
        session.ask_disconnect_github();
        assert!(session.mcp_ui.confirm_disconnect);
        session.cancel_disconnect_github();
        assert!(!session.mcp_ui.confirm_disconnect);
        // A disconnect failure is shown on the card.
        session.apply_mcp(McpMsg::GithubDisconnected(Err(ApiError::Transport)));
        assert!(session.mcp_ui.github_error.is_some());
        // A good one clears the code panel and notes it.
        session.mcp_ui.target = Some(SignInTarget::Github);
        session.mcp_ui.flow = Some(ProviderFlow::new("github", FlowKind::Device));
        session.apply_mcp(McpMsg::GithubDisconnected(Ok(None)));
        assert!(session.mcp_ui.flow.is_none());
        assert_eq!(
            session.mcp_ui.notice.as_deref(),
            Some("GitHub is disconnected.")
        );
    }

    #[test]
    fn a_finished_sign_in_refreshes_what_the_screen_shows() {
        let mut session = connected();
        session.mcp_ui.target = Some(SignInTarget::Plugin {
            slug: "canva".into(),
            account_id: "default".into(),
        });
        session.mcp_ui.flow = Some(ProviderFlow::new("plugin", FlowKind::Browser));
        let (sender, receiver) = channel();
        session.mcp_ui.flow_rx = Some(receiver);
        sender
            .send(FlowMsg::Ended(FlowEnd::Done {
                account_id: "default".into(),
                created: true,
            }))
            .unwrap();
        session.poll_mcp();
        assert_eq!(session.mcp_ui.notice.as_deref(), Some("Signed in."));
        assert!(session.mcp_ui.flow_rx.is_none());
        assert!(matches!(
            session.mcp_ui.flow.as_ref().unwrap().phase,
            crate::provider_flow::FlowPhase::Done { created: true }
        ));
    }

    #[test]
    fn sign_out_of_the_console_cancels_a_running_sign_in() {
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut ui = McpUi::default();
            ui.flow_cancel = Some(Arc::clone(&cancel));
        }
        assert!(cancel.load(Ordering::SeqCst), "dropping the state cancels");
    }
}
