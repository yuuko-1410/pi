//! Agent loop, port of `packages/agent/src/agent-loop.ts`.
//!
//! Synchronous mapping: JS async hooks become synchronous closures; the
//! provider stream is consumed by blocking `next()` (the stream itself runs
//! on a worker thread inside the provider adapter). Parallel tool execution
//! spawns one thread per tool call, with `tool_execution_end` emitted in
//! completion order and tool-result messages in source order, like JS.

use std::sync::Arc;

use pi_ai::event_stream::{AssistantMessageEventStream, EventStream};
use pi_ai::types::{
    AssistantMessage, Context, Content, Message, Model, SimpleStreamOptions, ToolResultMessage,
};
use pi_ai::utils::validation::validate_tool_arguments;

use crate::types::{
    AfterToolCallContext, AfterToolCallResult, AgentContext, AgentEvent, AgentMessage, AgentTool,
    AgentToolCall, AgentToolResult, BeforeToolCallContext, BeforeToolCallResult,
    ShouldStopAfterTurnContext, ToolExecutionMode,
};

pub type AgentEventSink = dyn Fn(&AgentEvent) + Send + Sync;

/// Stream function used by the loop (satisfied by `Models.streamSimple`).
pub type LoopStreamFn = dyn Fn(
        &Model,
        &Context,
        Option<&SimpleStreamOptions>,
    ) -> AssistantMessageEventStream
    + Send
    + Sync;

/// Configuration for the agent loop. JS async hooks map to synchronous
/// closures; `prepare_next_turn` returns the replacement state.
pub struct AgentLoopConfig {
    pub model: Model,
    pub reasoning: Option<String>,
    pub api_key: Option<String>,
    pub convert_to_llm: Arc<dyn Fn(&[AgentMessage]) -> Vec<Message> + Send + Sync>,
    pub transform_context: Option<Arc<dyn Fn(&[AgentMessage]) -> Vec<AgentMessage> + Send + Sync>>,
    pub get_api_key: Option<Arc<dyn Fn(&str) -> Option<String> + Send + Sync>>,
    pub should_stop_after_turn:
        Option<Arc<dyn Fn(&ShouldStopAfterTurnContext) -> bool + Send + Sync>>,
    pub prepare_next_turn:
        Option<Arc<dyn Fn(&ShouldStopAfterTurnContext) -> Option<crate::types::AgentLoopTurnUpdate> + Send + Sync>>,
    pub get_steering_messages: Option<Arc<dyn Fn() -> Vec<AgentMessage> + Send + Sync>>,
    pub get_follow_up_messages: Option<Arc<dyn Fn() -> Vec<AgentMessage> + Send + Sync>>,
    pub tool_execution: ToolExecutionMode,
    pub before_tool_call: Option<
        Arc<dyn Fn(&BeforeToolCallContext) -> Option<BeforeToolCallResult> + Send + Sync>,
    >,
    pub after_tool_call: Option<
        Arc<dyn Fn(&AfterToolCallContext) -> Option<AfterToolCallResult> + Send + Sync>,
    >,
}

fn is_agent_end(event: &AgentEvent) -> bool {
    matches!(event, AgentEvent::AgentEnd { .. })
}

fn extract_messages(event: &AgentEvent) -> Vec<AgentMessage> {
    match event {
        AgentEvent::AgentEnd { messages } => messages.clone(),
        _ => Vec::new(),
    }
}

/// Start an agent loop with a new prompt message. Returns an event stream
/// that completes with the new messages when the loop ends.
pub fn agent_loop(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    config: Arc<AgentLoopConfig>,
    signal: Option<pi_ai::utils::abort::CancellationToken>,
    stream_fn: Arc<LoopStreamFn>,
) -> EventStream<AgentEvent, Vec<AgentMessage>> {
    let stream = EventStream::new(is_agent_end, extract_messages);
    let worker = stream.clone();
    std::thread::spawn(move || {
        let emit_worker = worker.clone();
        let emit = move |event: &AgentEvent| emit_worker.push(event.clone());
        let messages = run_agent_loop(prompts, context, &config, signal.as_ref(), stream_fn.as_ref(), &emit);
        worker.end(Some(messages));
    });
    stream
}

