//! Outbound notification fan-out for replies, voice and approval requests.

use super::*;

#[derive(Debug, Clone)]
pub struct ApprovalNotice {
    pub approval: Approval,
    pub action: Action,
}

#[async_trait]
pub trait Notifier: Send + Sync {
    /// Deliver one text message to the acting user's conversation. The
    /// user id scopes the delivery (event rows, per-user transports).
    async fn send_text(&self, user_id: &str, conversation_id: &str, text: &str) -> Result<()>;
    async fn approval_requested(
        &self,
        user_id: &str,
        conversation_id: &str,
        notice: &ApprovalNotice,
    ) -> Result<()>;

    /// Deliver one voice reply. The default falls back to text so a transport
    /// that cannot carry audio still delivers the content; Telegram overrides
    /// it with a real voice note.
    async fn send_voice(
        &self,
        user_id: &str,
        conversation_id: &str,
        text: &str,
        _audio: &[u8],
    ) -> Result<()> {
        self.send_text(user_id, conversation_id, text).await
    }
}

pub struct NullNotifier;

#[async_trait]
impl Notifier for NullNotifier {
    async fn send_text(&self, _user_id: &str, _conversation_id: &str, _text: &str) -> Result<()> {
        Ok(())
    }

    async fn approval_requested(
        &self,
        _user_id: &str,
        _conversation_id: &str,
        _notice: &ApprovalNotice,
    ) -> Result<()> {
        Ok(())
    }
}

#[derive(Clone)]
pub struct CompositeNotifier {
    targets: Vec<Arc<dyn Notifier>>,
}

impl CompositeNotifier {
    pub fn new(targets: Vec<Arc<dyn Notifier>>) -> Self {
        Self { targets }
    }
}

#[async_trait]
impl Notifier for CompositeNotifier {
    async fn send_text(&self, user_id: &str, conversation_id: &str, text: &str) -> Result<()> {
        let mut first_error = None;
        for target in &self.targets {
            if let Err(error) = target.send_text(user_id, conversation_id, text).await {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    async fn approval_requested(
        &self,
        user_id: &str,
        conversation_id: &str,
        notice: &ApprovalNotice,
    ) -> Result<()> {
        let mut first_error = None;
        for target in &self.targets {
            if let Err(error) = target
                .approval_requested(user_id, conversation_id, notice)
                .await
            {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}
