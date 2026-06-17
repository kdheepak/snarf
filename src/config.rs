use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use color_eyre::eyre;
use directories::BaseDirs;
use serde::{Deserialize, Serialize};

use crate::urlrewrite::Rule;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub backend: String,
    pub searxng_url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub brave_api_key: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub exa_api_key: String,
    pub limit: usize,
    pub cache_ttl: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub browser: String,
    #[serde(default)]
    pub code_backend: String,
    #[serde(default)]
    pub docs_backend: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub context7_api_key: String,
    #[serde(default)]
    pub sourcegraph_url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub github_token: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub url_rewrites: Vec<Rule>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            backend: "ddg".to_string(),
            searxng_url: "http://localhost:8081".to_string(),
            brave_api_key: String::new(),
            exa_api_key: String::new(),
            limit: 5,
            cache_ttl: "72h".to_string(),
            browser: String::new(),
            code_backend: "grepapp".to_string(),
            docs_backend: "context7".to_string(),
            context7_api_key: String::new(),
            sourcegraph_url: "https://sourcegraph.com".to_string(),
            github_token: String::new(),
            url_rewrites: Vec::new(),
        }
    }
}

impl AppConfig {
    pub fn load() -> Self {
        let path = match config_path() {
            Ok(path) => path,
            Err(_) => return Self::default(),
        };

        let data = match fs::read_to_string(path) {
            Ok(data) => data,
            Err(_) => return Self::default(),
        };

        let mut config = Self::default();
        match serde_json::from_str::<serde_json::Value>(&data) {
            Ok(value) => {
                merge_config_value(&mut config, value);
                config
            }
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> eyre::Result<()> {
        let path = config_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_string_pretty(self)?;
        write_file_atomic(&path, &format!("{data}\n"))?;
        Ok(())
    }

    pub fn resolve_github_token(&self) -> (String, String) {
        if !self.github_token.is_empty() {
            return (self.github_token.clone(), "config".to_string());
        }
        if let Ok(token) = std::env::var("GITHUB_TOKEN")
            && !token.is_empty()
        {
            return (token, "env".to_string());
        }
        if let Ok(token) = std::env::var("GH_TOKEN")
            && !token.is_empty()
        {
            return (token, "env".to_string());
        }
        if let Some(token) = github_token_from_gh("gh", Duration::from_secs(2)) {
            return (token, "gh-cli".to_string());
        }
        (String::new(), "none".to_string())
    }
}

fn github_token_from_gh(command: impl AsRef<std::ffi::OsStr>, timeout: Duration) -> Option<String> {
    let mut child = Command::new(command)
        .args(["auth", "token"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut stdout = String::new();
                child.stdout.take()?.read_to_string(&mut stdout).ok()?;
                let token = stdout.trim().to_string();
                return if token.is_empty() { None } else { Some(token) };
            }
            Ok(None) => {
                if started.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return None,
        }
    }
}

fn merge_config_value(config: &mut AppConfig, value: serde_json::Value) {
    let object = match value.as_object() {
        Some(object) => object,
        None => return,
    };

    if let Some(value) = object.get("backend").and_then(serde_json::Value::as_str) {
        config.backend = value.to_string();
    }
    if let Some(value) = object
        .get("searxng_url")
        .and_then(serde_json::Value::as_str)
    {
        config.searxng_url = value.to_string();
    }
    if let Some(value) = object
        .get("brave_api_key")
        .and_then(serde_json::Value::as_str)
    {
        config.brave_api_key = value.to_string();
    }
    if let Some(value) = object
        .get("exa_api_key")
        .and_then(serde_json::Value::as_str)
    {
        config.exa_api_key = value.to_string();
    }
    if let Some(value) = object.get("limit").and_then(serde_json::Value::as_u64) {
        config.limit = value as usize;
    }
    if let Some(value) = object.get("cache_ttl").and_then(serde_json::Value::as_str) {
        config.cache_ttl = value.to_string();
    }
    if let Some(value) = object.get("browser").and_then(serde_json::Value::as_str) {
        config.browser = value.to_string();
    }
    if let Some(value) = object
        .get("code_backend")
        .and_then(serde_json::Value::as_str)
    {
        config.code_backend = value.to_string();
    }
    if let Some(value) = object
        .get("docs_backend")
        .and_then(serde_json::Value::as_str)
    {
        config.docs_backend = value.to_string();
    }
    if let Some(value) = object
        .get("context7_api_key")
        .and_then(serde_json::Value::as_str)
    {
        config.context7_api_key = value.to_string();
    }
    if let Some(value) = object
        .get("sourcegraph_url")
        .and_then(serde_json::Value::as_str)
    {
        config.sourcegraph_url = value.to_string();
    }
    if let Some(value) = object
        .get("github_token")
        .and_then(serde_json::Value::as_str)
    {
        config.github_token = value.to_string();
    }
    if let Some(value) = object.get("url_rewrites")
        && let Ok(rules) = serde_json::from_value::<Vec<Rule>>(value.clone())
    {
        config.url_rewrites = rules;
    }
}

pub fn config_path() -> eyre::Result<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(dir).join("snarf").join("config.json"));
    }
    let dirs = BaseDirs::new().ok_or_else(|| eyre::eyre!("cannot locate config dir"))?;
    Ok(dirs.config_dir().join("snarf").join("config.json"))
}