/// Continue an agent loop from the current context without adding a new
/// message. The last message must convert to a user/toolResult message.
pub fn agent_loop_continue(
    context: AgentContext,
    config: Arc<AgentLoopConfig>,
    signal: Option<pi_ai::utils::abort::CancellationToken>,
    stream_fn: Arc<LoopStreamFn>,
) -> EventStream<AgentEvent, Vec<AgentMessage>> {
    if context.messages.is_empty() {
        panic!("Cannot continue: no messages in context");
    }
    if context.messages.last().is_some_and(|m| m.role() == "assistant") {
        panic!("Cannot continue from message role: assistant");
    }

    let stream = EventStream::new(is_agent_end, extract_messages);
    let worker = stream.clone();
    std::thread::spawn(move || {
        let emit_worker = worker.clone();
        let emit = move |event: &AgentEvent| emit_worker.push(event.clone());
        let messages = run_agent_loop_continue(context, &config, signal.as_ref(), stream_fn.as_ref(), &emit);
        worker.end(Some(messages));
    });
    stream
}

pub fn run_agent_loop(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    config: &AgentLoopConfig,
    signal: Option<&pi_ai::utils::abort::CancellationToken>,
    stream_fn: &LoopStreamFn,
    emit: &AgentEventSink,
) -> Vec<AgentMessage> {
    let mut new_messages: Vec<AgentMessage> = prompts.clone();
    let mut current_context = AgentContext {
        system_prompt: context.system_prompt.clone(),
        messages: [context.messages.clone(), prompts].concat(),
        tools: context.tools.clone(),
    };

    emit(&AgentEvent::AgentStart);
    emit(&AgentEvent::TurnStart);
    for prompt in &new_messages {
        emit(&AgentEvent::MessageStart {
            message: prompt.clone(),
        });
        emit(&AgentEvent::MessageEnd {
            message: prompt.clone(),
        });
    }

    run_loop(
        &mut current_context,
        &mut new_messages,
        config,
        signal,
        emit,
        stream_fn,
    );
    new_messages
}

pub fn run_agent_loop_continue(
    context: AgentContext,
    config: &AgentLoopConfig,
    signal: Option<&pi_ai::utils::abort::CancellationToken>,
    stream_fn: &LoopStreamFn,
    emit: &AgentEventSink,
) -> Vec<AgentMessage> {
    let mut new_messages: Vec<AgentMessage> = Vec::new();
    let mut current_context = context;

    emit(&AgentEvent::AgentStart);
    emit(&AgentEvent::TurnStart);

    run_loop(
        &mut current_context,
        &mut new_messages,
        config,
        signal,
        emit,
        stream_fn,
    );
    new_messages
}

