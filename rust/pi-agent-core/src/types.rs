//! Agent types, port of `packages/agent/src/types.ts`.
//!
//! Language mapping: JS async hooks become synchronous closures (the Rust
//! agent loop is synchronous; the underlying provider streams run on worker
//! threads). `AgentMessage` is the ai `Message` plus custom app messages
//! (represented as a marker trait here; apps extend with their own types).

use pi_ai::types::{
    AssistantMessage, AssistantMessageEvent, Context, Message, Model, SimpleStreamOptions, Tool,
    ToolResultMessage, Usage,
};

/// Stream function used by the agent loop. Must not throw for request/model
/// failures; failures are encoded in the returned stream.
pub type StreamFn = dyn Fn(&Model, &Context, Option<&SimpleStreamOptions>) -> pi_ai::event_stream::AssistantMessageEventStream
    + Send
    + Sync;

/// Configuration for how tool calls from a single assistant message are
/// executed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolExecutionMode {
    Sequential,
    Parallel,
}

/// Controls how many queued user messages are injected at a queue drain
/// point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueMode {
    All,
    OneAtATime,
}

/// A single tool call content block emitted by an assistant message.
pub type AgentToolCall = pi_ai::types::ToolCall;

/// Result returned from `before_tool_call`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BeforeToolCallResult {
    pub block: Option<bool>,
    pub reason: Option<String>,
    /// Hint that the agent should stop after the current tool batch when
    /// this call is blocked.
    pub terminate: Option<bool>,
}

/// Partial override returned from `after_tool_call`. Merge semantics are
/// field-by-field; omitted fields keep the original values.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AfterToolCallResult {
    pub content: Option<Vec<pi_ai::types::Content>>,
    pub details: Option<pi_ai::types::JsonValue>,
    pub is_error: Option<bool>,
    pub usage: Option<Usage>,
    pub terminate: Option<bool>,
}

/// Context passed to `before_tool_call`.
pub struct BeforeToolCallContext<'a> {
    pub assistant_message: &'a AssistantMessage,
    pub tool_call: &'a AgentToolCall,
    pub args: &'a pi_ai::types::JsonValue,
    pub context: &'a AgentContext,
}

/// Context passed to `after_tool_call`.
pub struct AfterToolCallContext<'a> {
    pub assistant_message: &'a AssistantMessage,
    pub tool_call: &'a AgentToolCall,
    pub args: &'a pi_ai::types::JsonValue,
    pub result: &'a AgentToolResult,
    pub is_error: bool,
    pub context: &'a AgentContext,
}

/// Context passed to `should_stop_after_turn`.
pub struct ShouldStopAfterTurnContext<'a> {
    pub message: &'a AssistantMessage,
    pub tool_results: &'a [ToolResultMessage],
    pub context: &'a AgentContext,
    pub new_messages: &'a [AgentMessage],
}

/// Replacement runtime state used before starting another provider request.
#[derive(Clone, Debug, Default)]
pub struct AgentLoopTurnUpdate {
    pub context: Option<AgentContext>,
    pub model: Option<Model>,
    pub thinking_level: Option<String>,
}

/// Thinking/reasoning level for models that support it.
pub type ThinkingLevel = String;

/// Extensible marker for custom app messages; apps implement this trait.
pub trait CustomAgentMessage: std::fmt::Debug + Send + Sync {}

/// AgentMessage: LLM messages plus custom app messages. Custom messages are
/// compared by identity (Arc pointer), matching JS object identity.
#[derive(Clone)]
pub enum AgentMessage {
    Llm(Message),
    Custom(std::sync::Arc<dyn CustomAgentMessage>),
}

impl std::fmt::Debug for AgentMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentMessage::Llm(message) => write!(f, "Llm({message:?})"),
            AgentMessage::Custom(custom) => write!(f, "Custom({:?})", custom.as_ref()),
        }
    }
}

