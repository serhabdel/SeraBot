use crate::{
    ApiClient, ApiError, Approval, ApprovalDecision, Catalog, ConversationKind, CreatedBot,
    CreatedConversation, CreatedSkill, Health, History, Scope, Selection, SkillSummary, Task,
};
use secrecy::SecretString;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
    mpsc::{self, Receiver, SyncSender, TrySendError},
};
use url::Url;
use zeroize::Zeroizing;

pub struct Credentials {
    pub endpoint: Url,
    pub token: SecretString,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationKind {
    Health,
    Catalog,
    History,
    Apply,
    Submit,
    Task,
    Approvals,
    Decide,
    CreateBot,
    CreateConversation,
    CreateSkill,
    ListSkills,
}

pub enum Operation {
    Health,
    Catalog,
    History,
    Apply(Selection),
    Submit(Zeroizing<String>),
    Task(String),
    Approvals,
    Decide {
        id: String,
        decision: ApprovalDecision,
    },
    CreateBot {
        id: String,
        description: Zeroizing<String>,
        tools: Vec<String>,
        skills: Vec<String>,
    },
    CreateConversation {
        kind: ConversationKind,
        slug: String,
        parent_id: Option<String>,
    },
    CreateSkill {
        id: String,
        description: Zeroizing<String>,
        body: Zeroizing<String>,
    },
    ListSkills,
}

impl Operation {
    pub fn kind(&self) -> OperationKind {
        match self {
            Self::Health => OperationKind::Health,
            Self::Catalog => OperationKind::Catalog,
            Self::History => OperationKind::History,
            Self::Apply(_) => OperationKind::Apply,
            Self::Submit(_) => OperationKind::Submit,
            Self::Task(_) => OperationKind::Task,
            Self::Approvals => OperationKind::Approvals,
            Self::Decide { .. } => OperationKind::Decide,
            Self::CreateBot { .. } => OperationKind::CreateBot,
            Self::CreateConversation { .. } => OperationKind::CreateConversation,
            Self::CreateSkill { .. } => OperationKind::CreateSkill,
            Self::ListSkills => OperationKind::ListSkills,
        }
    }
}

pub enum Reply {
    Health(Health),
    Catalog(Catalog),
    History(History),
    Selection(Selection),
    Task(Task),
    Approvals(Vec<Approval>),
    Decided,
    CreatedBot(CreatedBot),
    CreatedConversation(CreatedConversation),
    CreatedSkill(CreatedSkill),
    Skills(Vec<SkillSummary>),
}

pub struct Job {
    pub scope: Scope,
    pub credentials: Arc<Credentials>,
    pub operation: Operation,
}
pub struct Completion {
    pub scope: Scope,
    pub kind: OperationKind,
    pub result: Result<Reply, ApiError>,
}

pub struct Worker {
    sender: SyncSender<Job>,
    receiver: Receiver<Completion>,
    epoch: Arc<AtomicU64>,
}

impl Worker {
    pub fn start() -> std::io::Result<Self> {
        let (sender, jobs) = mpsc::sync_channel::<Job>(1);
        let (results, receiver) = mpsc::channel();
        let epoch = Arc::new(AtomicU64::new(0));
        let current = Arc::clone(&epoch);
        std::thread::Builder::new()
            .name("serabot-http".into())
            .spawn(move || {
                while let Ok(job) = jobs.recv() {
                    if current.load(Ordering::SeqCst) != job.scope.epoch {
                        continue;
                    }
                    let kind = job.operation.kind();
                    // Blocking client construction, I/O and destruction stay off UI.
                    let result = execute(&job, &current);
                    // Wipe/drop stale content here, not just when UI eventually polls.
                    if current.load(Ordering::SeqCst) != job.scope.epoch {
                        continue;
                    }
                    if results
                        .send(Completion {
                            scope: job.scope,
                            kind,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })?;
        Ok(Self {
            sender,
            receiver,
            epoch,
        })
    }

    pub fn set_epoch(&self, epoch: u64) {
        self.epoch.store(epoch, Ordering::SeqCst);
    }
    pub fn dispatch(&self, job: Job) -> Result<(), Box<TrySendError<Job>>> {
        // The error carries the unsent job. Box it so a fleet payload does not
        // inflate the Result error variant.
        self.sender.try_send(job).map_err(Box::new)
    }
    pub fn completion(&self) -> Option<Completion> {
        self.receiver.try_recv().ok()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
    }
}

fn execute(job: &Job, epoch: &AtomicU64) -> Result<Reply, ApiError> {
    let client = ApiClient::new(
        job.credentials.endpoint.clone(),
        job.credentials.token.clone(),
    )?;
    // A queued action invalidated by disconnect/conversation switch is never sent.
    // An already in-flight HTTP request cannot be recalled or safely replayed.
    if epoch.load(Ordering::SeqCst) != job.scope.epoch {
        return Err(ApiError::Transport);
    }
    let conversation = &job.scope.conversation;
    match &job.operation {
        Operation::Health => client.health().map(Reply::Health),
        Operation::Catalog => client.models(conversation).map(Reply::Catalog),
        Operation::History => client.history(conversation).map(Reply::History),
        Operation::Apply(selection) => client.select(conversation, selection).map(Reply::Selection),
        Operation::Submit(text) => client.submit(conversation, text).map(Reply::Task),
        Operation::Task(id) => client.task(id).map(Reply::Task),
        Operation::Approvals => client.approvals().map(Reply::Approvals),
        Operation::Decide { id, decision } => client
            .decide_approval(id, *decision)
            .map(|_| Reply::Decided),
        Operation::CreateBot {
            id,
            description,
            tools,
            skills,
        } => client
            .create_bot(id, description, tools, skills)
            .map(Reply::CreatedBot),
        Operation::CreateConversation {
            kind,
            slug,
            parent_id,
        } => client
            .create_conversation(*kind, slug, parent_id.as_deref())
            .map(Reply::CreatedConversation),
        Operation::CreateSkill {
            id,
            description,
            body,
        } => {
            let body = if body.is_empty() {
                None
            } else {
                Some(body.as_str())
            };
            client
                .create_skill(id, description, body)
                .map(Reply::CreatedSkill)
        }
        Operation::ListSkills => client.list_skills().map(Reply::Skills),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        time::Duration,
    };

    #[test]
    fn epoch_change_drops_late_results_and_never_sends_queued_old_mutations() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint =
            crate::validate_endpoint(&format!("http://{}", listener.local_addr().unwrap()))
                .unwrap();
        let (started, first_request) = mpsc::channel();
        let (release, wait_release) = mpsc::channel();
        let server = std::thread::spawn(move || {
            for index in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                assert!(
                    request.starts_with(b"GET /v1/health "),
                    "stale mutation was sent"
                );
                if index == 0 {
                    started.send(()).unwrap();
                    wait_release.recv_timeout(Duration::from_secs(5)).unwrap();
                }
                let body = r#"{"ok":true,"version":"synthetic"}"#;
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let credentials = Arc::new(Credentials {
            endpoint,
            token: SecretString::new("synthetic-token-for-worker-test-0000".into()),
        });
        let worker = Worker::start().unwrap();
        let old = Scope {
            epoch: 1,
            conversation: "old".into(),
        };
        let new = Scope {
            epoch: 2,
            conversation: "new".into(),
        };
        worker.set_epoch(old.epoch);
        assert!(
            worker
                .dispatch(Job {
                    scope: old.clone(),
                    credentials: Arc::clone(&credentials),
                    operation: Operation::Health
                })
                .is_ok()
        );
        first_request.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            worker
                .dispatch(Job {
                    scope: old,
                    credentials: Arc::clone(&credentials),
                    operation: Operation::Submit(Zeroizing::new("must never be sent".into()))
                })
                .is_ok()
        );
        worker.set_epoch(new.epoch);
        release.send(()).unwrap();
        // A blocking send is confined to this test, to put the new job after the
        // already-queued obsolete mutation without timing-based sleeps.
        assert!(
            worker
                .sender
                .send(Job {
                    scope: new.clone(),
                    credentials,
                    operation: Operation::Health
                })
                .is_ok()
        );
        let result = worker
            .receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert_eq!(result.scope, new);
        assert!(matches!(result.result, Ok(Reply::Health(_))));
        assert!(worker.completion().is_none());
        server.join().unwrap();
    }
}