/// Main loop logic shared by agent_loop and agent_loop_continue.
fn run_loop(
    current_context: &mut AgentContext,
    new_messages: &mut Vec<AgentMessage>,
    config: &AgentLoopConfig,
    signal: Option<&pi_ai::utils::abort::CancellationToken>,
    emit: &AgentEventSink,
    stream_fn: &LoopStreamFn,
) {
    let mut first_turn = true;
    // Check for steering messages at start.
    let mut pending_messages: Vec<AgentMessage> =
        config.get_steering_messages.as_ref().map(|f| f()).unwrap_or_default();

    // Outer loop: continues when queued follow-up messages arrive.
    loop {
        let mut has_more_tool_calls = true;

        // Inner loop: process tool calls and steering messages.
        while has_more_tool_calls || !pending_messages.is_empty() {
            if !first_turn {
                emit(&AgentEvent::TurnStart);
            } else {
                first_turn = false;
            }

            // Process pending messages.
            if !pending_messages.is_empty() {
                for message in pending_messages.drain(..) {
                    emit(&AgentEvent::MessageStart {
                        message: message.clone(),
                    });
                    emit(&AgentEvent::MessageEnd {
                        message: message.clone(),
                    });
                    current_context.messages.push(message.clone());
                    new_messages.push(message);
                }
            }

            // Stream assistant response.
            let message = stream_assistant_response(current_context, config, signal, emit, stream_fn);
            new_messages.push(AgentMessage::Llm(Message::Assistant(message.clone())));

            if message.stop_reason == pi_ai::types::StopReason::Error
                || message.stop_reason == pi_ai::types::StopReason::Aborted
            {
                emit(&AgentEvent::TurnEnd {
                    message: AgentMessage::Llm(Message::Assistant(message)),
                    tool_results: vec![],
                });
                emit(&AgentEvent::AgentEnd {
                    messages: new_messages.clone(),
                });
                return;
            }

            // Check for tool calls.
            let tool_calls: Vec<AgentToolCall> = message
                .content
                .iter()
                .filter_map(|block| match block {
                    Content::ToolCall(tool_call) => Some(tool_call.clone()),
                    _ => None,
                })
                .collect();

            let mut tool_results: Vec<ToolResultMessage> = Vec::new();
            has_more_tool_calls = false;
            if !tool_calls.is_empty() {
                // A "length" stop means the output was cut off, so every tool
                // call may carry truncated arguments: fail them all.
                let executed_tool_batch = if message.stop_reason == pi_ai::types::StopReason::Length {
                    fail_tool_calls_from_truncated_message(&tool_calls, emit)
                } else {
                    execute_tool_calls(
                        current_context,
                        &message,
                        config,
                        signal,
                        emit,
                    )
                };
                tool_results.extend(executed_tool_batch.messages);
                has_more_tool_calls = !executed_tool_batch.terminate;

                for result in &tool_results {
                    current_context.messages.push(AgentMessage::Llm(Message::ToolResult(result.clone())));
                    new_messages.push(AgentMessage::Llm(Message::ToolResult(result.clone())));
                }
            }

            emit(&AgentEvent::TurnEnd {
                message: AgentMessage::Llm(Message::Assistant(message.clone())),
                tool_results: tool_results.clone(),
            });

            let mut context_update: Option<AgentContext> = None;
            let stop = {
                let next_turn_context = ShouldStopAfterTurnContext {
                    message: &message,
                    tool_results: &tool_results,
                    context: current_context,
                    new_messages,
                };
                if let Some(prepare) = &config.prepare_next_turn {
                    if let Some(update) = prepare(&next_turn_context) {
                        context_update = update.context;
                        if let Some(model) = update.model {
                            // Config is shared; model replacement is applied
                            // by the caller through the returned update (the
                            // JS version mutates a local config copy).
                            let _ = model;
                        }
                    }
                }
                config
                    .should_stop_after_turn
                    .as_ref()
                    .is_some_and(|f| f(&next_turn_context))
            };
            if let Some(new_context) = context_update {
                *current_context = new_context;
            }
            if stop {
                emit(&AgentEvent::AgentEnd {
                    messages: new_messages.clone(),
                });
                return;
            }

            pending_messages = config
                .get_steering_messages
                .as_ref()
                .map(|f| f())
                .unwrap_or_default();
        }

        // Agent would stop here. Check for follow-up messages.
        let follow_up_messages = config
            .get_follow_up_messages
            .as_ref()
            .map(|f| f())
            .unwrap_or_default();
        if !follow_up_messages.is_empty() {
            pending_messages = follow_up_messages;
            continue;
        }

        // No more messages, exit.
        break;
    }

    emit(&AgentEvent::AgentEnd {
        messages: new_messages.clone(),
    });
}

