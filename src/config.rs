use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use clap::ValueEnum;
use color_eyre::eyre;
use directories::BaseDirs;
use serde::{Deserialize, Serialize};

use crate::fs_atomic;
use crate::urlrewrite::Rule;

macro_rules! backend_enum {
    (
        $vis:vis enum $name:ident {
            $($variant:ident => $value:literal),+ $(,)?
        }
    ) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
        $vis enum $name {
            $(
                #[serde(rename = $value)]
                #[value(name = $value)]
                $variant,
            )+
        }

        impl $name {
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $value,)+
                }
            }

            pub fn parse(value: &str) -> Result<Self, String> {
                <Self as ValueEnum>::from_str(value, true).map_err(|_| {
                    format!(
                        "invalid value {value:?} (valid: {})",
                        Self::values().join(", ")
                    )
                })
            }

            pub fn values() -> Vec<&'static str> {
                Self::value_variants()
                    .iter()
                    .map(|backend| backend.as_str())
                    .collect()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

backend_enum! {
    pub enum SearchBackend {
        Brave => "brave",
        Ddg => "ddg",
        Searxng => "searxng",
        Exa => "exa",
    }
}

backend_enum! {
    pub enum CodeBackend {
        Grepapp => "grepapp",
        Sourcegraph => "sourcegraph",
        Github => "github",
    }
}

backend_enum! {
    pub enum DocsBackend {
        Context7 => "context7",
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub backend: SearchBackend,
    pub searxng_url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub brave_api_key: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub exa_api_key: String,
    pub limit: usize,
    pub cache_ttl: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub browser: String,
    pub code_backend: CodeBackend,
    pub docs_backend: DocsBackend,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub context7_api_key: String,
    pub sourcegraph_url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub github_token: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub url_rewrites: Vec<Rule>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            backend: SearchBackend::Ddg,
            searxng_url: "http://localhost:8081".to_string(),
            brave_api_key: String::new(),
            exa_api_key: String::new(),
            limit: 5,
            cache_ttl: "72h".to_string(),
            browser: String::new(),
            code_backend: CodeBackend::Grepapp,
            docs_backend: DocsBackend::Context7,
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

        serde_json::from_str(&data).unwrap_or_default()
    }

    pub fn save(&self) -> eyre::Result<()> {
        let path = config_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_string_pretty(self)?;
        fs_atomic::write(&path, format!("{data}\n"))?;
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

pub fn available_backends() -> Vec<&'static str> {
    SearchBackend::values()
}

pub fn available_code_backends() -> Vec<&'static str> {
    CodeBackend::values()
}

pub fn available_doc_backends() -> Vec<&'static str> {
    DocsBackend::values()
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

    use super::{
        AppConfig, CodeBackend, DocsBackend, SearchBackend, github_token_from_gh, parse_duration,
    };

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
        assert_eq!(super::AppConfig::default().backend, SearchBackend::Ddg);
    }

    #[test]
    fn defaults_to_empty_url_rewrites() {
        assert!(AppConfig::default().url_rewrites.is_empty());
    }

    #[test]
    fn config_json_uses_defaults_for_missing_fields() {
        let decoded: AppConfig = serde_json::from_str(r#"{"backend":"searxng"}"#).unwrap();

        assert_eq!(decoded.backend, SearchBackend::Searxng);
        assert_eq!(decoded.searxng_url, "http://localhost:8081");
        assert_eq!(decoded.limit, 5);
        assert_eq!(decoded.cache_ttl, "72h");
        assert_eq!(decoded.code_backend, CodeBackend::Grepapp);
        assert_eq!(decoded.docs_backend, DocsBackend::Context7);
        assert_eq!(decoded.sourcegraph_url, "https://sourcegraph.com");
    }

    #[test]
    fn config_json_rejects_wrong_field_types() {
        let decoded = serde_json::from_str::<AppConfig>(r#"{"limit":"5"}"#);

        assert!(decoded.is_err());
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
}
