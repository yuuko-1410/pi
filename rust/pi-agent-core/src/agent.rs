//! Stateful Agent wrapper, port of `packages/agent/src/agent.ts`.
//!
//! Synchronous mapping: `prompt`/`continue` run the loop to completion on the
//! calling thread; subscribed listeners are invoked synchronously per event.
//! `abort` marks a cancellation token observed by the loop. Queued steering
//! and follow-up messages use the same drain modes as JS.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use pi_ai::event_stream::AssistantMessageEventStream;
use pi_ai::types::{Context, Message, Model, SimpleStreamOptions, Usage, UsageCost};
use pi_ai::utils::abort::CancellationToken;

use crate::agent_loop::{run_agent_loop, run_agent_loop_continue, AgentLoopConfig};
use crate::types::{
    AfterToolCallContext, AfterToolCallResult, AgentContext, AgentEvent, AgentMessage, AgentTool,
    BeforeToolCallContext, BeforeToolCallResult, QueueMode, ToolExecutionMode,
};

fn default_convert_to_llm(messages: &[AgentMessage]) -> Vec<Message> {
    messages
        .iter()
        .filter_map(|message| match message {
            AgentMessage::Llm(message) => match message {
                Message::User(_) | Message::Assistant(_) | Message::ToolResult(_) => Some(message.clone()),
            },
            AgentMessage::Custom(_) => None,
        })
        .collect()
}

const EMPTY_USAGE: Usage = Usage {
    input: 0.0,
    output: 0.0,
    cache_read: 0.0,
    cache_write: 0.0,
    cache_write_1h: None,
    reasoning: None,
    total_tokens: 0.0,
    cost: UsageCost {
        input: 0.0,
        output: 0.0,
        cache_read: 0.0,
        cache_write: 0.0,
        total: 0.0,
    },
};

/// Mutable agent state owned by the Agent.
#[derive(Clone, Debug)]
pub struct MutableAgentState {
    pub system_prompt: String,
    pub model: Model,
    pub thinking_level: String,
    pub tools: Vec<AgentTool>,
    pub messages: Vec<AgentMessage>,
    pub is_streaming: bool,
    pub streaming_message: Option<AgentMessage>,
    pub pending_tool_calls: std::collections::HashSet<String>,
    pub error_message: Option<String>,
}