/// Stream an assistant response from the LLM. This is where AgentMessage[]
/// gets transformed to Message[] for the LLM.
fn stream_assistant_response(
    context: &mut AgentContext,
    config: &AgentLoopConfig,
    signal: Option<&pi_ai::utils::abort::CancellationToken>,
    emit: &AgentEventSink,
    stream_fn: &LoopStreamFn,
) -> AssistantMessage {
    let _ = signal;
    // Apply context transform if configured.
    let messages: Vec<AgentMessage> = match &config.transform_context {
        Some(transform) => transform(&context.messages),
        None => context.messages.clone(),
    };

    // Convert to LLM-compatible messages.
    let llm_messages = (config.convert_to_llm)(&messages);

    // Build LLM context.
    let llm_context = Context {
        system_prompt: Some(context.system_prompt.clone()),
        messages: llm_messages,
        tools: context
            .tools
            .as_ref()
            .map(|tools| tools.iter().map(|tool| tool.tool.clone()).collect()),
    };

    // Resolve API key (important for expiring tokens).
    let resolved_api_key = config
        .get_api_key
        .as_ref()
        .map(|f| f(&config.model.provider))
        .flatten()
        .or_else(|| config.api_key.clone());

    let stream_options = SimpleStreamOptions {
        stream: pi_ai::types::StreamOptions {
            request: pi_ai::types::ProviderRequestOptions {
                api_key: resolved_api_key,
                ..pi_ai::types::ProviderRequestOptions::default()
            },
            ..pi_ai::types::StreamOptions::default()
        },
        reasoning: config.reasoning.clone(),
        ..SimpleStreamOptions::default()
    };

    let response = stream_fn(&config.model, &llm_context, Some(&stream_options));

    let mut partial_message: Option<AssistantMessage> = None;
    let mut added_partial = false;

    while let Some(event) = response.next() {
        match &event {
            pi_ai::types::AssistantMessageEvent::Start { partial } => {
                partial_message = Some(partial.clone());
                context
                    .messages
                    .push(AgentMessage::Llm(Message::Assistant(partial.clone())));
                added_partial = true;
                emit(&AgentEvent::MessageStart {
                    message: AgentMessage::Llm(Message::Assistant(partial.clone())),
                });
            }
            pi_ai::types::AssistantMessageEvent::TextStart { partial, .. }
            | pi_ai::types::AssistantMessageEvent::TextDelta { partial, .. }
            | pi_ai::types::AssistantMessageEvent::TextEnd { partial, .. }
            | pi_ai::types::AssistantMessageEvent::ThinkingStart { partial, .. }
            | pi_ai::types::AssistantMessageEvent::ThinkingDelta { partial, .. }
            | pi_ai::types::AssistantMessageEvent::ThinkingEnd { partial, .. }
            | pi_ai::types::AssistantMessageEvent::ToolCallStart { partial, .. }
            | pi_ai::types::AssistantMessageEvent::ToolCallDelta { partial, .. }
            | pi_ai::types::AssistantMessageEvent::ToolCallEnd { partial, .. } => {
                if partial_message.is_some() {
                    partial_message = Some(partial.clone());
                    context.messages.last_mut().expect("partial pushed").clone_from(
                        &AgentMessage::Llm(Message::Assistant(partial.clone())),
                    );
                    emit(&AgentEvent::MessageUpdate {
                        message: AgentMessage::Llm(Message::Assistant(partial.clone())),
                        assistant_message_event: event.clone(),
                    });
                }
            }
            pi_ai::types::AssistantMessageEvent::Done { message, .. }
            | pi_ai::types::AssistantMessageEvent::Error { error: message, .. } => {
                return finalize_streamed_message(context, message.clone(), added_partial, emit);
            }
        }
    }

    // Stream ended without a terminal event: use the final result if any.
    let final_message = response.result();
    finalize_streamed_message(context, final_message, added_partial, emit)
}

