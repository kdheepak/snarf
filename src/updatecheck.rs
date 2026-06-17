use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use color_eyre::eyre;
use semver::Version;
use serde::{Deserialize, Serialize};

use crate::config;
use crate::fs_atomic;
use crate::http;

const CHECK_TTL_HOURS: i64 = 24;
const FAILED_RETRY_HOURS: i64 = 6;
const NOTIFY_TTL_HOURS: i64 = 24;
const SCHEMA_VERSION: usize = 1;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpdateStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<DateTime<Utc>>,
    pub cache_stale: bool,
    pub available: bool,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub latest_version: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub install_type: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub command: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub release_url: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub current_version: String,
    pub allow_network: bool,
    pub timeout: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CacheState {
    schema_version: usize,
    #[serde(default)]
    current_version: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    last_checked_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    last_check_failed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    latest_version: String,
    #[serde(default)]
    release_url: String,
    update_available: bool,
    #[serde(default)]
    install_type: String,
    #[serde(default)]
    update_command: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    last_notified_at: Option<DateTime<Utc>>,
    #[serde(default)]
    last_notified_version: String,
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
}

#[derive(Debug, Clone)]
struct ReleaseInfo {
    version: String,
    url: String,
}

pub async fn get_status(options: Options) -> UpdateStatus {
    let install_type = detect_install_type();
    let command = render_command(&install_type);
    let release_url = release_url();
    let mut status = UpdateStatus {
        install_type: install_type.clone(),
        command: command.clone(),
        release_url,
        source: "none".to_string(),
        ..UpdateStatus::default()
    };

    if !is_version_checkable(&options.current_version) {
        return status;
    }

    let cached = load_state().ok();
    if let Some(state) = cached.as_ref() {
        if state.schema_version == SCHEMA_VERSION {
            status = status_from_state(state, &options.current_version, &install_type, &command);
            status.cache_stale = is_cache_stale(state);
            if !status.cache_stale || !options.allow_network || in_failure_backoff(state) {
                status.source = "cache".to_string();
                return status;
            }
        }
    } else if !options.allow_network {
        return status;
    }

    if !options.allow_network {
        return status;
    }

    match fetch_latest_release(options.timeout).await {
        Ok(release) => {
            let previous = cached.as_ref();
            let state = CacheState {
                schema_version: SCHEMA_VERSION,
                current_version: options.current_version.clone(),
                last_checked_at: Some(Utc::now()),
                last_check_failed_at: None,
                latest_version: release.version.clone(),
                release_url: release.url,
                update_available: compare_versions(&options.current_version, &release.version) < 0,
                install_type: install_type.clone(),
                update_command: command.clone(),
                last_notified_at: previous.and_then(|state| state.last_notified_at),
                last_notified_version: previous
                    .map(|state| state.last_notified_version.clone())
                    .unwrap_or_default(),
            };
            let _ = save_state(&state);
            let mut status =
                status_from_state(&state, &options.current_version, &install_type, &command);
            status.source = "live".to_string();
            status
        }
        Err(_) => {
            if let Some(mut state) = cached {
                state.last_check_failed_at = Some(Utc::now());
                let _ = save_state(&state);
                let mut status =
                    status_from_state(&state, &options.current_version, &install_type, &command);
                status.cache_stale = true;
                status.source = "cache".to_string();
                status
            } else {
                status
            }
        }
    }
}

fn status_from_state(
    state: &CacheState,
    current_version: &str,
    current_install_type: &str,
    current_command: &str,
) -> UpdateStatus {
    let latest_version = normalize_version(&state.latest_version);
    UpdateStatus {
        checked_at: state.last_checked_at,
        cache_stale: is_cache_stale(state),
        available: compare_versions(current_version, &latest_version) < 0,
        latest_version,
        install_type: current_install_type.to_string(),
        command: command_from_cached_state(
            &state.update_command,
            current_command,
            update_command_override().is_some(),
        ),
        release_url: if state.release_url.is_empty() {
            release_url()
        } else {
            state.release_url.clone()
        },
        source: "cache".to_string(),
    }
}

async fn fetch_latest_release(timeout: Duration) -> eyre::Result<ReleaseInfo> {
    let client = http::snarf_client(timeout)?;
    let release: GitHubRelease = client
        .get(release_api_url())
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let version = normalize_version(&release.tag_name);
    if !is_version_checkable(&version) {
        eyre::bail!("latest release version is not semver");
    }
    Ok(ReleaseInfo {
        version,
        url: if release.html_url.is_empty() {
            release_url()
        } else {
            release.html_url
        },
    })
}

fn load_state() -> eyre::Result<CacheState> {
    let data = fs::read_to_string(cache_path()?)?;
    Ok(serde_json::from_str(&data)?)
}

fn save_state(state: &CacheState) -> eyre::Result<()> {
    let path = cache_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs_atomic::write(&path, format!("{}\n", serde_json::to_string_pretty(state)?))?;
    Ok(())
}

fn cache_path() -> eyre::Result<PathBuf> {
    Ok(config::cache_dir()?.join("update-check.json"))
}

fn is_cache_stale(state: &CacheState) -> bool {
    let Some(last_checked_at) = state.last_checked_at else {
        return true;
    };
    Utc::now()
        .signed_duration_since(last_checked_at)
        .num_hours()
        >= CHECK_TTL_HOURS
}

fn in_failure_backoff(state: &CacheState) -> bool {
    let Some(last_failed_at) = state.last_check_failed_at else {
        return false;
    };
    Utc::now().signed_duration_since(last_failed_at).num_hours() < FAILED_RETRY_HOURS
}

