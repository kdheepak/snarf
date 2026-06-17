use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use color_eyre::eyre;
use reqwest::StatusCode;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap};
use serde::Deserialize;
use serde_json::json;

use crate::types::CodeResult;

#[derive(Debug, Clone)]
pub struct CodeQuery {
    pub term: String,
    pub lang: String,
    pub limit: usize,
    pub regexp: bool,
}

pub async fn search(
    backend: &str,
    query: CodeQuery,
    sourcegraph_url: &str,
    github_token: &str,
) -> eyre::Result<Vec<CodeResult>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .user_agent("Mozilla/5.0 (compatible; snarf/1.0)")
        .build()?;

    match backend {
        "grepapp" => grepapp(&client, &query).await,
        "sourcegraph" => sourcegraph(&client, &query, sourcegraph_url).await,
        "github" => github(&client, &query, github_token).await,
        _ => eyre::bail!("unknown code backend: {backend}"),
    }
}

async fn grepapp(client: &reqwest::Client, query: &CodeQuery) -> eyre::Result<Vec<CodeResult>> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "searchGitHub",
            "arguments": {
                "query": query.term,
                "language": normalize_grep_lang(&query.lang),
                "useRegexp": query.regexp
            }
        }
    });

    let response = client
        .post("https://mcp.grep.app")
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "application/json, text/event-stream")
        .json(&body)
        .send()
        .await
        .map_err(|err| eyre::eyre!("grep.app request failed: {err}"))?;
    if !response.status().is_success() {
        eyre::bail!("grep.app returned status {}", response.status().as_u16());
    }

    let raw = response
        .text()
        .await
        .map_err(|err| eyre::eyre!("grep.app stream error: {err}"))?;
    let texts = parse_grep_sse(&raw)?;
    Ok(texts
        .into_iter()
        .filter_map(|text| parse_grep_block(&text, &query.term, &query.lang))
        .take(query.limit)
        .collect())
}

#[derive(Deserialize)]
struct GrepMcpResponse {
    result: Option<GrepMcpResult>,
    error: Option<GrepMcpError>,
}

#[derive(Deserialize)]
struct GrepMcpResult {
    content: Vec<GrepMcpContent>,
    #[serde(rename = "isError", default)]
    is_error: bool,
}

#[derive(Deserialize)]
struct GrepMcpContent {
    #[serde(rename = "type")]
    kind: String,
    text: String,
}

#[derive(Deserialize)]
struct GrepMcpError {
    message: String,
}

fn parse_grep_sse(raw: &str) -> eyre::Result<Vec<String>> {
    for data in raw
        .lines()
        .filter_map(|line| line.trim().strip_prefix("data:"))
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let Ok(message) = serde_json::from_str::<GrepMcpResponse>(data) else {
            continue;
        };
        if let Some(error) = message.error {
            eyre::bail!("grep.app error: {}", error.message);
        }
        let Some(result) = message.result else {
            continue;
        };
        if result.is_error {
            eyre::bail!("grep.app search failed");
        }
        return Ok(result
            .content
            .into_iter()
            .filter(|content| content.kind == "text" && !content.text.is_empty())
            .map(|content| content.text)
            .collect());
    }
    Ok(Vec::new())
}

fn parse_grep_block(text: &str, query: &str, lang: &str) -> Option<CodeResult> {
    let mut result = CodeResult {
        source: "grepapp".to_string(),
        language: lang.to_string(),
        ..CodeResult::default()
    };
    let lines: Vec<&str> = text.lines().collect();
    let mut code_start = None;
    let mut start_line = 0usize;

    for (index, line) in lines.iter().enumerate() {
        if let Some(repo) = line.strip_prefix("Repository:") {
            result.repo = repo.trim().to_string();
        } else if let Some(path) = line.strip_prefix("Path:") {
            result.path = path.trim().to_string();
        } else if let Some(url) = line.strip_prefix("URL:") {
            result.url = url.trim().to_string();
        } else if line.starts_with("--- Snippet") && code_start.is_none() {
            start_line = parse_snippet_line(line);
            code_start = Some(index + 1);
        }
    }

    if result.repo.is_empty() || result.url.is_empty() {
        return None;
    }

    if let Some(start) = code_start {
        let (snippet, line) = extract_grep_snippet(&lines[start..], start_line, query);
        result.snippet = snippet;
        result.line = line;
    }
    Some(result)
}