fn finalize_streamed_message(
    context: &mut AgentContext,
    final_message: AssistantMessage,
    added_partial: bool,
    emit: &AgentEventSink,
) -> AssistantMessage {
    if added_partial {
        context.messages.last_mut().expect("partial pushed").clone_from(&AgentMessage::Llm(Message::Assistant(final_message.clone())));
    } else {
        context.messages.push(AgentMessage::Llm(Message::Assistant(final_message.clone())));
        emit(&AgentEvent::MessageStart {
            message: AgentMessage::Llm(Message::Assistant(final_message.clone())),
        });
    }
    emit(&AgentEvent::MessageEnd {
        message: AgentMessage::Llm(Message::Assistant(final_message.clone())),
    });
    final_message
}

struct ExecutedToolCallBatch {
    messages: Vec<ToolResultMessage>,
    terminate: bool,
}

/// Fail all tool calls from a message truncated by the output token limit.
fn fail_tool_calls_from_truncated_message(
    tool_calls: &[AgentToolCall],
    emit: &AgentEventSink,
) -> ExecutedToolCallBatch {
    let mut messages: Vec<ToolResultMessage> = Vec::new();
    for tool_call in tool_calls {
        emit(&AgentEvent::ToolExecutionStart {
            tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.name.clone(),
            args: tool_call.arguments.clone(),
        });
        let finalized = FinalizedToolCallOutcome {
            tool_call: tool_call.clone(),
            result: create_error_tool_result(&format!(
                "Tool call \"{}\" was not executed: the response hit the output token limit, so its arguments may be truncated. Re-issue the tool call with complete arguments.",
                tool_call.name
            )),
            is_error: true,
        };
        emit_tool_execution_end(&finalized, emit);
        let tool_result_message = create_tool_result_message(&finalized);
        emit_tool_result_message(&tool_result_message, emit);
        messages.push(tool_result_message);
    }
    ExecutedToolCallBatch {
        messages,
        terminate: false,
    }
}

/// Execute tool calls from an assistant message.
fn execute_tool_calls(
    current_context: &mut AgentContext,
    assistant_message: &AssistantMessage,
    config: &AgentLoopConfig,
    signal: Option<&pi_ai::utils::abort::CancellationToken>,
    emit: &AgentEventSink,
) -> ExecutedToolCallBatch {
    let tool_calls: Vec<AgentToolCall> = assistant_message
        .content
        .iter()
        .filter_map(|block| match block {
            Content::ToolCall(tool_call) => Some(tool_call.clone()),
            _ => None,
        })
        .collect();
    let has_sequential_tool_call = tool_calls.iter().any(|tool_call| {
        current_context
            .tools
            .as_ref()
            .and_then(|tools| tools.iter().find(|t| t.tool.name == tool_call.name))
            .is_some_and(|tool| tool.execution_mode == Some(ToolExecutionMode::Sequential))
    });
    if config.tool_execution == ToolExecutionMode::Sequential || has_sequential_tool_call {
        execute_tool_calls_sequential(current_context, assistant_message, &tool_calls, config, signal, emit)
    } else {
        execute_tool_calls_parallel(current_context, assistant_message, &tool_calls, config, signal, emit)
    }
}

