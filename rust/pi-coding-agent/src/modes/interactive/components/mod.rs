//! Interactive components (port of `modes/interactive/components/`).
//!
//! ponytail: rendering is built on pi-tui primitives. Callback-style JS
//! props become explicit setters or constructor arguments. TUI-driven
//! timers (setInterval) are driven by an explicit `tick()` (see
//! countdown_timer and status_indicator).

pub mod armin;
pub mod branch_summary_message;
pub mod compaction_summary_message;
pub mod countdown_timer;
pub mod custom_entry;
pub mod daxnuts;
pub mod dynamic_border;
pub mod earendil_announcement;
pub mod keybinding_hints;
pub mod markdown_transform;
pub mod mermaid;
pub mod session_selector_search;
pub mod show_images_selector;
pub mod skill_invocation_message;
pub mod status_indicator;
pub mod theme_selector;
pub mod thinking_selector;
pub mod user_message;
pub mod visual_truncate;

pub use armin::ArminComponent;
pub use branch_summary_message::BranchSummaryMessageComponent;
pub use compaction_summary_message::CompactionSummaryMessageComponent;
pub use countdown_timer::CountdownTimer;
pub use custom_entry::CustomEntryComponent;
pub use daxnuts::DaxnutsComponent;
pub use dynamic_border::DynamicBorder;
pub use earendil_announcement::EarendilAnnouncementComponent;
pub use keybinding_hints::{key_hint, key_text, raw_key_hint};
pub use markdown_transform::create_markdown_transform;
pub use mermaid::create_mermaid_markdown_transformer;
pub use session_selector_search::{
    filter_and_sort_sessions, has_session_name, match_session, parse_search_query, MatchResult, NameFilter,
    ParsedSearchQuery, SortMode,
};
pub use show_images_selector::ShowImagesSelectorComponent;
pub use skill_invocation_message::SkillInvocationMessageComponent;
pub use status_indicator::{
    BranchSummaryStatusIndicator, CompactionStatusReason, CompactionStatusIndicator, IdleStatus,
    RetryStatusIndicator, StatusIndicator, StatusIndicatorKind, WorkingStatusIndicator,
};
pub use theme_selector::ThemeSelectorComponent;
pub use thinking_selector::ThinkingSelectorComponent;
pub use user_message::UserMessageComponent;
pub use visual_truncate::{truncate_to_visual_lines, VisualTruncateResult};
