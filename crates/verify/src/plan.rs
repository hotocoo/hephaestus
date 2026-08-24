//! Verification plan: ordered layers with their commands.

use serde::{Deserialize, Serialize};

use hephaestus_core::{Error, Result};

/// Canonical layer names. The order of execution is the order in the
/// plan, but these names let policy require specific layers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    /// Formatting check (fastest; fail fast on style drift).
    Format,
    /// Linting.
    Lint,
    /// Type checking.
    TypeCheck,
    /// Unit tests.
    UnitTests,
    /// Integration tests.
    IntegrationTests,
    /// Security scans.
    Security,
    /// Build producing artifacts.
    Build,
}

impl Layer {
    /// Stable name for reports and API payloads.
    pub fn as_str(self) -> &'static str {
        match self {
            Layer::Format => "format",
            Layer::Lint => "lint",
            Layer::TypeCheck => "typecheck",
            Layer::UnitTests => "unit_tests",
            Layer::IntegrationTests => "integration_tests",
            Layer::Security => "security",
            Layer::Build => "build",
        }
    }
}

/// One layer's command: program plus argv (no shell interpretation).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerSpec {
    /// Which canonical layer this executes.
    pub layer: Layer,
    /// Program to run (must be allowlisted by caller capabilities).
    pub program: String,
    /// Arguments.
    pub args: Vec<String>,
}

impl LayerSpec {
    /// Construct a spec.
    pub fn new(layer: Layer, program: impl Into<String>, args: &[&str]) -> Self {
        Self {
            layer,
            program: program.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// An ordered verification plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerificationPlan {
    layers: Vec<LayerSpec>,
}

impl VerificationPlan {
    /// Validate and build a plan from specs.
    ///
    /// Rules (fail closed):
    /// * at least one layer,
    /// * no duplicate canonical layer names,
    /// * every spec has a non-empty program without separators
    ///   (allowlisting happens at capability level; this is a second
    ///   line of defense against path-like programs).
    pub fn new(specs: Vec<LayerSpec>) -> Result<Self> {
        if specs.is_empty() {
            return Err(Error::Validation {
                field: "layers".into(),
                message: "verification plan requires at least one layer".into(),
            });
        }
        let mut seen = std::collections::HashSet::new();
        for spec in &specs {
            if !seen.insert(spec.layer) {
                return Err(Error::Validation {
                    field: "layers".into(),
                    message: format!("duplicate layer {}", spec.layer.as_str()),
                });
            }
            let p = &spec.program;
            if p.is_empty() || p.contains('/') || p.contains('\\') || p != p.trim() {
                return Err(Error::Validation {
                    field: "program".into(),
                    message: format!("invalid program name {p:?}"),
                });
            }
        }
        Ok(Self { layers: specs })
    }

    /// Ordered layer specs.
    pub fn layers(&self) -> &[LayerSpec] {
        &self.layers
    }
}