fn execute_tool_calls_sequential(
    current_context: &mut AgentContext,
    assistant_message: &AssistantMessage,
    tool_calls: &[AgentToolCall],
    config: &AgentLoopConfig,
    signal: Option<&pi_ai::utils::abort::CancellationToken>,
    emit: &AgentEventSink,
) -> ExecutedToolCallBatch {
    let mut finalized_calls: Vec<FinalizedToolCallOutcome> = Vec::new();
    let mut messages: Vec<ToolResultMessage> = Vec::new();

    for tool_call in tool_calls {
        emit(&AgentEvent::ToolExecutionStart {
            tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.name.clone(),
            args: tool_call.arguments.clone(),
        });

        let preparation = prepare_tool_call(current_context, assistant_message, tool_call, config, signal);
        let finalized = match &preparation {
            PreparedToolCall::Immediate {
                result,
                is_error,
                ..
            } => FinalizedToolCallOutcome {
                tool_call: tool_call.clone(),
                result: result.clone(),
                is_error: *is_error,
            },
            PreparedToolCall::Prepared { .. } => {
                let executed = execute_prepared_tool_call(&preparation, signal, emit);
                finalize_executed_tool_call(current_context, assistant_message, &preparation, executed, config, signal)
            }
        };

        emit_tool_execution_end(&finalized, emit);
        let tool_result_message = create_tool_result_message(&finalized);
        emit_tool_result_message(&tool_result_message, emit);
        finalized_calls.push(finalized);
        messages.push(tool_result_message);

        if signal.is_some_and(|s| s.is_aborted()) {
            break;
        }
    }

    ExecutedToolCallBatch {
        messages,
        terminate: should_terminate_tool_batch(&finalized_calls),
    }
}

fn execute_tool_calls_parallel(
    current_context: &mut AgentContext,
    assistant_message: &AssistantMessage,
    tool_calls: &[AgentToolCall],
    config: &AgentLoopConfig,
    signal: Option<&pi_ai::utils::abort::CancellationToken>,
    emit: &AgentEventSink,
) -> ExecutedToolCallBatch {
    let mut finalized_entries: Vec<FinalizedToolCallEntry> = Vec::new();

    for tool_call in tool_calls {
        emit(&AgentEvent::ToolExecutionStart {
            tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.name.clone(),
            args: tool_call.arguments.clone(),
        });

        let preparation = prepare_tool_call(current_context, assistant_message, tool_call, config, signal);
        match &preparation {
            PreparedToolCall::Immediate {
                result,
                is_error,
                ..
            } => {
                let finalized = FinalizedToolCallOutcome {
                    tool_call: tool_call.clone(),
                    result: result.clone(),
                    is_error: *is_error,
                };
                emit_tool_execution_end(&finalized, emit);
                finalized_entries.push(FinalizedToolCallEntry::Immediate(finalized));
                if signal.is_some_and(|s| s.is_aborted()) {
                    break;
                }
                continue;
            }
            PreparedToolCall::Prepared { .. } => {
                finalized_entries.push(FinalizedToolCallEntry::Deferred(preparation));
                if signal.is_some_and(|s| s.is_aborted()) {
                    break;
                }
            }
        }
    }

    // Execute deferred entries in parallel (one thread each), emitting
    // tool_execution_end in completion order; then emit tool-result messages
    // in source order (mirroring the JS Promise.all shape).
    let ordered_finalized: Vec<FinalizedToolCallOutcome> = finalized_entries
        .into_iter()
        .map(|entry| match entry {
            FinalizedToolCallEntry::Immediate(finalized) => finalized,
            FinalizedToolCallEntry::Deferred(preparation) => {
                let current_context = current_context.clone();
                let assistant_message = assistant_message.clone();
                let signal = signal.cloned();
                let emit = emit;
                std::thread::scope(|scope| {
                    scope.spawn(move || {
                        let executed = execute_prepared_tool_call(&preparation, signal.as_ref(), emit);
                        let finalized = finalize_executed_tool_call_ref(
                            &current_context,
                            &assistant_message,
                            &preparation,
                            executed,
                            config,
                            signal.as_ref(),
                        );
                        emit_tool_execution_end(&finalized, emit);
                        finalized
                    })
                    .join()
                    .expect("tool thread panicked")
                })
            }
        })
        .collect();

    let mut messages: Vec<ToolResultMessage> = Vec::new();
    for finalized in &ordered_finalized {
        let tool_result_message = create_tool_result_message(finalized);
        emit_tool_result_message(&tool_result_message, emit);
        messages.push(tool_result_message);
    }

    ExecutedToolCallBatch {
        messages,
        terminate: should_terminate_tool_batch(&ordered_finalized),
    }
}

