//! EmbeddingGemma 2's task prompts (`Prompts`, Model.swift:94-106). Media inputs take no prompt.
//!
//! These strings are part of the vector contract: `ReferenceVectorTests` feeds exactly these to
//! the model, so a port that rephrases a prompt makes different vectors.

/// `task: search result | query: …` — what a query goes in as.
pub fn query(text: &str) -> String {
    format!("task: search result | query: {text}")
}

/// `task: code retrieval | query: …` — a search of the code index (the model card's code prompt).
pub fn code_query(text: &str) -> String {
    format!("task: code retrieval | query: {text}")
}

/// `title: … | text: …` — what a document, page, chunk, code stretch or notebook stretch goes in
/// as, with its path as the title ("none" when there is none).
pub fn document(title: Option<&str>, text: &str) -> String {
    let title = match title {
        Some(t) if !t.is_empty() => t,
        _ => "none",
    };
    format!("title: {title} | text: {text}")
}
