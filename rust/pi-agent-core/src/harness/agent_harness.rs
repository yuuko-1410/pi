//! Agent harness facade, port of `packages/agent/src/harness/agent-harness.ts`.
//!
//! The current JS implementation implements configuration accessors while
//! the operational entry points (prompt/compact/navigate/resume/steer/...)
//! are `unavailable()` placeholders; this port mirrors that shape.

use pi_ai::types::{AssistantMessage, DeferredHandle, ImageContent, Model, Usage};
use pi_ai::utils::retry::RetryPolicy;

use crate::harness::session_types::{
    BranchSummaryEntry, CompactionEntry, Entry, JsonValue, ProvisionedEntry, SessionError, SessionTree,
};
use crate::types::{AgentMessage, AgentTool, QueueMode};

#[derive(Clone, Debug, PartialEq)]
pub struct LaneBusy {
    pub lane: String,
    pub operation_id: String,
    pub operation_kind: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MissingIdentities {
    pub lane: String,
    pub tools: Vec<String>,
    pub models: Vec<String>,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NoActiveRun {
    pub lane: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NoActiveOperation {
    pub lane: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NothingToResume {
    pub lane: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InvalidMessage {
    pub lane: String,
    pub reason: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UnknownSkill {
    pub name: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UnknownTemplate {
    pub name: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UnknownTarget {
    pub target_id: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UnknownQueueItem {
    pub lane: String,
    pub entry_id: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LaneExists {
    pub lane: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InvalidLane {
    pub lane: String,
    pub reason: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NothingToCompact {
    pub lane: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Closed {
    pub message: String,
}

/// Rejection union for harness operations.
#[derive(Clone, Debug, PartialEq)]
pub enum HarnessRejection {
    LaneBusy(LaneBusy),
    MissingIdentities(MissingIdentities),
    NoActiveRun(NoActiveRun),
    NoActiveOperation(NoActiveOperation),
    NothingToResume(NothingToResume),
    InvalidMessage(InvalidMessage),
    UnknownSkill(UnknownSkill),
    UnknownTemplate(UnknownTemplate),
    UnknownTarget(UnknownTarget),
    UnknownQueueItem(UnknownQueueItem),
    LaneExists(LaneExists),
    InvalidLane(InvalidLane),
    NothingToCompact(NothingToCompact),
    Closed(Closed),
}

impl HarnessRejection {
    pub fn message(&self) -> &str {
        match self {
            HarnessRejection::LaneBusy(e) => &e.message,
            HarnessRejection::MissingIdentities(e) => &e.message,
            HarnessRejection::NoActiveRun(e) => &e.message,
            HarnessRejection::NoActiveOperation(e) => &e.message,
            HarnessRejection::NothingToResume(e) => &e.message,
            HarnessRejection::InvalidMessage(e) => &e.message,
            HarnessRejection::UnknownSkill(e) => &e.message,
            HarnessRejection::UnknownTemplate(e) => &e.message,
            HarnessRejection::UnknownTarget(e) => &e.message,
            HarnessRejection::UnknownQueueItem(e) => &e.message,
            HarnessRejection::LaneExists(e) => &e.message,
            HarnessRejection::InvalidLane(e) => &e.message,
            HarnessRejection::NothingToCompact(e) => &e.message,
            HarnessRejection::Closed(e) => &e.message,
        }
    }
}

/// Non-rejection harness failures.
#[derive(Clone, Debug, PartialEq)]
pub enum HarnessFault {
    Fault { message: String, cause: String },
    Closed,
    NotImplemented { operation: String },
    Session(SessionError),
}

/// Mirrors the JS `Result` shape for harness operations.
pub type HarnessResult<T> = Result<T, HarnessRejection>;
pub type RunResult = HarnessResult<RunOutcomeEnvelope>;
pub type CompactionResult = HarnessResult<CompactionOutcomeEnvelope>;
pub type NavigationResult = HarnessResult<NavigationOutcomeEnvelope>;
pub type QueueResult = HarnessResult<QueueResultValue>;
pub type CancelQueuedResult = HarnessResult<CancelQueuedOutcome>;
pub type ResumeResult = HarnessResult<ResumeOutcomeEnvelope>;
pub type CreateLaneResult = HarnessResult<CreateLaneValue>;

#[derive(Clone, Debug, PartialEq)]
pub struct RunOutcomeEnvelope {
    pub run_id: String,
    pub outcome: RunOutcome,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RunOutcome {
    Completed {
        leaf_id: String,
        final_entry_id: String,
        final_message: AssistantMessage,
    },
    Aborted {
        leaf_id: String,
        final_entry_id: String,
        final_message: AssistantMessage,
    },
    Failed {
        leaf_id: String,
        error: OperationError,
        final_entry_id: Option<String>,
        final_message: Option<AssistantMessage>,
    },
    Suspended {
        leaf_id: String,
        final_entry_id: String,
        deferred: DeferredHandle,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct OperationError {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompactionOutcomeEnvelope {
    pub run_id: String,
    pub outcome: CompactionOutcome,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CompactionOutcome {
    Completed {
        leaf_id: String,
        entry: CompactionEntry,
    },
    Declined { leaf_id: String },
    Aborted { leaf_id: String },
    Failed {
        leaf_id: String,
        error: OperationError,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct NavigationOutcomeEnvelope {
    pub run_id: String,
    pub outcome: NavigationOutcome,
}

#[derive(Clone, Debug, PartialEq)]
pub enum NavigationOutcome {
    Completed {
        new_leaf_id: Option<String>,
        summary_entry: Option<BranchSummaryEntry>,
    },
    Declined { leaf_id: Option<String> },
    Aborted { leaf_id: Option<String> },
    Failed {
        leaf_id: Option<String>,
        error: OperationError,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueueResultValue {
    pub entry_id: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CancelQueuedOutcome {
    Cancelled,
    AlreadyConsumed,
    AlreadyCleared,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ResumeOutcomeEnvelope {
    Run { run_id: String, outcome: RunOutcome },
    Compaction { run_id: String, outcome: CompactionOutcome },
    Navigation { run_id: String, outcome: NavigationOutcome },
}

pub struct CreateLaneValue;
impl Clone for CreateLaneValue {
    fn clone(&self) -> Self {
        CreateLaneValue
    }
}
impl PartialEq for CreateLaneValue {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}
impl std::fmt::Debug for CreateLaneValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CreateLaneValue")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NavigateOptions {
    pub summarize: Option<bool>,
    pub custom_instructions: Option<String>,
    pub label: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SuspendedOperation {
    pub lane: String,
    pub kind: String,
    pub id: String,
    pub started_at: f64,
    pub reason: String,
    pub prompt: Option<Vec<AgentMessage>>,
    pub deferred: Option<DeferredHandle>,
    pub aborting: Option<(Vec<AgentMessage>, Vec<AgentMessage>)>,
    pub missing: (Vec<String>, Vec<String>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct LaneInfo {
    pub name: String,
    pub leaf_id: Option<String>,
    pub operation: Option<LaneOperationInfo>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LaneOperationInfo {
    pub id: String,
    pub kind: String,
    pub status: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueuedItem {
    pub entry_id: String,
    pub message: AgentMessage,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LaneSnapshot {
    pub lane: String,
    pub transcript: Vec<Entry>,
    pub leaf_id: Option<String>,
    pub operation: Option<LaneOperationInfo>,
    pub queues: QueuesSnapshot,
    pub pending_writes: Vec<(String, ProvisionedEntry)>,
    pub faulted: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct QueuesSnapshot {
    pub steer: Vec<QueuedItem>,
    pub follow_up: Vec<QueuedItem>,
    pub next_run: Vec<QueuedItem>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionSnapshot {
    pub lanes: Vec<LaneInfoWithSuspended>,
    pub faulted: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LaneInfoWithSuspended {
    pub info: LaneInfo,
    pub suspended: Option<SuspendedOperation>,
}

pub type HookName = &'static str;

pub const HOOKS: [&str; 11] = [
    "before_run",
    "before_resume",
    "before_run_end",
    "transform_context",
    "before_request",
    "before_payload",
    "after_response",
    "before_tool",
    "after_tool",
    "before_compaction",
    "before_navigation",
];

#[derive(Clone, Debug, PartialEq)]
pub enum ActionInfo {
    AppendEntry { entry_type: String, entry_id: String },
    AppendRecord { record_type: String },
    MoveLane { to: Option<String> },
    SetFact { fact: String },
    TryFinishRun { outcome: String },
    FinishOperation { outcome: String },
    CommitFollowUp,
    ConsumeQueueItem { queue: String, entry_id: String },
    ApplyPendingWrite { entry_id: String },
    StreamAssistant { step: String, attempt: f64 },
    ExecuteTool { tool_call_id: String, tool_name: String },
    FetchDeferred { provider: String, id: String },
    CancelDeferred { provider: String, id: String },
    Hook { name: &'static str },
    Sleep { delay_ms: f64 },
}

/// Harness tool: agent tool plus replay semantics.
#[derive(Clone, Debug)]
pub struct HarnessTool {
    pub tool: AgentTool,
    pub replay: Option<String>,
}

pub type StreamOptions = pi_ai::types::SimpleStreamOptions;
pub type EntryProjector = Box<dyn Fn(&Entry) -> Vec<AgentMessage> + Send + Sync>;

/// Configuration for an AgentHarness.
pub struct AgentHarnessOptions {
    pub model: Model,
    pub thinking_level: Option<String>,
    pub active_tool_names: Option<Vec<String>>,
    pub tools: Option<Vec<HarnessTool>>,
    pub system_prompt: Option<String>,
    pub stream_options: Option<StreamOptions>,
    pub retry: Option<RetryPolicy>,
    pub steering_mode: Option<QueueMode>,
    pub follow_up_mode: Option<QueueMode>,
    pub tool_execution: Option<crate::types::ToolExecutionMode>,
}

/// The lane interface implemented by AgentHarness.
pub trait AgentLane: Send + Sync {
    fn name(&self) -> &str;
    fn get_leaf_id(&self) -> Result<Option<String>, HarnessFault>;
    fn get_model(&self) -> Result<Model, HarnessFault>;
    fn set_model(&self, model: Model) -> Result<(), HarnessFault>;
    fn get_thinking_level(&self) -> Result<String, HarnessFault>;
    fn set_thinking_level(&self, level: &str) -> Result<(), HarnessFault>;
    fn get_active_tools(&self) -> Result<Vec<String>, HarnessFault>;
    fn set_active_tools(&self, names: Vec<String>) -> Result<(), HarnessFault>;
    fn close(&self) -> Result<(), HarnessFault>;
}

pub struct WatchHandle<TSnapshot> {
    pub snapshot: TSnapshot,
}

/// The harness facade. Operational entry points return
/// `HarnessNotImplemented` until the operations port lands, mirroring the
/// current JS implementation.
pub struct AgentHarness<S: SessionTree> {
    pub name: String,
    pub session: S,
    model: Model,
    thinking_level: String,
    active_tool_names: Vec<String>,
    tools: Vec<HarnessTool>,
    stream_options: StreamOptions,
    retry_policy: RetryPolicy,
    steering_mode: QueueMode,
    follow_up_mode: QueueMode,
    closed: bool,
}

impl<S: SessionTree> AgentHarness<S> {
    pub fn new(options: AgentHarnessOptions, session: S) -> Self {
        let tools = options.tools.unwrap_or_default();
        let active_tool_names = options
            .active_tool_names
            .unwrap_or_else(|| tools.iter().map(|tool| tool.tool.tool.name.clone()).collect());
        Self {
            name: "main".to_string(),
            session,
            model: options.model,
            thinking_level: options.thinking_level.unwrap_or_else(|| "off".to_string()),
            active_tool_names,
            tools,
            stream_options: options.stream_options.unwrap_or_default(),
            retry_policy: options.retry.unwrap_or(RetryPolicy {
                enabled: false,
                max_retries: 0,
                base_delay_ms: 1000,
            }),
            steering_mode: options.steering_mode.unwrap_or(QueueMode::OneAtATime),
            follow_up_mode: options.follow_up_mode.unwrap_or(QueueMode::OneAtATime),
            closed: false,
        }
    }

    fn unavailable<T>(&self, operation: &str) -> Result<T, HarnessFault> {
        if self.closed {
            Err(HarnessFault::Closed)
        } else {
            Err(HarnessFault::NotImplemented {
                operation: operation.to_string(),
            })
        }
    }

    pub fn get_leaf_id(&self) -> Result<Option<String>, HarnessFault> {
        self.session
            .get_leaf_id()
            .map_err(HarnessFault::Session)
    }

    pub fn get_model(&self) -> Result<Model, HarnessFault> {
        Ok(self.model.clone())
    }

    pub fn set_model(&mut self, model: Model) -> Result<(), HarnessFault> {
        self.model = model;
        Ok(())
    }

    pub fn get_thinking_level(&self) -> Result<String, HarnessFault> {
        Ok(self.thinking_level.clone())
    }

    pub fn set_thinking_level(&mut self, level: &str) -> Result<(), HarnessFault> {
        self.thinking_level = level.to_string();
        Ok(())
    }

    pub fn get_active_tools(&self) -> Result<Vec<String>, HarnessFault> {
        Ok(self.active_tool_names.clone())
    }

    pub fn set_active_tools(&mut self, names: Vec<String>) -> Result<(), HarnessFault> {
        self.active_tool_names = names;
        Ok(())
    }

    pub fn get_tools(&self) -> Result<Vec<HarnessTool>, HarnessFault> {
        Ok(self.tools.clone())
    }

    pub fn set_tools(&mut self, tools: Vec<HarnessTool>, active_names: Option<Vec<String>>) -> Result<(), HarnessFault> {
        let names = active_names.unwrap_or_else(|| tools.iter().map(|tool| tool.tool.tool.name.clone()).collect());
        self.tools = tools;
        self.active_tool_names = names;
        Ok(())
    }

    pub fn get_stream_options(&self) -> Result<StreamOptions, HarnessFault> {
        Ok(self.stream_options.clone())
    }

    pub fn set_stream_options(&mut self, options: StreamOptions) -> Result<(), HarnessFault> {
        self.stream_options = options;
        Ok(())
    }

    pub fn get_retry_policy(&self) -> Result<RetryPolicy, HarnessFault> {
        Ok(self.retry_policy.clone())
    }

    pub fn set_retry_policy(&mut self, policy: RetryPolicy) -> Result<(), HarnessFault> {
        self.retry_policy = policy;
        Ok(())
    }

    pub fn get_steering_mode(&self) -> Result<QueueMode, HarnessFault> {
        Ok(self.steering_mode)
    }

    pub fn set_steering_mode(&mut self, mode: QueueMode) -> Result<(), HarnessFault> {
        self.steering_mode = mode;
        Ok(())
    }

    pub fn get_follow_up_mode(&self) -> Result<QueueMode, HarnessFault> {
        Ok(self.follow_up_mode)
    }

    pub fn set_follow_up_mode(&mut self, mode: QueueMode) -> Result<(), HarnessFault> {
        self.follow_up_mode = mode;
        Ok(())
    }

    pub fn close(&mut self) -> Result<(), HarnessFault> {
        self.closed = true;
        Ok(())
    }

    // Operational entry points: not implemented yet (mirrors JS).

    pub fn prompt(&self, _input: PromptInput) -> Result<RunResult, HarnessFault> {
        self.unavailable("prompt")
    }

    pub fn compact(&self, _options: Option<CompactionOptions>) -> Result<CompactionResult, HarnessFault> {
        self.unavailable("compact")
    }

    pub fn navigate_tree(
        &self,
        _target_id: Option<&str>,
        _options: Option<&NavigateOptions>,
    ) -> Result<NavigationResult, HarnessFault> {
        self.unavailable("navigateTree")
    }

    pub fn resume(&self) -> Result<ResumeResult, HarnessFault> {
        self.unavailable("resume")
    }

    pub fn abort(&self) -> Result<HarnessResult<AbortValue>, HarnessFault> {
        self.unavailable("abort")
    }

    pub fn steer(&self, _message: AgentMessage) -> Result<QueueResult, HarnessFault> {
        self.unavailable("steer")
    }

    pub fn follow_up(&self, _message: AgentMessage) -> Result<QueueResult, HarnessFault> {
        self.unavailable("followUp")
    }

    pub fn next_run(&self, _message: AgentMessage) -> Result<QueueResult, HarnessFault> {
        self.unavailable("nextRun")
    }

    pub fn cancel_queued(&self, _entry_id: &str) -> Result<CancelQueuedResult, HarnessFault> {
        self.unavailable("cancelQueued")
    }

    pub fn record_usage(
        &self,
        _usage: &Usage,
        _options: Option<(&str, Option<JsonValue>)>,
    ) -> Result<HarnessResult<()>, HarnessFault> {
        self.unavailable("recordUsage")
    }

    pub fn peek_action(&self) -> Result<Option<ActionInfo>, HarnessFault> {
        self.unavailable("peekAction")
    }

    pub fn execute_action(&self) -> Result<Option<ActionInfo>, HarnessFault> {
        self.unavailable("executeAction")
    }
}

#[derive(Clone, Debug)]
pub enum PromptInput {
    Text(String, Vec<ImageContent>),
    Messages(Vec<AgentMessage>),
}

#[derive(Clone, Debug, Default)]
pub struct CompactionOptions {
    pub custom_instructions: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AbortValue {
    pub run_id: String,
    pub steer: Vec<AgentMessage>,
    pub follow_up: Vec<AgentMessage>,
}