pub fn should_notify(status: &UpdateStatus) -> bool {
    if disabled() || !status.available || status.latest_version.is_empty() {
        return false;
    }

    let Ok(state) = load_state() else {
        return true;
    };
    if state.last_notified_version != status.latest_version {
        return true;
    }
    let Some(last_notified_at) = state.last_notified_at else {
        return true;
    };
    Utc::now()
        .signed_duration_since(last_notified_at)
        .num_hours()
        >= NOTIFY_TTL_HOURS
}

pub fn mark_notified(status: &UpdateStatus) -> eyre::Result<()> {
    if !status.available || status.latest_version.is_empty() {
        return Ok(());
    }

    let mut state = load_state().unwrap_or_else(|_| CacheState {
        schema_version: SCHEMA_VERSION,
        latest_version: status.latest_version.clone(),
        release_url: status.release_url.clone(),
        install_type: status.install_type.clone(),
        update_command: status.command.clone(),
        ..CacheState::default()
    });
    state.last_notified_at = Some(Utc::now());
    state.last_notified_version = status.latest_version.clone();
    save_state(&state)
}

pub fn format_notice(status: &UpdateStatus) -> String {
    if !status.available || status.latest_version.is_empty() {
        return String::new();
    }
    let command = if status.command.is_empty() {
        &status.release_url
    } else {
        &status.command
    };
    format!(
        "A newer snarf is available: {}\nUpdate: {}",
        status.latest_version, command
    )
}

pub fn disabled() -> bool {
    matches!(
        std::env::var("SNARF_NO_UPDATE_NOTIFIER")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn is_version_checkable(version: &str) -> bool {
    Version::parse(&normalize_version(version)).is_ok()
}

fn compare_versions(left: &str, right: &str) -> i8 {
    let Ok(left) = Version::parse(&normalize_version(left)) else {
        return 0;
    };
    let Ok(right) = Version::parse(&normalize_version(right)) else {
        return 0;
    };
    match left.cmp(&right) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

fn normalize_version(version: &str) -> String {
    version.trim().trim_start_matches('v').to_string()
}

fn release_api_url() -> String {
    let repo = env!("CARGO_PKG_REPOSITORY")
        .trim_end_matches('/')
        .trim_start_matches("https://github.com/");
    format!("https://api.github.com/repos/{repo}/releases/latest")
}

fn release_url() -> String {
    format!(
        "{}/releases/latest",
        env!("CARGO_PKG_REPOSITORY").trim_end_matches('/')
    )
}

fn detect_install_type() -> String {
    if let Ok(forced) = std::env::var("SNARF_INSTALL_METHOD") {
        let forced = forced.trim().to_ascii_lowercase();
        if matches!(forced.as_str(), "homebrew" | "cargo" | "manual" | "unknown") {
            return forced;
        }
    }

    let exe = std::env::current_exe()
        .ok()
        .map(|path| path.display().to_string().to_ascii_lowercase())
        .unwrap_or_default();
    if exe.contains("/homebrew/") || exe.contains("/cellar/") {
        "homebrew".to_string()
    } else if exe.contains("/.cargo/bin/") || exe.ends_with("/cargo/bin/snarf") {
        "cargo".to_string()
    } else if exe.ends_with("/snarf") || exe.ends_with("\\snarf.exe") {
        "manual".to_string()
    } else {
        "unknown".to_string()
    }
}

fn render_command(install_type: &str) -> String {
    if let Some(command) = update_command_override() {
        return command;
    }

    match install_type {
        "cargo" => format!(
            "cargo install --git {} --force",
            env!("CARGO_PKG_REPOSITORY")
        ),
        _ => release_url(),
    }
}

fn update_command_override() -> Option<String> {
    let command = std::env::var("SNARF_UPDATE_COMMAND")
        .ok()?
        .trim()
        .to_string();
    (!command.is_empty()).then_some(command)
}

fn command_from_cached_state(
    cached_command: &str,
    current_command: &str,
    override_active: bool,
) -> String {
    if override_active || cached_command.is_empty() {
        current_command.to_string()
    } else {
        cached_command.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        UpdateStatus, command_from_cached_state, compare_versions, format_notice, normalize_version,
    };

    #[test]
    fn normalizes_release_tags() {
        assert_eq!(normalize_version("v1.2.3"), "1.2.3");
        assert_eq!(normalize_version("  1.2.3  "), "1.2.3");
    }

    #[test]
    fn compares_semver_versions() {
        assert_eq!(compare_versions("0.1.0", "0.2.0"), -1);
        assert_eq!(compare_versions("1.0.0", "1.0.0"), 0);
        assert_eq!(compare_versions("2.0.0", "1.0.0"), 1);
    }

    #[test]
    fn formats_snarf_update_notice() {
        let notice = format_notice(&UpdateStatus {
            available: true,
            latest_version: "1.2.3".to_string(),
            command: "cargo install --git https://example.com/snarf --force".to_string(),
            ..UpdateStatus::default()
        });
        assert_eq!(
            notice,
            "A newer snarf is available: 1.2.3\nUpdate: cargo install --git https://example.com/snarf --force"
        );
    }

    #[test]
    fn cached_update_command_respects_current_override() {
        assert_eq!(
            command_from_cached_state("cached command", "override command", true),
            "override command"
        );
        assert_eq!(
            command_from_cached_state("cached command", "current command", false),
            "cached command"
        );
        assert_eq!(
            command_from_cached_state("", "current command", false),
            "current command"
        );
    }
}
