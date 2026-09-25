/// Called per message per turn on the trim path: must be cheap, never block, never call a model.
pub trait TokenCounter: Send + Sync {
    /// Tokens `text` will occupy, excluding any per-message envelope.
    fn count(&self, text: &str) -> usize;

    /// True ONLY for the active model's own vocabulary; any other real tokenizer is inexact.
    /// Callers widen the safety margin for inexact counts.
    fn is_exact(&self) -> bool;

    /// Short label for logs and telemetry, e.g. `"chars/4"`, `"tiktoken"`.
    fn name(&self) -> &'static str;
}
