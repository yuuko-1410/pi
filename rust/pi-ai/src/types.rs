//! Core model, message, and stream types.
//!
//! Rust port of `packages/ai/src/types.ts` (the type layer). JS string-literal
//! unions are open (`Api = KnownApi | (string & {})`), so Rust models them as
//! `String` plus the known-value constants below. `Record<string, X>` becomes
//! `Vec<(String, X)>` preserving insertion order. JSON-valued fields reuse
//! `pi_protocol::Value` (a strict JSON value tree).

use pi_protocol::Value as JsonValue;

pub const KNOWN_APIS: &[&str] = &[
    "openai-completions",
    "mistral-conversations",
    "openai-responses",
    "azure-openai-responses",
    "openai-codex-responses",
    "anthropic-messages",
    "bedrock-converse-stream",
    "google-generative-ai",
    "google-vertex",
    "pi-messages",
];

pub const KNOWN_IMAGES_APIS: &[&str] = &["openrouter-images"];

pub const KNOWN_PROVIDERS: &[&str] = &[
    "amazon-bedrock",
    "ant-ling",
    "anthropic",
    "google",
    "google-vertex",
    "openai",
    "azure-openai-responses",
    "openai-codex",
    "radius",
    "nvidia",
    "deepseek",
    "github-copilot",
    "xai",
    "groq",
    "cerebras",
    "openrouter",
    "vercel-ai-gateway",
    "zai",
    "zai-coding-cn",
    "mistral",
    "minimax",
    "minimax-cn",
    "moonshotai",
    "moonshotai-cn",
    "huggingface",
    "fireworks",
    "together",
    "baseten",
    "opencode",
    "opencode-go",
    "kimi-coding",
    "cloudflare-workers-ai",
    "cloudflare-ai-gateway",
    "qwen-token-plan",
    "qwen-token-plan-cn",
    "qwen-token-plan-individual",
    "xiaomi",
    "xiaomi-token-plan-cn",
    "xiaomi-token-plan-ams",
    "xiaomi-token-plan-sgp",
];

pub const KNOWN_IMAGES_PROVIDERS: &[&str] = &["openrouter"];

pub type Api = String;
pub type ImagesApi = String;
pub type ProviderId = String;
pub type ImagesProviderId = String;

pub const THINKING_LEVELS: [&str; 6] = ["minimal", "low", "medium", "high", "xhigh", "max"];
pub const MODEL_THINKING_LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];
pub const CACHE_RETENTIONS: [&str; 3] = ["none", "short", "long"];
pub const TRANSPORTS: [&str; 4] = ["sse", "websocket", "websocket-cached", "auto"];
pub const SESSION_AFFINITY_FORMATS: [&str; 3] = ["openai", "openai-nosession", "openrouter"];

pub type ThinkingLevel = String;
pub type ModelThinkingLevel = String;
pub type CacheRetention = String;
pub type Transport = String;
pub type SessionAffinityFormat = String;

/// Partial record of thinking level -> provider value; `None` marks a level
/// as unsupported.
pub type ThinkingLevelMap = Vec<(ModelThinkingLevel, Option<String>)>;

#[derive(Clone, Debug, PartialEq)]
pub enum ChatTemplateKwargValue {
    Str(String),
    Number(f64),
    Bool(bool),
    Null,
    Var {
        var: String,
        omit_when_off: Option<bool>,
    },
}

/// Token budgets for each thinking level (token-based providers only).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThinkingBudgets {
    pub minimal: Option<f64>,
    pub low: Option<f64>,
    pub medium: Option<f64>,
    pub high: Option<f64>,
}

/// Provider-scoped environment overrides. Values take precedence over process
/// env.
pub type ProviderEnv = Vec<(String, String)>;

/// Custom HTTP headers; a `None` value suppresses a provider default with the
/// same name.
pub type ProviderHeaders = Vec<(String, Option<String>)>;

#[derive(Clone, Debug, PartialEq)]
pub struct ProviderResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
}

