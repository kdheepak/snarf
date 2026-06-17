use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub fetched_url: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub description: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CodeResult {
    pub repo: String,
    pub path: String,
    #[serde(skip_serializing_if = "is_zero", default)]
    pub line: usize,
    pub snippet: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub language: String,
    #[serde(skip_serializing_if = "is_zero", default)]
    pub stars: usize,
    pub url: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Page {
    pub url: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub fetched_url: String,
    pub title: String,
    pub markdown: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub etag: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub last_modified: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub content_hash: String,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}
