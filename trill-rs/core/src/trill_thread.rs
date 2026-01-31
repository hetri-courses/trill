use crate::agent::AgentStatus;
use crate::codex::Codex;
use crate::error::Result as CodexResult;
use crate::protocol::Event;
use crate::protocol::Op;
use crate::protocol::Submission;
use trill_protocol::config_types::Personality;
use trill_protocol::openai_models::ReasoningEffort;
use trill_protocol::protocol::AskForApproval;
use trill_protocol::protocol::SandboxPolicy;
use trill_protocol::protocol::SessionSource;
use std::path::PathBuf;
use tokio::sync::watch;

use crate::state_db::StateDbHandle;

#[derive(Clone, Debug)]
pub struct ThreadConfigSnapshot {
    pub model: String,
    pub model_provider_id: String,
    pub approval_policy: AskForApproval,
    pub sandbox_policy: SandboxPolicy,
    pub cwd: PathBuf,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub personality: Option<Personality>,
    pub session_source: SessionSource,
}

pub struct TrillThread {
    codex: Codex,
    rollout_path: Option<PathBuf>,
}

/// Conduit for the bidirectional stream of messages that compose a thread
/// (formerly called a conversation) in Codex.
impl TrillThread {
    pub(crate) fn new(codex: Codex, rollout_path: Option<PathBuf>) -> Self {
        Self {
            codex,
            rollout_path,
        }
    }

    pub async fn submit(&self, op: Op) -> CodexResult<String> {
        self.trill.submit(op).await
    }

    /// Use sparingly: this is intended to be removed soon.
    pub async fn submit_with_id(&self, sub: Submission) -> CodexResult<()> {
        self.trill.submit_with_id(sub).await
    }

    pub async fn next_event(&self) -> CodexResult<Event> {
        self.trill.next_event().await
    }

    pub async fn agent_status(&self) -> AgentStatus {
        self.trill.agent_status().await
    }

    pub(crate) fn subscribe_status(&self) -> watch::Receiver<AgentStatus> {
        self.trill.agent_status.clone()
    }

    pub fn rollout_path(&self) -> Option<PathBuf> {
        self.rollout_path.clone()
    }

    pub fn state_db(&self) -> Option<StateDbHandle> {
        self.trill.state_db()
    }

    pub async fn config_snapshot(&self) -> ThreadConfigSnapshot {
        self.trill.thread_config_snapshot().await
    }
}
