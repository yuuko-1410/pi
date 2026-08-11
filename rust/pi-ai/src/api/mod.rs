//! API-layer helpers. TEMPORARY: parallel-agent modules disabled for
//! isolated verification; restored after validation.

pub mod azure_openai_responses;
pub mod constrained_sampling;
pub mod github_copilot_headers;
pub mod google_generative_ai;
pub mod google_shared;
pub mod google_vertex;
pub mod openai_responses;
pub mod openai_codex_responses;
pub mod mistral_conversations;
pub mod openai_stream;
pub mod openai_responses_shared;
pub mod openrouter_images;
pub mod pi_messages;
pub mod prompt_cache;
pub mod simple_options;
pub mod transform_messages;