pub fn cache_dir() -> eyre::Result<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_CACHE_HOME") {
        let dir = PathBuf::from(dir).join("snarf");
        fs::create_dir_all(&dir)?;
        return Ok(dir);
    }
    let dirs = BaseDirs::new().ok_or_else(|| eyre::eyre!("cannot locate cache dir"))?;
    let dir = dirs.cache_dir().join("snarf");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn write_file_atomic(path: &Path, contents: &str) -> eyre::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp_path = path.with_extension("json.tmp");
    fs::write(&tmp_path, contents)?;
    fs::rename(tmp_path, path)?;
    Ok(())
}

pub fn available_backends() -> [&'static str; 4] {
    ["brave", "ddg", "searxng", "exa"]
}

pub fn available_code_backends() -> [&'static str; 3] {
    ["grepapp", "sourcegraph", "github"]
}

pub fn available_doc_backends() -> [&'static str; 2] {
    ["context7", "local"]
}

pub fn parse_duration(value: &str) -> eyre::Result<Duration> {
    let value = value.trim();
    if value.is_empty() {
        eyre::bail!("duration is empty");
    }
    if let Some(rest) = value.strip_prefix('-')
        && !rest.is_empty()
    {
        eyre::bail!("negative durations are not supported");
    }

    let value = value.strip_prefix('+').unwrap_or(value);
    let mut rest = value;
    let mut total_seconds = 0.0f64;

    while !rest.is_empty() {
        let number_len = duration_number_len(rest);
        if number_len == 0 {
            eyre::bail!("duration segment is missing a number");
        }

        let number = rest[..number_len].parse::<f64>()?;
        rest = &rest[number_len..];
        let Some((unit, tail)) = take_duration_unit(rest) else {
            eyre::bail!("duration segment is missing a unit");
        };
        total_seconds += number * duration_unit_seconds(unit);
        rest = tail;
    }

    Duration::try_from_secs_f64(total_seconds)
        .map_err(|err| eyre::eyre!("duration is out of range: {err}"))
}

fn duration_number_len(value: &str) -> usize {
    let bytes = value.as_bytes();
    let mut index = 0;
    let mut saw_digit = false;

    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
        saw_digit = true;
    }

    if index < bytes.len() && bytes[index] == b'.' {
        index += 1;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
            saw_digit = true;
        }
    }

    if saw_digit { index } else { 0 }
}

fn take_duration_unit(value: &str) -> Option<(&str, &str)> {
    for unit in ["ns", "us", "\u{00B5}s", "\u{03BC}s", "ms", "s", "m", "h"] {
        if let Some(rest) = value.strip_prefix(unit) {
            return Some((unit, rest));
        }
    }
    None
}