fn default_model() -> Model {
    Model {
        id: "unknown".to_string(),
        name: "unknown".to_string(),
        api: "unknown".to_string(),
        provider: "unknown".to_string(),
        base_url: String::new(),
        reasoning: false,
        thinking_level_map: None,
        input: Vec::new(),
        cost: pi_ai::types::ModelCost {
            rates: pi_ai::types::ModelCostRates {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            tiers: None,
        },
        context_window: 0.0,
        max_tokens: 0.0,
        sampling_params: None,
        headers: None,
        compat: None,
    }
}

impl Default for MutableAgentState {
    fn default() -> Self {
        Self {
            system_prompt: String::new(),
            model: default_model(),
            thinking_level: "off".to_string(),
            tools: Vec::new(),
            messages: Vec::new(),
            is_streaming: false,
            streaming_message: None,
            pending_tool_calls: std::collections::HashSet::new(),
            error_message: None,
        }
    }
}

/// Options for constructing an Agent.
pub struct AgentOptions {
    pub initial_state: Option<MutableAgentState>,
    pub convert_to_llm: Option<Arc<dyn Fn(&[AgentMessage]) -> Vec<Message> + Send + Sync>>,
    pub stream_fn: Arc<dyn Fn(&Model, &Context, Option<&SimpleStreamOptions>) -> AssistantMessageEventStream + Send + Sync>,
    pub before_tool_call: Option<Arc<dyn Fn(&BeforeToolCallContext) -> Option<BeforeToolCallResult> + Send + Sync>>,
    pub after_tool_call: Option<Arc<dyn Fn(&AfterToolCallContext) -> Option<AfterToolCallResult> + Send + Sync>>,
    pub steering_mode: Option<QueueMode>,
    pub follow_up_mode: Option<QueueMode>,
    pub tool_execution: Option<ToolExecutionMode>,
}

impl Default for AgentOptions {
    fn default() -> Self {
        Self {
            initial_state: None,
            convert_to_llm: None,
            stream_fn: Arc::new(|_model, _context, _options| panic!("no stream fn configured")),
            before_tool_call: None,
            after_tool_call: None,
            steering_mode: None,
            follow_up_mode: None,
            tool_execution: None,
        }
    }
}

/// A queue of pending messages with a drain mode.
#[derive(Clone, Debug)]
pub struct PendingMessageQueue {
    messages: Vec<AgentMessage>,
    pub mode: QueueMode,
}

impl PendingMessageQueue {
    pub fn new(mode: QueueMode) -> Self {
        Self {
            messages: Vec::new(),
            mode,
        }
    }

    pub fn enqueue(&mut self, message: AgentMessage) {
        self.messages.push(message);
    }

    pub fn has_items(&self) -> bool {
        !self.messages.is_empty()
    }

    pub fn drain(&mut self) -> Vec<AgentMessage> {
        if self.mode == QueueMode::All {
            return std::mem::take(&mut self.messages);
        }
        if self.messages.is_empty() {
            return Vec::new();
        }
        let first = self.messages.remove(0);
        vec![first]
    }

    pub fn clear(&mut self) {
        self.messages.clear();
    }
}

/// Stateful wrapper around the low-level agent loop. All state is behind a
/// mutex so the event sink closures (which require Send + Sync) can share the
/// agent with the loop running on the calling thread.
pub struct Agent {
    state: Mutex<MutableAgentState>,
    listeners: Mutex<Vec<Box<dyn Fn(&AgentEvent) + Send + Sync>>>,
    steering_queue: Mutex<PendingMessageQueue>,
    follow_up_queue: Mutex<PendingMessageQueue>,
    pub convert_to_llm: Arc<dyn Fn(&[AgentMessage]) -> Vec<Message> + Send + Sync>,
    pub stream_fn: Arc<dyn Fn(&Model, &Context, Option<&SimpleStreamOptions>) -> AssistantMessageEventStream + Send + Sync>,
    pub before_tool_call: Option<Arc<dyn Fn(&BeforeToolCallContext) -> Option<BeforeToolCallResult> + Send + Sync>>,
    pub after_tool_call: Option<Arc<dyn Fn(&AfterToolCallContext) -> Option<AfterToolCallResult> + Send + Sync>>,
    pub tool_execution: ToolExecutionMode,
    token: Mutex<Arc<CancellationToken>>,
    active: AtomicBool,
}

impl Agent {
    pub fn new(options: AgentOptions) -> Self {
        Self {
            state: Mutex::new(options.initial_state.unwrap_or_default()),
            listeners: Mutex::new(Vec::new()),
            steering_queue: Mutex::new(PendingMessageQueue::new(
                options.steering_mode.unwrap_or(QueueMode::OneAtATime),
            )),
            follow_up_queue: Mutex::new(PendingMessageQueue::new(
                options.follow_up_mode.unwrap_or(QueueMode::OneAtATime),
            )),
            convert_to_llm: options.convert_to_llm.unwrap_or_else(|| Arc::new(default_convert_to_llm)),
            stream_fn: options.stream_fn,
            before_tool_call: options.before_tool_call,
            after_tool_call: options.after_tool_call,
            tool_execution: options.tool_execution.unwrap_or(ToolExecutionMode::Parallel),
            token: Mutex::new(Arc::new(CancellationToken::new())),
            active: AtomicBool::new(false),
        }
    }

    /// Subscribe to agent lifecycle events. Returns a subscription id.
    pub fn subscribe(&mut self, listener: Box<dyn Fn(&AgentEvent) + Send + Sync>) -> usize {
        let mut listeners = self.listeners.lock().unwrap();
        listeners.push(listener);
        listeners.len() - 1
    }

    pub fn unsubscribe(&mut self, id: usize) {
        let mut listeners = self.listeners.lock().unwrap();
        if id < listeners.len() {
            let _ = listeners.remove(id);
        }
    }

    /// Snapshot of the current agent state.
    pub fn state(&self) -> MutableAgentState {
        self.state.lock().unwrap().clone()
    }

    pub fn state_mut(&mut self) -> std::sync::MutexGuard<'_, MutableAgentState> {
        self.state.lock().unwrap()
    }

    /// Queue a message to be injected after the current assistant turn
    /// finishes.
    pub fn steer(&self, message: AgentMessage) {
        self.steering_queue.lock().unwrap().enqueue(message);
    }

    /// Queue a message to run only after the agent would otherwise stop.
    pub fn follow_up(&self, message: AgentMessage) {
        self.follow_up_queue.lock().unwrap().enqueue(message);
    }

    pub fn clear_steering_queue(&self) {
        self.steering_queue.lock().unwrap().clear();
    }

    pub fn clear_follow_up_queue(&self) {
        self.follow_up_queue.lock().unwrap().clear();
    }

    pub fn has_queued_messages(&self) -> bool {
        self.steering_queue.lock().unwrap().has_items()
            || self.follow_up_queue.lock().unwrap().has_items()
    }

    /// Abort the current run, if one is active.
    pub fn abort(&self) {
        self.token.lock().unwrap().abort();
    }

    /// Reset transcript state, runtime state, and queued messages.
    pub fn reset(&self) -> Result<(), String> {
        if self.active.load(Ordering::SeqCst) {
            return Err("Agent is already processing. Wait for completion before resetting.".to_string());
        }
        let mut state = self.state.lock().unwrap();
        state.messages.clear();
        state.is_streaming = false;
        state.streaming_message = None;
        state.pending_tool_calls.clear();
        state.error_message = None;
        self.follow_up_queue.lock().unwrap().clear();
        self.steering_queue.lock().unwrap().clear();
        Ok(())
    }

    /// Start a new prompt. Runs the loop to completion on the calling thread.
    pub fn prompt(&self, messages: Vec<AgentMessage>) -> Result<(), String> {
        if self.active.swap(true, Ordering::SeqCst) {
            return Err(
                "Agent is already processing a prompt. Use steer() or followUp() to queue messages, or wait for completion."
                    .to_string(),
            );
        }
        self.run_prompt_messages(messages, false);
        Ok(())
    }

    /// Continue from the current transcript. The last message must be a user
    /// or tool-result message.
    pub fn continue_(&self) -> Result<(), String> {
        if self.active.swap(true, Ordering::SeqCst) {
            return Err("Agent is already processing. Wait for completion before continuing.".to_string());
        }
        let last_message = self.state.lock().unwrap().messages.last().cloned();
        let Some(last_message) = last_message else {
            self.active.store(false, Ordering::SeqCst);
            return Err("No messages to continue from".to_string());
        };
        if last_message.role() == "assistant" {
            let queued_steering = self.steering_queue.lock().unwrap().drain();
            if !queued_steering.is_empty() {
                self.run_prompt_messages(queued_steering, true);
                return Ok(());
            }
            let queued_follow_ups = self.follow_up_queue.lock().unwrap().drain();
            if !queued_follow_ups.is_empty() {
                self.run_prompt_messages(queued_follow_ups, false);
                return Ok(());
            }
            self.active.store(false, Ordering::SeqCst);
            return Err("Cannot continue from message role: assistant".to_string());
        }
        self.run_continuation();
        Ok(())
    }

    fn create_context_snapshot(&self) -> AgentContext {
        let state = self.state.lock().unwrap();
        AgentContext {
            system_prompt: state.system_prompt.clone(),
            messages: state.messages.clone(),
            tools: if state.tools.is_empty() {
                None
            } else {
                Some(state.tools.clone())
            },
        }
    }

    fn create_loop_config(&self, skip_initial_steering_poll: bool) -> AgentLoopConfig {
        let skip_initial_steering_poll = std::sync::atomic::AtomicBool::new(skip_initial_steering_poll);
        let steering_queue = std::sync::Mutex::new(self.steering_queue.lock().unwrap().clone());
        let follow_up_queue = std::sync::Mutex::new(self.follow_up_queue.lock().unwrap().clone());
        let before_tool_call = self.before_tool_call.clone();
        let after_tool_call = self.after_tool_call.clone();
        let tool_execution = self.tool_execution;
        let convert_to_llm = self.convert_to_llm.clone();
        let state = self.state.lock().unwrap();
        let thinking_level = if state.thinking_level == "off" {
            None
        } else {
            Some(state.thinking_level.clone())
        };
        let model = state.model.clone();
        drop(state);
        AgentLoopConfig {
            model,
            reasoning: thinking_level,
            api_key: None,
            convert_to_llm,
            transform_context: None,
            get_api_key: None,
            should_stop_after_turn: None,
            prepare_next_turn: None,
            get_steering_messages: Some(Arc::new(move || {
                if skip_initial_steering_poll.swap(false, Ordering::SeqCst) {
                    return Vec::new();
                }
                steering_queue.lock().unwrap().drain()
            })),
            get_follow_up_messages: Some(Arc::new(move || follow_up_queue.lock().unwrap().drain())),
            tool_execution,
            before_tool_call,
            after_tool_call,
        }
    }

    fn run_prompt_messages(&self, messages: Vec<AgentMessage>, skip_initial_steering_poll: bool) {
        self.run_with_lifecycle(|agent| {
            let stream_fn = agent.stream_fn.clone();
            let context = agent.create_context_snapshot();
            let config = agent.create_loop_config(skip_initial_steering_poll);
            run_agent_loop(
                messages,
                context,
                &config,
                Some(agent.token.lock().unwrap().as_ref()),
                stream_fn.as_ref(),
                &|event| agent.process_events(event),
            );
        });
    }

    fn run_continuation(&self) {
        self.run_with_lifecycle(|agent| {
            let stream_fn = agent.stream_fn.clone();
            let context = agent.create_context_snapshot();
            let config = agent.create_loop_config(false);
            run_agent_loop_continue(
                context,
                &config,
                Some(agent.token.lock().unwrap().as_ref()),
                stream_fn.as_ref(),
                &|event| agent.process_events(event),
            );
        });
    }

    fn run_with_lifecycle(&self, executor: impl FnOnce(&Agent)) {
        *self.token.lock().unwrap() = Arc::new(CancellationToken::new());
        {
            let mut state = self.state.lock().unwrap();
            state.is_streaming = true;
            state.streaming_message = None;
            state.error_message = None;
        }

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            executor(self);
        }));
        if let Err(panic) = result {
            let message = panic_message(&panic);
            self.handle_run_failure(message, false);
        }
        self.finish_run();
    }

    fn handle_run_failure(&self, message: String, aborted: bool) {
        let state = self.state.lock().unwrap();
        let failure_message = AgentMessage::Llm(Message::Assistant(pi_ai::types::AssistantMessage {
            content: vec![pi_ai::types::Content::Text(pi_ai::types::TextContent {
                text: String::new(),
                text_signature: None,
            })],
            api: state.model.api.clone(),
            provider: state.model.provider.clone(),
            model: state.model.id.clone(),
            response_model: None,
            response_id: None,
            usage: EMPTY_USAGE,
            stop_reason: if aborted {
                pi_ai::types::StopReason::Aborted
            } else {
                pi_ai::types::StopReason::Error
            },
            deferred: None,
            error_message: Some(message),
            raw_stop_reason: None,
            end_turn: None,
            timestamp: now_ms(),
        }));
        drop(state);
        self.process_events(&AgentEvent::MessageStart {
            message: failure_message.clone(),
        });
        self.process_events(&AgentEvent::MessageEnd {
            message: failure_message.clone(),
        });
        self.process_events(&AgentEvent::TurnEnd {
            message: failure_message.clone(),
            tool_results: vec![],
        });
        self.process_events(&AgentEvent::AgentEnd {
            messages: vec![failure_message],
        });
    }

    fn finish_run(&self) {
        let mut state = self.state.lock().unwrap();
        state.is_streaming = false;
        state.streaming_message = None;
        state.pending_tool_calls.clear();
        drop(state);
        self.active.store(false, Ordering::SeqCst);
    }

    /// Reduce internal state for a loop event, then notify listeners.
    fn process_events(&self, event: &AgentEvent) {
        let mut state = self.state.lock().unwrap();
        match event {
            AgentEvent::MessageStart { message } => {
                state.streaming_message = Some(message.clone());
            }
            AgentEvent::MessageUpdate { message, .. } => {
                state.streaming_message = Some(message.clone());
            }
            AgentEvent::MessageEnd { message } => {
                state.streaming_message = None;
                state.messages.push(message.clone());
            }
            AgentEvent::ToolExecutionStart { tool_call_id, .. } => {
                state.pending_tool_calls.insert(tool_call_id.clone());
            }
            AgentEvent::ToolExecutionEnd { tool_call_id, .. } => {
                state.pending_tool_calls.remove(tool_call_id);
            }
            AgentEvent::TurnEnd { message, .. } => {
                if let AgentMessage::Llm(Message::Assistant(assistant)) = message {
                    if let Some(error_message) = &assistant.error_message {
                        state.error_message = Some(error_message.clone());
                    }
                }
            }
            AgentEvent::AgentEnd { .. } => {
                state.streaming_message = None;
            }
            AgentEvent::AgentStart | AgentEvent::TurnStart | AgentEvent::ToolExecutionUpdate { .. } => {}
        }
        drop(state);
        let listeners = self.listeners.lock().unwrap();
        for listener in listeners.iter() {
            listener(event);
        }
    }
}

fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = panic.downcast_ref::<&str>() {
        message.to_string()
    } else if let Some(message) = panic.downcast_ref::<String>() {
        message.clone()
    } else {
        "Agent loop panicked".to_string()
    }
}

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as f64
}
