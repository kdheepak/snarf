use std::time::Duration;

use color_eyre::eyre;
use reqwest::header::AUTHORIZATION;
use serde::Deserialize;

use crate::types::{DocsResult, LibraryMatch};

pub async fn search(
    backend: &str,
    query: &str,
    limit: usize,
    context7_api_key: &str,
) -> eyre::Result<Vec<DocsResult>> {
    match backend {
        "context7" => {
            if context7_api_key.is_empty() {
                eyre::bail!(
                    "context7: API key not set (get one then: snarf config set context7_api_key <key>)"
                );
            }
            let client = client()?;
            let libs = resolve_library(&client, query, context7_api_key).await?;
            let Some(library) = libs.first() else {
                eyre::bail!("context7: no library found for {query:?}");
            };
            let mut results = get_docs(&client, &library.id, query, 4000, context7_api_key).await?;
            results.truncate(limit);
            Ok(results)
        }
        "local" => eyre::bail!("local fts5 backend not yet implemented"),
        _ => eyre::bail!("unknown docs backend: {backend}"),
    }
}

pub async fn resolve(query: &str, context7_api_key: &str) -> eyre::Result<Vec<LibraryMatch>> {
    if context7_api_key.is_empty() {
        eyre::bail!(
            "context7: API key not set (get one then: snarf config set context7_api_key <key>)"
        );
    }
    resolve_library(&client()?, query, context7_api_key).await
}

pub async fn docs_for_library(
    query: &str,
    library_id: &str,
    tokens: usize,
    context7_api_key: &str,
) -> eyre::Result<Vec<DocsResult>> {
    if context7_api_key.is_empty() {
        eyre::bail!(
            "context7: API key not set (get one then: snarf config set context7_api_key <key>)"
        );
    }
    get_docs(&client()?, library_id, query, tokens, context7_api_key).await
}

fn client() -> eyre::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("Mozilla/5.0 (compatible; snarf/1.0)")
        .build()?)
}

async fn resolve_library(
    client: &reqwest::Client,
    name: &str,
    api_key: &str,
) -> eyre::Result<Vec<LibraryMatch>> {
    #[derive(Deserialize)]
    struct Context7SearchResponse {
        results: Vec<LibraryMatch>,
    }

    let response = client
        .get("https://context7.com/api/v1/search")
        .query(&[("query", name)])
        .header(AUTHORIZATION, format!("Bearer {api_key}"))
        .send()
        .await
        .map_err(|err| eyre::eyre!("context7 resolve request failed: {err}"))?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        eyre::bail!("context7: invalid API key (set via: snarf config set context7_api_key <key>)");
    }
    if !status.is_success() {
        eyre::bail!("context7 resolve returned status {}", status.as_u16());
    }
    let body: Context7SearchResponse = response
        .json()
        .await
        .map_err(|err| eyre::eyre!("failed to decode context7 resolve response: {err}"))?;
    Ok(body.results)
}

async fn get_docs(
    client: &reqwest::Client,
    library_id: &str,
    query: &str,
    tokens: usize,
    api_key: &str,
) -> eyre::Result<Vec<DocsResult>> {
    #[derive(Deserialize)]
    struct Context7DocsResponse {
        #[serde(rename = "codeSnippets", default)]
        code_snippets: Vec<Context7CodeSnippet>,
        #[serde(rename = "infoSnippets", default)]
        info_snippets: Vec<Context7InfoSnippet>,
    }
    #[derive(Deserialize)]
    struct Context7CodeSnippet {
        #[serde(rename = "codeTitle", default)]
        code_title: String,
        #[serde(rename = "codeId", default)]
        code_id: String,
        #[serde(rename = "codeList", default)]
        code_list: Vec<Context7CodeEntry>,
    }
    #[derive(Deserialize)]
    struct Context7CodeEntry {
        #[serde(default)]
        code: String,
    }
    #[derive(Deserialize)]
    struct Context7InfoSnippet {
        #[serde(rename = "pageId", default)]
        page_id: String,
        #[serde(default)]
        breadcrumb: String,
        #[serde(default)]
        content: String,
    }

    let response = client
        .get("https://context7.com/api/v2/context")
        .query(&[
            ("libraryId", library_id),
            ("query", query),
            ("type", "json"),
            ("tokens", &tokens.to_string()),
        ])
        .header(AUTHORIZATION, format!("Bearer {api_key}"))
        .send()
        .await
        .map_err(|err| eyre::eyre!("context7 docs request failed: {err}"))?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        eyre::bail!("context7: invalid API key (set via: snarf config set context7_api_key <key>)");
    }
    if !status.is_success() {
        eyre::bail!("context7 docs returned status {}", status.as_u16());
    }

    let body: Context7DocsResponse = response
        .json()
        .await
        .map_err(|err| eyre::eyre!("failed to decode context7 docs response: {err}"))?;
    let mut results = Vec::new();

    for snippet in body.code_snippets {
        results.push(DocsResult {
            library: library_id.to_string(),
            title: snippet.code_title,
            snippet: snippet
                .code_list
                .first()
                .map(|entry| entry.code.clone())
                .unwrap_or_default(),
            url: snippet.code_id,
            source: "context7".to_string(),
            ..DocsResult::default()
        });
    }

    for snippet in body.info_snippets {
        results.push(DocsResult {
            library: library_id.to_string(),
            title: snippet.breadcrumb.clone(),
            breadcrumb: snippet.breadcrumb,
            snippet: snippet.content,
            url: snippet.page_id,
            source: "context7".to_string(),
            ..DocsResult::default()
        });
    }

    Ok(results)
}