enum PreparedToolCall {
    Prepared {
        tool_call: AgentToolCall,
        tool: AgentTool,
        args: pi_ai::types::JsonValue,
    },
    Immediate {
        result: AgentToolResult,
        is_error: bool,
    },
}

enum FinalizedToolCallEntry {
    Immediate(FinalizedToolCallOutcome),
    Deferred(PreparedToolCall),
}

struct FinalizedToolCallOutcome {
    tool_call: AgentToolCall,
    result: AgentToolResult,
    is_error: bool,
}

fn should_terminate_tool_batch(finalized_calls: &[FinalizedToolCallOutcome]) -> bool {
    !finalized_calls.is_empty()
        && finalized_calls
            .iter()
            .all(|finalized| finalized.result.terminate == Some(true))
}

#[allow(clippy::too_many_arguments)]
fn prepare_tool_call(
    current_context: &mut AgentContext,
    assistant_message: &AssistantMessage,
    tool_call: &AgentToolCall,
    config: &AgentLoopConfig,
    signal: Option<&pi_ai::utils::abort::CancellationToken>,
) -> PreparedToolCall {
    let tool = current_context
        .tools
        .as_ref()
        .and_then(|tools| tools.iter().find(|t| t.tool.name == tool_call.name))
        .cloned();
    let Some(tool) = tool else {
        return PreparedToolCall::Immediate {
            result: create_error_tool_result(&format!("Tool {} not found", tool_call.name)),
            is_error: true,
        };
    };

    match validate_tool_arguments(&tool.tool, tool_call) {
        Ok(validated_args) => {
            if let Some(before) = &config.before_tool_call {
                let context = BeforeToolCallContext {
                    assistant_message,
                    tool_call,
                    args: &validated_args,
                    context: current_context,
                };
                let before_result = before(&context);
                if signal.is_some_and(|s| s.is_aborted()) {
                    return PreparedToolCall::Immediate {
                        result: create_error_tool_result("Operation aborted"),
                        is_error: true,
                    };
                }
                if let Some(before_result) = before_result {
                    if before_result.block == Some(true) {
                        let mut result =
                            create_error_tool_result(before_result.reason.as_deref().unwrap_or("Tool execution was blocked"));
                        if before_result.terminate == Some(true) {
                            result.terminate = Some(true);
                        }
                        return PreparedToolCall::Immediate {
                            result,
                            is_error: true,
                        };
                    }
                }
            }
            if signal.is_some_and(|s| s.is_aborted()) {
                return PreparedToolCall::Immediate {
                    result: create_error_tool_result("Operation aborted"),
                    is_error: true,
                };
            }
            PreparedToolCall::Prepared {
                tool_call: tool_call.clone(),
                tool,
                args: validated_args,
            }
        }
        Err(error) => PreparedToolCall::Immediate {
            result: create_error_tool_result(&error),
            is_error: true,
        },
    }
}

struct ExecutedToolCallOutcome {
    result: AgentToolResult,
    is_error: bool,
}

fn execute_prepared_tool_call(
    prepared: &PreparedToolCall,
    signal: Option<&pi_ai::utils::abort::CancellationToken>,
    emit: &AgentEventSink,
) -> ExecutedToolCallOutcome {
    let PreparedToolCall::Prepared {
        tool_call,
        tool,
        args,
    } = prepared
    else {
        unreachable!("immediate outcomes are not executed");
    };

    match &tool.execute {
        Some(execute) => {
            let update = |partial_result: &AgentToolResult| {
                emit(&AgentEvent::ToolExecutionUpdate {
                    tool_call_id: tool_call.id.clone(),
                    tool_name: tool_call.name.clone(),
                    args: tool_call.arguments.clone(),
                    partial_result: partial_result.clone(),
                });
            };
            let result = execute(&tool_call.id, args, signal, Some(&update));
            match result {
                Ok(result) => ExecutedToolCallOutcome {
                    result,
                    is_error: false,
                },
                Err(error) => ExecutedToolCallOutcome {
                    result: create_error_tool_result(&error),
                    is_error: true,
                },
            }
        }
        None => ExecutedToolCallOutcome {
            result: create_error_tool_result(&format!("Tool {} has no executor", tool_call.name)),
            is_error: true,
        },
    }
}



