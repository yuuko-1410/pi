//! API-layer helpers. TEMPORARY: parallel-agent modules disabled for
//! isolated verification; restored after validation.

pub mod anthropic_messages;
pub mod azure_openai_responses;
pub mod constrained_sampling;
pub mod github_copilot_headers;
pub mod google_generative_ai;
pub mod google_shared;
pub mod google_vertex;
pub mod openai_responses;
pub mod openai_codex_responses;
pub mod openai_completions;
pub mod mistral_conversations;
pub mod openai_stream;
pub mod openai_responses_shared;
pub mod openrouter_images;
pub mod pi_messages;
pub mod prompt_cache;
pub mod simple_options;
pub mod transform_messages;

/// Dispatch a stream by `model.api`, mirroring the JS streamSimple
/// provider-selection switch in `src/index.ts`.
pub fn dispatch_stream_simple(
    model: &crate::types::Model,
    context: &crate::types::Context,
    options: Option<&crate::types::SimpleStreamOptions>,
    api_key: Option<&str>,
    client: &crate::http::client::HttpClient,
) -> crate::event_stream::AssistantMessageEventStream {
    match model.api.as_str() {
        "openai" => openai_responses::stream_simple(model, context, options, api_key, client),
        "openai-completions" => {
            openai_completions::stream_simple(model, context, options, api_key, client)
        }
        "openai-codex" => openai_codex_responses::stream_simple(model, context, options, api_key, client, None),
        "anthropic" => anthropic_messages::stream_simple(model, context, options, api_key, client),
        "google" => google_generative_ai::stream_simple(model, context, options, api_key, client),
        "azure-openai-responses" => {
            azure_openai_responses::stream_simple(model, context, options, api_key, client)
        }
        other => panic!("unsupported model api: {other}"),
    }
}
