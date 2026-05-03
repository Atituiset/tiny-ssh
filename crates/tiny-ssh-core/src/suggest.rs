//! Layered suggestion engine.
//!
//! v0.1 ships only Layer 2 (history-backed, Fish-style autosuggest). Layers 3
//! (static knowledge base) and 4 (LLM) are deferred but the trait below is
//! shaped to accommodate them: each layer returns a [`Suggestion`] with its
//! own provenance, and the engine picks the best score.

use crate::history::{History, HistoryError, Suggestion, SuggestContext};

/// Provenance of a suggestion. Mirrors [`crate::history::HistorySource`] but
/// describes which engine layer produced the suggestion (vs. which layer
/// produced the recorded command).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// Layer 2: history match (Fish-style autosuggest).
    History,
    /// Layer 3: static knowledge base (man / tldr / parameter schema). Reserved.
    Knowledge,
    /// Layer 4: LLM completion. Reserved.
    Llm,
}

#[derive(Debug, Clone)]
pub struct EngineSuggestion {
    pub command: String,
    pub provenance: Provenance,
    pub score: f64,
}

impl EngineSuggestion {
    fn from_history(s: Suggestion) -> Self {
        Self {
            command: s.command,
            provenance: Provenance::History,
            score: s.score,
        }
    }
}

/// MVP engine: delegates entirely to [`History`].
pub struct SuggestEngine<'a> {
    history: &'a History,
}

impl<'a> SuggestEngine<'a> {
    pub fn new(history: &'a History) -> Self {
        Self { history }
    }

    /// Best suggestion for the given context, or `None`.
    pub fn suggest(
        &self,
        ctx: &SuggestContext<'_>,
    ) -> Result<Option<EngineSuggestion>, HistoryError> {
        Ok(self.history.suggest(ctx)?.map(EngineSuggestion::from_history))
    }
}