fn duration_unit_seconds(unit: &str) -> f64 {
    match unit {
        "ns" => 0.000_000_001,
        "us" | "\u{00B5}s" | "\u{03BC}s" => 0.000_001,
        "ms" => 0.001,
        "s" => 1.0,
        "m" => 60.0,
        "h" => 60.0 * 60.0,
        _ => unreachable!("duration unit is selected from a static list"),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use crate::urlrewrite::Rule;

    use super::{AppConfig, github_token_from_gh, parse_duration, write_file_atomic};

    static NEXT_CONFIG_TEST_ID: AtomicUsize = AtomicUsize::new(0);

    #[test]
    fn parses_go_style_durations_used_by_scrape() {
        assert_eq!(parse_duration("30m").unwrap(), Duration::from_secs(1800));
        assert_eq!(parse_duration("72h").unwrap(), Duration::from_secs(259200));
        assert_eq!(parse_duration("10s").unwrap(), Duration::from_secs(10));
        assert_eq!(parse_duration("1h30m").unwrap(), Duration::from_secs(5400));
        assert_eq!(parse_duration("1.5h").unwrap(), Duration::from_secs(5400));
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(
            parse_duration("1m500ms").unwrap(),
            Duration::from_millis(60_500)
        );
        assert_eq!(parse_duration("1us").unwrap(), Duration::from_micros(1));
        assert_eq!(
            parse_duration("1\u{00B5}s").unwrap(),
            Duration::from_micros(1)
        );
        assert_eq!(
            parse_duration("1\u{03BC}s").unwrap(),
            Duration::from_micros(1)
        );
    }

    #[test]
    fn rejects_non_go_style_durations() {
        assert!(parse_duration("10").is_err());
        assert!(parse_duration("1d").is_err());
        assert!(parse_duration("1h nope").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn gh_token_fallback_returns_trimmed_stdout() {
        let gh = fake_gh("ok", "printf '  gh-token\\n'\n");

        let token = github_token_from_gh(&gh, Duration::from_secs(2));

        assert_eq!(token.as_deref(), Some("gh-token"));
    }

    #[cfg(unix)]
    #[test]
    fn gh_token_fallback_times_out() {
        let gh = fake_gh("timeout", "while :; do sleep 1; done\n");
        let started = std::time::Instant::now();

        let token = github_token_from_gh(&gh, Duration::from_millis(50));

        assert_eq!(token, None);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[cfg(unix)]
    fn fake_gh(name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let id = NEXT_CONFIG_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("config-tests")
            .join(format!("{}-{id}", std::process::id()));
        fs::create_dir_all(&root).expect("config test dir exists");
        let path = root.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}")).expect("fake gh script is written");
        let mut permissions = fs::metadata(&path).expect("fake gh metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("fake gh is executable");
        path
    }

    #[test]
    fn defaults_to_ddg_search() {
        assert_eq!(super::AppConfig::default().backend, "ddg");
    }

    #[test]
    fn defaults_to_empty_url_rewrites() {
        assert!(AppConfig::default().url_rewrites.is_empty());
    }

    #[test]
    fn config_json_round_trips_url_rewrites() {
        let config = AppConfig {
            url_rewrites: vec![Rule {
                r#match: r"^https?://www\.reddit\.com/(.*)$".to_string(),
                replace: "https://old.reddit.com/$1".to_string(),
            }],
            ..AppConfig::default()
        };

        let data = serde_json::to_string(&config).unwrap();
        let decoded: AppConfig = serde_json::from_str(&data).unwrap();

        assert_eq!(decoded.url_rewrites.len(), 1);
        assert_eq!(
            decoded.url_rewrites[0].r#match,
            config.url_rewrites[0].r#match
        );
        assert_eq!(
            decoded.url_rewrites[0].replace,
            config.url_rewrites[0].replace
        );
    }

    #[test]
    fn config_json_omits_empty_url_rewrites() {
        let data = serde_json::to_string(&AppConfig::default()).unwrap();
        assert!(!data.contains("url_rewrites"));
    }

    #[test]
    fn atomic_write_replaces_file_and_removes_temp_file() {
        let id = NEXT_CONFIG_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("config-tests")
            .join(format!("atomic-{}-{id}", std::process::id()));
        let path = root.join("config.json");

        write_file_atomic(&path, "first\n").expect("first write succeeds");
        write_file_atomic(&path, "second\n").expect("second write succeeds");

        assert_eq!(fs::read_to_string(&path).unwrap(), "second\n");
        assert!(!path.with_extension("json.tmp").exists());
    }
}