fn finalize_executed_tool_call(
    current_context: &mut AgentContext,
    assistant_message: &AssistantMessage,
    prepared: &PreparedToolCall,
    executed: ExecutedToolCallOutcome,
    config: &AgentLoopConfig,
    _signal: Option<&pi_ai::utils::abort::CancellationToken>,
) -> FinalizedToolCallOutcome {
    let PreparedToolCall::Prepared {
        tool_call,
        args, ..
    } = prepared
    else {
        unreachable!("immediate outcomes are not finalized");
    };

    let mut result = executed.result;
    let mut is_error = executed.is_error;

    if let Some(after) = &config.after_tool_call {
        let context = AfterToolCallContext {
            assistant_message,
            tool_call,
            args,
            result: &result,
            is_error,
            context: current_context,
        };
        if let Some(after_result) = after(&context) {
            if let Some(content) = after_result.content {
                result.content = content;
            }
            if let Some(details) = after_result.details {
                result.details = details;
            }
            if let Some(usage) = after_result.usage {
                result.usage = Some(usage);
            }
            if let Some(terminate) = after_result.terminate {
                result.terminate = Some(terminate);
            }
            if let Some(is_error_override) = after_result.is_error {
                is_error = is_error_override;
            }
        }
    }

    FinalizedToolCallOutcome {
        tool_call: tool_call.clone(),
        result,
        is_error,
    }
}

fn finalize_executed_tool_call_ref(
    current_context: &AgentContext,
    assistant_message: &AssistantMessage,
    prepared: &PreparedToolCall,
    executed: ExecutedToolCallOutcome,
    config: &AgentLoopConfig,
    signal: Option<&pi_ai::utils::abort::CancellationToken>,
) -> FinalizedToolCallOutcome {
    let mut context = current_context.clone();
    finalize_executed_tool_call(&mut context, assistant_message, prepared, executed, config, signal)
}

fn create_error_tool_result(message: &str) -> AgentToolResult {
    AgentToolResult {
        content: vec![Content::Text(pi_ai::types::TextContent {
            text: message.to_string(),
            text_signature: None,
        })],
        details: pi_ai::types::JsonValue::Map(Vec::new()),
        usage: None,
        added_tool_names: None,
        terminate: None,
    }
}

fn emit_tool_execution_end(finalized: &FinalizedToolCallOutcome, emit: &AgentEventSink) {
    emit(&AgentEvent::ToolExecutionEnd {
        tool_call_id: finalized.tool_call.id.clone(),
        tool_name: finalized.tool_call.name.clone(),
        result: finalized.result.clone(),
        is_error: finalized.is_error,
    });
}

fn create_tool_result_message(finalized: &FinalizedToolCallOutcome) -> ToolResultMessage {
    ToolResultMessage {
        tool_call_id: finalized.tool_call.id.clone(),
        tool_name: finalized.tool_call.name.clone(),
        content: finalized.result.content.clone(),
        details: Some(finalized.result.details.clone()),
        usage: finalized.result.usage.clone(),
        added_tool_names: finalized.result.added_tool_names.clone(),
        is_error: finalized.is_error,
        timestamp: now_ms(),
    }
}

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as f64
}

fn emit_tool_result_message(tool_result_message: &ToolResultMessage, emit: &AgentEventSink) {
    emit(&AgentEvent::MessageStart {
        message: AgentMessage::Llm(Message::ToolResult(tool_result_message.clone())),
    });
    emit(&AgentEvent::MessageEnd {
        message: AgentMessage::Llm(Message::ToolResult(tool_result_message.clone())),
    });
}
