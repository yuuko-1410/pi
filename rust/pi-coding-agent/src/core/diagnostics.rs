//! Resource diagnostics, port of `core/diagnostics.ts`.

#[derive(Clone, Debug, PartialEq)]
pub struct ResourceCollision {
    pub resource_type: String, // "extension" | "skill" | "prompt" | "theme"
    pub name: String,
    pub winner_path: String,
    pub loser_path: String,
    pub winner_source: Option<String>,
    pub loser_source: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResourceDiagnostic {
    pub kind: String, // "warning" | "error" | "collision"
    pub message: String,
    pub path: Option<String>,
    pub collision: Option<ResourceCollision>,
}