impl PartialEq for AgentMessage {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (AgentMessage::Llm(a), AgentMessage::Llm(b)) => a == b,
            (AgentMessage::Custom(a), AgentMessage::Custom(b)) => std::sync::Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl AgentMessage {
    pub fn role(&self) -> &'static str {
        match self {
            AgentMessage::Llm(message) => match message {
                Message::User(_) => "user",
                Message::Assistant(_) => "assistant",
                Message::ToolResult(_) => "toolResult",
            },
            AgentMessage::Custom(_) => "custom",
        }
    }
}

impl From<Message> for AgentMessage {
    fn from(message: Message) -> Self {
        AgentMessage::Llm(message)
    }
}

/// Tool definition used by the agent runtime. Manual Clone/Debug because the
/// execute closure is an Arc.
#[derive(Clone)]
pub struct AgentTool {
    pub tool: Tool,
    /// Human-readable label for UI display.
    pub label: String,
    /// Execute the tool call. Errors are returned as `Err` (the loop encodes
    /// them into error tool results).
    pub execute: Option<std::sync::Arc<dyn Fn(&str, &pi_ai::types::JsonValue, Option<&AgentToolUpdate>) -> Result<AgentToolResult, String> + Send + Sync>>,
    /// Per-tool execution mode override.
    pub execution_mode: Option<ToolExecutionMode>,
}

impl std::fmt::Debug for AgentTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentTool")
            .field("tool", &self.tool)
            .field("label", &self.label)
            .field("execute", &self.execute.is_some())
            .field("execution_mode", &self.execution_mode)
            .finish()
    }
}

/// Result produced by a tool.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentToolResult {
    /// Text or image content returned to the model.
    pub content: Vec<pi_ai::types::Content>,
    /// Arbitrary structured details for logs or UI rendering.
    pub details: pi_ai::types::JsonValue,
    /// Usage from the final tool execution itself.
    pub usage: Option<Usage>,
    /// Names of tools introduced by this result.
    pub added_tool_names: Option<Vec<String>>,
    /// Hint that the agent should stop after the current tool batch.
    pub terminate: Option<bool>,
}

/// Partial execution update pushed by tools.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentToolUpdate {
    pub partial_result: AgentToolResult,
}

/// Context snapshot passed into the low-level agent loop.
#[derive(Clone, Debug, Default)]
pub struct AgentContext {
    /// System prompt included with the request.
    pub system_prompt: String,
    /// Transcript visible to the model.
    pub messages: Vec<AgentMessage>,
    /// Tools available for this run.
    pub tools: Option<Vec<AgentTool>>,
}

impl PartialEq for AgentContext {
    fn eq(&self, other: &Self) -> bool {
        self.system_prompt == other.system_prompt && self.messages == other.messages
    }
}

impl AgentContext {
    /// The LLM context derived from this agent context.
    pub fn to_llm_context(&self, convert: &dyn Fn(&[AgentMessage]) -> Vec<Message>) -> Context {
        Context {
            system_prompt: Some(self.system_prompt.clone()),
            messages: convert(&self.messages),
            tools: self
                .tools
                .as_ref()
                .map(|tools| tools.iter().map(|tool| tool.tool.clone()).collect()),
        }
    }
}

/// Events emitted by the Agent for UI updates.
#[derive(Clone, Debug, PartialEq)]
pub enum AgentEvent {
    AgentStart,
    AgentEnd { messages: Vec<AgentMessage> },
    TurnStart,
    TurnEnd {
        message: AgentMessage,
        tool_results: Vec<ToolResultMessage>,
    },
    MessageStart { message: AgentMessage },
    MessageUpdate {
        message: AgentMessage,
        assistant_message_event: AssistantMessageEvent,
    },
    MessageEnd { message: AgentMessage },
    ToolExecutionStart {
        tool_call_id: String,
        tool_name: String,
        args: pi_ai::types::JsonValue,
    },
    ToolExecutionUpdate {
        tool_call_id: String,
        tool_name: String,
        args: pi_ai::types::JsonValue,
        partial_result: AgentToolResult,
    },
    ToolExecutionEnd {
        tool_call_id: String,
        tool_name: String,
        result: AgentToolResult,
        is_error: bool,
    },
}