fn parse_snippet_line(header: &str) -> usize {
    header
        .split("(Line ")
        .nth(1)
        .and_then(|rest| rest.split(')').next())
        .and_then(|number| number.trim().parse().ok())
        .unwrap_or(0)
}

fn extract_grep_snippet(lines: &[&str], start_line: usize, query: &str) -> (String, usize) {
    let query = query.to_ascii_lowercase();
    let mut first_non_empty = String::new();
    let mut first_non_empty_line = 0usize;

    for (index, line) in lines.iter().enumerate() {
        if line.starts_with("--- Snippet") {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if first_non_empty.is_empty() {
            first_non_empty = trimmed.to_string();
            first_non_empty_line = start_line + index;
        }
        if trimmed.to_ascii_lowercase().contains(&query) {
            return (trimmed.to_string(), start_line + index);
        }
    }

    (first_non_empty, first_non_empty_line)
}

fn normalize_grep_lang(lang: &str) -> Vec<String> {
    if lang.is_empty() {
        return Vec::new();
    }
    let lower = lang.to_ascii_lowercase();
    let name = match lower.as_str() {
        "go" | "golang" => "Go",
        "py" | "python" => "Python",
        "js" | "javascript" => "JavaScript",
        "ts" | "typescript" => "TypeScript",
        "tsx" => "TSX",
        "jsx" => "JSX",
        "rb" | "ruby" => "Ruby",
        "rs" | "rust" => "Rust",
        "java" => "Java",
        "kt" | "kotlin" => "Kotlin",
        "c" => "C",
        "cpp" | "c++" => "C++",
        "cs" | "csharp" => "C#",
        "php" => "PHP",
        "swift" => "Swift",
        "scala" => "Scala",
        "sh" | "bash" | "shell" => "Shell",
        "html" => "HTML",
        "css" => "CSS",
        "sql" => "SQL",
        _ => return vec![capitalize(lang)],
    };
    vec![name.to_string()]
}

fn capitalize(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

async fn sourcegraph(
    client: &reqwest::Client,
    query: &CodeQuery,
    base_url: &str,
) -> eyre::Result<Vec<CodeResult>> {
    let mut full = query.term.clone();
    if !query.lang.is_empty() {
        full.push_str(" lang:");
        full.push_str(&query.lang);
    }
    if !full.contains("archived:") {
        full.push_str(" archived:no");
    }
    if !full.contains("fork:") {
        full.push_str(" fork:no");
    }
    if query.regexp {
        full.push_str(" patterntype:regexp");
    }

    let url = format!("{}/.api/search/stream", base_url.trim_end_matches('/'));
    let response = client
        .get(url)
        .query(&[("q", full), ("display", query.limit.to_string())])
        .header(ACCEPT, "text/event-stream")
        .send()
        .await
        .map_err(|err| eyre::eyre!("sourcegraph request failed: {err}"))?;
    if !response.status().is_success() {
        eyre::bail!("sourcegraph returned status {}", response.status().as_u16());
    }

    let raw = response
        .text()
        .await
        .map_err(|err| eyre::eyre!("sourcegraph stream error: {err}"))?;
    Ok(parse_sourcegraph_sse(
        &raw,
        base_url.trim_end_matches('/'),
        query.limit,
    ))
}

#[derive(Deserialize)]
struct SourcegraphMatch {
    #[serde(rename = "type")]
    kind: String,
    repository: String,
    path: String,
    #[serde(default)]
    language: String,
    #[serde(rename = "repoStars", default)]
    repo_stars: usize,
    #[serde(rename = "lineMatches", default)]
    line_matches: Vec<SourcegraphLineMatch>,
}

#[derive(Deserialize)]
struct SourcegraphLineMatch {
    line: String,
    #[serde(rename = "lineNumber")]
    line_number: usize,
}

fn parse_sourcegraph_sse(raw: &str, base_url: &str, limit: usize) -> Vec<CodeResult> {
    let mut event_type = "";
    let mut results = Vec::new();
    for line in raw.lines() {
        if let Some(event) = line.strip_prefix("event:") {
            event_type = event.trim();
            continue;
        }
        if event_type != "matches" {
            continue;
        }
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let Ok(matches) = serde_json::from_str::<Vec<SourcegraphMatch>>(data.trim()) else {
            continue;
        };
        for item in matches {
            if results.len() >= limit {
                return results;
            }
            if item.kind != "content" || item.line_matches.is_empty() {
                continue;
            }
            let line_match = &item.line_matches[0];
            results.push(CodeResult {
                repo: item.repository.clone(),
                path: item.path.clone(),
                line: line_match.line_number,
                snippet: line_match.line.clone(),
                language: item.language.clone(),
                stars: item.repo_stars,
                url: format!(
                    "{}/{}/-/blob/{}#L{}",
                    base_url, item.repository, item.path, line_match.line_number
                ),
                source: "sourcegraph".to_string(),
            });
        }
    }
    results
}

async fn github(
    client: &reqwest::Client,
    query: &CodeQuery,
    token: &str,
) -> eyre::Result<Vec<CodeResult>> {
    if token.is_empty() {
        eyre::bail!("github: token required");
    }
    if query.regexp {
        eyre::bail!("REGEX_UNSUPPORTED");
    }

    let response = github_search_code(client, query, token).await?;
    let mut results = Vec::new();
    let mut node_ids = Vec::new();

    for item in &response.items {
        if results.len() >= query.limit {
            break;
        }
        let snippet = item
            .text_matches
            .first()
            .map(|text_match| extract_matched_line(&text_match.fragment, &text_match.matches))
            .unwrap_or_default();
        results.push(CodeResult {
            repo: item.repository.full_name.clone(),
            path: item.path.clone(),
            snippet,
            url: item.html_url.clone(),
            source: "github".to_string(),
            ..CodeResult::default()
        });
        if !item.repository.node_id.is_empty() {
            node_ids.push(item.repository.node_id.clone());
        }
    }

    if let Ok(stars) = github_fetch_stars(client, &node_ids, token).await {
        for (index, result) in results.iter_mut().enumerate() {
            if let Some(item) = response.items.get(index)
                && let Some(stars) = stars.get(&item.repository.node_id)
            {
                result.stars = *stars;
            }
        }
    }

    Ok(results)
}

#[derive(Deserialize)]
struct GithubSearchResponse {
    items: Vec<GithubItem>,
}

#[derive(Deserialize)]
struct GithubItem {
    path: String,
    html_url: String,
    repository: GithubRepo,
    #[serde(default)]
    text_matches: Vec<GithubTextMatch>,
}

#[derive(Deserialize)]
struct GithubRepo {
    full_name: String,
    #[serde(default)]
    node_id: String,
}

#[derive(Deserialize)]
struct GithubTextMatch {
    fragment: String,
    #[serde(default)]
    matches: Vec<GithubMatchRange>,
}

#[derive(Deserialize)]
struct GithubMatchRange {
    #[serde(default)]
    indices: Vec<usize>,
}

async fn github_search_code(
    client: &reqwest::Client,
    query: &CodeQuery,
    token: &str,
) -> eyre::Result<GithubSearchResponse> {
    let mut full = query.term.clone();
    if !query.lang.is_empty() && !full.contains("language:") {
        full.push_str(" language:");
        full.push_str(&query.lang);
    }
    let per_page = if query.limit == 0 || query.limit > 100 {
        30
    } else {
        query.limit
    };

    let response = client
        .get("https://api.github.com/search/code")
        .query(&[("q", full), ("per_page", per_page.to_string())])
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .header(ACCEPT, "application/vnd.github.text-match+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(|err| eyre::eyre!("github request failed: {err}"))?;

    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        eyre::bail!("github: invalid token (token must have 'repo' scope; check: gh auth status)");
    }
    if status == StatusCode::FORBIDDEN || status == StatusCode::TOO_MANY_REQUESTS {
        eyre::bail!("{}", github_rate_limit_error(status, response.headers()));
    }
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        eyre::bail!(
            "github returned status {}: {}",
            status.as_u16(),
            body.trim()
        );
    }

    response
        .json()
        .await
        .map_err(|err| eyre::eyre!("failed to decode github response: {err}"))
}

async fn github_fetch_stars(
    client: &reqwest::Client,
    node_ids: &[String],
    token: &str,
) -> eyre::Result<HashMap<String, usize>> {
    if node_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let body = json!({
        "query": "query($ids: [ID!]!) { nodes(ids: $ids) { ... on Repository { id stargazerCount } } }",
        "variables": { "ids": node_ids }
    });
    let response = client
        .post("https://api.github.com/graphql")
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .header(CONTENT_TYPE, "application/json")
        .json(&body)
        .send()
        .await?;
    if !response.status().is_success() {
        eyre::bail!("graphql status {}", response.status().as_u16());
    }

    #[derive(Deserialize)]
    struct GraphqlResponse {
        data: GraphqlData,
    }
    #[derive(Deserialize)]
    struct GraphqlData {
        nodes: Vec<GraphqlNode>,
    }
    #[derive(Deserialize)]
    struct GraphqlNode {
        id: String,
        #[serde(rename = "stargazerCount", default)]
        stargazer_count: usize,
    }

    let response: GraphqlResponse = response.json().await?;
    Ok(response
        .data
        .nodes
        .into_iter()
        .filter(|node| !node.id.is_empty())
        .map(|node| (node.id, node.stargazer_count))
        .collect())
}

fn extract_matched_line(fragment: &str, matches: &[GithubMatchRange]) -> String {
    if let Some(range) = matches.first()
        && let Some(offset) = range.indices.first().copied()
        && offset < fragment.len()
    {
        let start = fragment[..offset]
            .rfind('\n')
            .map(|idx| idx + 1)
            .unwrap_or(0);
        let end = fragment[offset..]
            .find('\n')
            .map(|idx| offset + idx)
            .unwrap_or(fragment.len());
        return fragment[start..end].trim().to_string();
    }
    fragment
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or(fragment.trim())
        .to_string()
}

fn github_rate_limit_error(status: StatusCode, headers: &HeaderMap) -> String {
    github_rate_limit_error_at(status, headers, Utc::now())
}

fn github_rate_limit_error_at(
    status: StatusCode,
    headers: &HeaderMap,
    now: DateTime<Utc>,
) -> String {
    let Some(reset) = headers.get("X-RateLimit-Reset") else {
        return format!("github: rate limited (status {})", status.as_u16());
    };
    let Ok(reset) = reset.to_str() else {
        return format!("github: rate limited (status {})", status.as_u16());
    };
    let Ok(reset) = reset.parse::<i64>() else {
        return format!("github: rate limited (status {})", status.as_u16());
    };
    let Some(reset_at) = DateTime::<Utc>::from_timestamp(reset, 0) else {
        return format!("github: rate limited (status {})", status.as_u16());
    };

    let wait_seconds = reset_at.signed_duration_since(now).num_seconds().max(0);
    format!(
        "github: rate limited (30 req/min on code search). Resets in {} at {}",
        format_seconds_duration(wait_seconds),
        reset_at.format("%H:%M:%S")
    )
}

fn format_seconds_duration(total_seconds: i64) -> String {
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;

    if hours > 0 {
        format!("{hours}h{minutes}m{seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m{seconds}s")
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use reqwest::StatusCode;
    use reqwest::header::HeaderMap;

    use super::{
        extract_grep_snippet, format_seconds_duration, github_rate_limit_error_at,
        normalize_grep_lang, parse_snippet_line,
    };

    #[test]
    fn parses_snippet_line() {
        assert_eq!(parse_snippet_line("--- Snippet 1 (Line 42) ---"), 42);
    }

    #[test]
    fn extracts_matching_grep_line() {
        let lines = ["alpha", "needle here", "omega"];
        assert_eq!(
            extract_grep_snippet(&lines, 10, "needle"),
            ("needle here".to_string(), 11)
        );
    }

    #[test]
    fn normalizes_grep_language_aliases() {
        assert_eq!(normalize_grep_lang("rs"), vec!["Rust".to_string()]);
    }

    #[test]
    fn formats_github_rate_limit_reset_header() {
        let mut headers = HeaderMap::new();
        headers.insert("X-RateLimit-Reset", "1700000065".parse().unwrap());
        let now = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();

        let error = github_rate_limit_error_at(StatusCode::FORBIDDEN, &headers, now);

        assert_eq!(
            error,
            "github: rate limited (30 req/min on code search). Resets in 1m5s at 22:14:25"
        );
    }

    #[test]
    fn formats_github_rate_limit_without_reset_header() {
        let now = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();

        let error =
            github_rate_limit_error_at(StatusCode::TOO_MANY_REQUESTS, &HeaderMap::new(), now);

        assert_eq!(error, "github: rate limited (status 429)");
    }

    #[test]
    fn formats_seconds_as_go_style_duration() {
        assert_eq!(format_seconds_duration(5), "5s");
        assert_eq!(format_seconds_duration(65), "1m5s");
        assert_eq!(format_seconds_duration(3665), "1h1m5s");
    }
}