/// Authentication, HTTP transport, and lifecycle options shared by provider
/// requests. HTTP transport callbacks (fetch, onPayload, onResponse) belong
/// to the provider/API layer and are modeled there.
#[derive(Clone, Debug, Default)]
pub struct ProviderRequestOptions {
    pub api_key: Option<String>,
    pub env: Option<ProviderEnv>,
    pub headers: Option<ProviderHeaders>,
    /// HTTP request timeout in milliseconds.
    pub timeout_ms: Option<u64>,
    /// Maximum retry attempts for providers/SDKs that support client-side
    /// retries.
    pub max_retries: Option<u64>,
    /// Maximum delay in milliseconds to wait for a retry when the server
    /// requests a long wait. Default 60000; 0 disables the cap.
    pub max_retry_delay_ms: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct StreamOptions {
    pub request: ProviderRequestOptions,
    pub temperature: Option<f64>,
    /// Arbitrary sampling parameters merged into the request body as-is,
    /// after the named request fields. Only applied by OpenAI-compatible
    /// adapters.
    pub sampling_params: Option<Vec<(String, JsonValue)>>,
    pub max_tokens: Option<f64>,
    /// Preferred transport for providers that support multiple transports.
    pub transport: Option<Transport>,
    /// Prompt cache retention preference. Default: "short".
    pub cache_retention: Option<CacheRetention>,
    /// Optional session identifier for session-based caching providers.
    pub session_id: Option<String>,
    /// WebSocket connect timeout in milliseconds (connect/open handshake
    /// only).
    pub websocket_connect_timeout_ms: Option<u64>,
    /// Optional metadata included in API requests; providers extract what
    /// they understand.
    pub metadata: Option<Vec<(String, JsonValue)>>,
}

/// Deferred-response long-poll options.
#[derive(Clone, Debug, Default)]
pub struct DeferredFetchOptions {
    pub request: ProviderRequestOptions,
    /// Maximum provider long-poll duration in milliseconds. Defaults to 0,
    /// which performs one status check.
    pub wait: Option<u64>,
}

/// Request options for best-effort deferred-response cancellation.
pub type DeferredCancelOptions = ProviderRequestOptions;

/// Unified options with reasoning passed to streamSimple()/completeSimple().
#[derive(Clone, Debug, Default)]
pub struct SimpleStreamOptions {
    pub stream: StreamOptions,
    pub reasoning: Option<ThinkingLevel>,
    /// Ask a capable provider to return a durable handle and continue the
    /// request asynchronously.
    pub deferred: Option<DeferredRequest>,
    /// Custom token budgets for thinking levels (token-based providers only).
    pub thinking_budgets: Option<ThinkingBudgets>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DeferredRequest {
    Bool(bool),
    Window { window: String },
}

pub struct TextSignatureV1 {
    pub v: u8,
    pub id: String,
    pub phase: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextContent {
    pub text: String,
    /// e.g., for OpenAI responses, message metadata (legacy id string or
    /// TextSignatureV1 JSON).
    pub text_signature: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ThinkingContent {
    pub thinking: String,
    /// e.g., for OpenAI responses, the reasoning item ID; when redacted, the
    /// opaque encrypted payload for multi-turn continuity.
    pub thinking_signature: Option<String>,
    /// When true, the thinking content was redacted by safety filters.
    pub redacted: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImageContent {
    /// base64 encoded image data
    pub data: String,
    /// e.g., "image/jpeg", "image/png"
    pub mime_type: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: JsonValue,
    /// Google-specific: opaque signature for reusing thought context.
    pub thought_signature: Option<String>,
    /// OpenAI Responses namespace for dynamically loaded/namespaced tools.
    pub namespace: Option<String>,
}

/// Content block. Position constraints match the JS types: user/tool
/// messages use Text|Image, assistant messages use Text|Thinking|ToolCall.
#[derive(Clone, Debug, PartialEq)]
pub enum Content {
    Text(TextContent),
    Thinking(ThinkingContent),
    Image(ImageContent),
    ToolCall(ToolCall),
}

#[derive(Clone, Debug, PartialEq)]
pub struct UsageCost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub total: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Usage {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    /// Subset of `cache_write` written with 1h retention. Only Anthropic
    /// reports this split.
    pub cache_write_1h: Option<f64>,
    /// Reasoning/thinking tokens, when the provider reports them. Subset of
    /// `output`.
    pub reasoning: Option<f64>,
    pub total_tokens: f64,
    pub cost: UsageCost,
}

#[derive(Clone, Debug, Copy, PartialEq, Eq)]
pub enum StopReason {
    Pending,
    Stop,
    Length,
    ToolUse,
    Error,
    Aborted,
    Deferred,
}

impl StopReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Stop => "stop",
            Self::Length => "length",
            Self::ToolUse => "toolUse",
            Self::Error => "error",
            Self::Aborted => "aborted",
            Self::Deferred => "deferred",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "pending" => Self::Pending,
            "stop" => Self::Stop,
            "length" => Self::Length,
            "toolUse" => Self::ToolUse,
            "error" => Self::Error,
            "aborted" => Self::Aborted,
            "deferred" => Self::Deferred,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DeferredHandle {
    pub provider: String,
    pub model_id: String,
    pub api: String,
    /// Provider token, such as a response id or batch id plus row id.
    pub id: String,
    pub expires_at: Option<f64>,
    pub poll_after_ms: Option<f64>,
    /// Provider conversion data required to reconstruct the final assistant
    /// message.
    pub data: Option<JsonValue>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UserMessage {
    pub content: UserMessageContent,
    /// Unix timestamp in milliseconds
    pub timestamp: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum UserMessageContent {
    Text(String),
    Blocks(Vec<Content>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct AssistantMessage {
    pub content: Vec<Content>,
    pub api: Api,
    pub provider: ProviderId,
    pub model: String,
    /// Concrete `chunk.model` when different from the requested `model`.
    pub response_model: Option<String>,
    /// Provider-specific response/message identifier when exposed.
    pub response_id: Option<String>,
    pub usage: Usage,
    pub stop_reason: StopReason,
    pub deferred: Option<DeferredHandle>,
    pub error_message: Option<String>,
    pub raw_stop_reason: Option<String>,
    /// Provider indication of whether the model explicitly ended its turn.
    pub end_turn: Option<bool>,
    /// Unix timestamp in milliseconds
    pub timestamp: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolResultMessage {
    pub tool_call_id: String,
    pub tool_name: String,
    /// Supports text and images
    pub content: Vec<Content>,
    pub details: Option<JsonValue>,
    /// Usage from the tool execution itself. Not part of main LLM context
    /// accounting.
    pub usage: Option<Usage>,
    /// Names from `Context.tools` that became available after this result.
    pub added_tool_names: Option<Vec<String>>,
    pub is_error: bool,
    /// Unix timestamp in milliseconds
    pub timestamp: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    User(UserMessage),
    Assistant(AssistantMessage),
    ToolResult(ToolResultMessage),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImagesContext {
    pub input: Vec<Content>,
}

#[derive(Clone, Debug, Copy, PartialEq, Eq)]
pub enum ImagesStopReason {
    Stop,
    Error,
    Aborted,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AssistantImages {
    pub api: ImagesApi,
    pub provider: ImagesProviderId,
    pub model: String,
    pub output: Vec<Content>,
    pub response_id: Option<String>,
    pub usage: Option<Usage>,
    pub stop_reason: ImagesStopReason,
    pub error_message: Option<String>,
    /// Unix timestamp in milliseconds
    pub timestamp: f64,
}

// ---------------------------------------------------------------------------
// Tools and context
// ---------------------------------------------------------------------------

pub const GRAMMAR_FORMATS: [&str; 2] = ["openai_lark", "openai_regex"];
pub type GrammarFormat = String;
pub type GrammarVariants = Vec<(GrammarFormat, String)>;

/// Optional provider-side constrained sampling configs for a tool.
#[derive(Clone, Debug, PartialEq)]
pub enum ConstrainedSamplingConfig {
    JsonSchema { strict: String },
    Grammar { variants: GrammarVariants },
}

/// Minimal JSON-schema shape used for tool parameter validation, mirroring
/// the `JsonSchemaObject` interface in utils/validation.ts.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct JsonSchemaObject {
    pub type_: Option<Vec<String>>,
    pub properties: Option<Vec<(String, JsonSchemaObject)>>,
    pub items: Option<Box<JsonSchemaValue>>,
    pub additional_properties: Option<JsonSchemaAdditional>,
    pub all_of: Option<Vec<JsonSchemaObject>>,
    pub any_of: Option<Vec<JsonSchemaObject>>,
    pub one_of: Option<Vec<JsonSchemaObject>>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum JsonSchemaValue {
    Schema(JsonSchemaObject),
    Schemas(Vec<JsonSchemaObject>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum JsonSchemaAdditional {
    Bool(bool),
    Schema(Box<JsonSchemaObject>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub parameters: JsonSchemaObject,
    pub constrained_sampling: Option<ConstrainedSampling>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ConstrainedSampling {
    Disabled,
    Config(ConstrainedSamplingConfig),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Context {
    pub system_prompt: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Option<Vec<Tool>>,
}

// ---------------------------------------------------------------------------
// Stream event protocol
// ---------------------------------------------------------------------------

/// Event protocol for the assistant message event stream. Streams emit
/// `start` before partial updates, then terminate with `done` or `error`.
#[derive(Clone, Debug, PartialEq)]
pub enum AssistantMessageEvent {
    Start { partial: AssistantMessage },
    TextStart { content_index: f64, partial: AssistantMessage },
    TextDelta { content_index: f64, delta: String, partial: AssistantMessage },
    TextEnd { content_index: f64, content: String, partial: AssistantMessage },
    ThinkingStart { content_index: f64, partial: AssistantMessage },
    ThinkingDelta { content_index: f64, delta: String, partial: AssistantMessage },
    ThinkingEnd { content_index: f64, content: String, partial: AssistantMessage },
    ToolCallStart { content_index: f64, partial: AssistantMessage },
    ToolCallDelta { content_index: f64, delta: String, partial: AssistantMessage },
    ToolCallEnd { content_index: f64, tool_call: ToolCall, partial: AssistantMessage },
    Done {
        reason: String,
        message: AssistantMessage,
    },
    Error {
        reason: String,
        error: AssistantMessage,
    },
}

// ---------------------------------------------------------------------------
// Compatibility settings
// ---------------------------------------------------------------------------

pub const THINKING_FORMATS: [&str; 11] = [
    "openai",
    "openrouter",
    "deepseek",
    "together",
    "baseten",
    "zai",
    "qwen",
    "chat-template",
    "qwen-chat-template",
    "string-thinking",
    "ant-ling",
];

/// Compatibility settings for OpenAI-compatible completions APIs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OpenAICompletionsCompat {
    pub supports_store: Option<bool>,
    pub supports_developer_role: Option<bool>,
    pub supports_reasoning_effort: Option<bool>,
    pub supports_usage_in_streaming: Option<bool>,
    pub supports_finish_reason: Option<bool>,
    pub max_tokens_field: Option<String>,
    pub requires_tool_result_name: Option<bool>,
    pub requires_assistant_after_tool_result: Option<bool>,
    pub requires_thinking_as_text: Option<bool>,
    pub requires_reasoning_content_on_assistant_messages: Option<bool>,
    pub thinking_format: Option<String>,
    pub chat_template_kwargs: Option<Vec<(String, ChatTemplateKwargValue)>>,
    pub chat_template_args: Option<Vec<(String, ChatTemplateKwargValue)>>,
    pub open_router_routing: Option<OpenRouterRouting>,
    pub vercel_gateway_routing: Option<VercelGatewayRouting>,
    pub zai_tool_stream: Option<bool>,
    pub supports_thinking_token_budget: Option<bool>,
    pub supports_openai_grammar_tools: Option<bool>,
    pub supports_strict_mode: Option<bool>,
    pub cache_control_format: Option<String>,
    pub send_session_affinity_headers: Option<bool>,
    pub deferred_tools_mode: Option<String>,
    pub session_affinity_format: Option<SessionAffinityFormat>,
    pub supports_long_cache_retention: Option<bool>,
}

/// Compatibility settings for OpenAI Responses APIs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OpenAIResponsesCompat {
    pub supports_developer_role: Option<bool>,
    pub session_affinity_format: Option<SessionAffinityFormat>,
    pub supports_long_cache_retention: Option<bool>,
    pub supports_strict_mode: Option<bool>,
    pub supports_openai_grammar_tools: Option<bool>,
    pub supports_additional_tools: Option<bool>,
    pub supports_tool_search: Option<bool>,
    pub supports_explicit_prompt_cache_mode: Option<bool>,
}

/// Compatibility settings for Anthropic Messages-compatible APIs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AnthropicMessagesCompat {
    pub supports_eager_tool_input_streaming: Option<bool>,
    pub supports_long_cache_retention: Option<bool>,
    pub send_session_affinity_headers: Option<bool>,
    pub supports_cache_control_on_tools: Option<bool>,
    pub supports_temperature: Option<bool>,
    pub force_adaptive_thinking: Option<bool>,
    pub allow_empty_signature: Option<bool>,
    pub supports_strict_tools: Option<bool>,
    pub supports_tool_references: Option<bool>,
}

/// Compatibility settings for Amazon Bedrock models.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BedrockCompat {
    pub supports_strict_mode: Option<bool>,
}

/// OpenRouter provider routing preferences (request `provider` field).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OpenRouterRouting {
    pub allow_fallbacks: Option<bool>,
    pub require_parameters: Option<bool>,
    pub data_collection: Option<String>,
    pub zdr: Option<bool>,
    pub enforce_distillable_text: Option<bool>,
    pub order: Option<Vec<String>>,
    pub only: Option<Vec<String>>,
    pub ignore: Option<Vec<String>>,
    pub quantizations: Option<Vec<String>>,
    pub sort: Option<OpenRouterSort>,
    pub max_price: Option<OpenRouterMaxPrice>,
    pub preferred_min_throughput: Option<OpenRouterThroughput>,
    pub preferred_max_latency: Option<OpenRouterLatency>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum OpenRouterSort {
    String(String),
    Object {
        by: Option<String>,
        partition: Option<String>,
    },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct OpenRouterMaxPrice {
    pub prompt: Option<OpenRouterPrice>,
    pub completion: Option<OpenRouterPrice>,
    pub image: Option<OpenRouterPrice>,
    pub audio: Option<OpenRouterPrice>,
    pub request: Option<OpenRouterPrice>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum OpenRouterPrice {
    Number(f64),
    String(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum OpenRouterThroughput {
    Number(f64),
    Percentiles {
        p50: Option<f64>,
        p75: Option<f64>,
        p90: Option<f64>,
        p99: Option<f64>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum OpenRouterLatency {
    Number(f64),
    Percentiles {
        p50: Option<f64>,
        p75: Option<f64>,
        p90: Option<f64>,
        p99: Option<f64>,
    },
}

/// Vercel AI Gateway routing preferences.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VercelGatewayRouting {
    pub only: Option<Vec<String>>,
    pub order: Option<Vec<String>>,
}

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct ModelCostRates {
    /// $/million tokens
    pub input: f64,
    /// $/million tokens
    pub output: f64,
    /// $/million tokens
    pub cache_read: f64,
    /// $/million tokens
    pub cache_write: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelCostTier {
    pub rates: ModelCostRates,
    /// Use this tier for requests whose total input usage exceeds this token
    /// count.
    pub input_tokens_above: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelCost {
    pub rates: ModelCostRates,
    /// Request-wide pricing tiers. The highest matching input threshold
    /// applies to the full request.
    pub tiers: Option<Vec<ModelCostTier>>,
}

/// Unified model definition.
#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    pub id: String,
    pub name: String,
    pub api: Api,
    pub provider: ProviderId,
    pub base_url: String,
    pub reasoning: bool,
    /// Maps pi thinking levels to provider/model-specific values. Missing
    /// keys use provider defaults; `None` marks a level as unsupported.
    pub thinking_level_map: Option<ThinkingLevelMap>,
    pub input: Vec<String>,
    pub cost: ModelCost,
    pub context_window: f64,
    pub max_tokens: f64,
    /// Default sampling parameters; per-request keys override these.
    pub sampling_params: Option<Vec<(String, JsonValue)>>,
    pub headers: Option<Vec<(String, String)>>,
    /// Compatibility overrides for OpenAI-compatible APIs. If not set,
    /// auto-detected from baseUrl.
    pub compat: Option<ModelCompat>,
}

/// Typed compat per API family; `None` means no compat overrides.
#[derive(Clone, Debug, PartialEq)]
pub enum ModelCompat {
    OpenAiCompletions(OpenAICompletionsCompat),
    OpenAiResponses(OpenAIResponsesCompat),
    AnthropicMessages(AnthropicMessagesCompat),
    Bedrock(BedrockCompat),
}

/// Image-generation model definition.
#[derive(Clone, Debug, PartialEq)]
pub struct ImagesModel {
    pub model: Model,
    pub api: ImagesApi,
    pub provider: ImagesProviderId,
    pub output: Vec<String>,
}
