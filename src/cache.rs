use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use color_eyre::eyre;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config;
use crate::fs_atomic;
use crate::types::Page;

#[derive(Debug, Clone)]
pub struct PageCache {
    path: PathBuf,
    ttl: Duration,
}

#[derive(Debug, Clone)]
pub struct CachedPage {
    pub page: Page,
    pub source: String,
    pub expired: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
    t: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    s: String,
    p: Page,
}

impl PageCache {
    pub fn new(ttl: Duration) -> eyre::Result<Self> {
        Ok(Self {
            path: db_path()?,
            ttl,
        })
    }

    #[cfg(test)]
    pub(crate) fn for_path(path: PathBuf, ttl: Duration) -> Self {
        Self { path, ttl }
    }

    pub fn get(&self, url: &str) -> Option<(Page, String)> {
        self.get_any(url)
            .filter(|cached| !cached.expired)
            .map(|cached| (cached.page, cached.source))
    }

    pub fn get_any(&self, url: &str) -> Option<CachedPage> {
        with_cache_lock(&self.path, LockMode::Shared, || {
            let entries = read_entries(&self.path)?;
            let Some(entry) = entries.get(&cache_key(url)) else {
                return Ok(None);
            };
            Ok(Some(CachedPage {
                page: entry.p.clone(),
                source: entry.s.clone(),
                expired: entry_expired(entry, self.ttl),
            }))
        })
        .ok()
        .flatten()
    }

    pub fn put(&self, url: &str, page: &Page, source: &str) {
        let _ = with_cache_lock(&self.path, LockMode::Exclusive, || {
            let mut entries = read_entries(&self.path).unwrap_or_default();
            entries.insert(
                cache_key(url),
                CacheEntry {
                    t: unix_now(),
                    s: source.to_string(),
                    p: page.clone(),
                },
            );
            write_entries(&self.path, &entries)
        });
    }

    pub fn stats(&self) -> (usize, u64) {
        with_cache_lock(&self.path, LockMode::Shared, || {
            let entries = read_entries(&self.path).unwrap_or_default();
            let bytes = fs::metadata(&self.path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            Ok((entries.len(), bytes))
        })
        .unwrap_or((0, 0))
    }

    pub fn clear(&self) -> eyre::Result<()> {
        with_cache_lock(&self.path, LockMode::Exclusive, || {
            write_entries(&self.path, &HashMap::new())
        })
    }
}

pub fn db_path() -> eyre::Result<PathBuf> {
    Ok(config::cache_dir()?.join("cache.json"))
}

pub fn stats() -> eyre::Result<(usize, u64)> {
    Ok(PageCache::new(Duration::from_secs(0))?.stats())
}

pub fn clear() -> eyre::Result<()> {
    PageCache::new(Duration::from_secs(0))?.clear()
}

fn entry_expired(entry: &CacheEntry, ttl: Duration) -> bool {
    if entry.t < 0 {
        return true;
    }
    let cached_at = UNIX_EPOCH + Duration::from_secs(entry.t as u64);
    SystemTime::now()
        .duration_since(cached_at)
        .map(|age| age > ttl)
        .unwrap_or(false)
}

fn read_entries(path: &PathBuf) -> eyre::Result<HashMap<String, CacheEntry>> {
    let data = match fs::read_to_string(path) {
        Ok(data) => data,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(err) => return Err(err.into()),
    };
    Ok(serde_json::from_str(&data)?)
}

fn write_entries(path: &PathBuf, entries: &HashMap<String, CacheEntry>) -> eyre::Result<()> {
    let data = serde_json::to_string(entries)?;
    fs_atomic::write(path, data)
}

#[derive(Debug, Clone, Copy)]
enum LockMode {
    Shared,
    Exclusive,
}

fn with_cache_lock<T>(
    path: &Path,
    mode: LockMode,
    f: impl FnOnce() -> eyre::Result<T>,
) -> eyre::Result<T> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let lock_path = path.with_extension("json.lock");
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?;

    match mode {
        LockMode::Shared => lock_file.lock_shared()?,
        LockMode::Exclusive => lock_file.lock_exclusive()?,
    }

    let result = f();
    let unlock_result = lock_file.unlock();
    match (result, unlock_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(err), _) => Err(err),
        (Ok(_), Err(err)) => Err(err.into()),
    }
}

fn cache_key(url: &str) -> String {
    let digest = Sha256::digest(url.as_bytes());
    hex::encode(&digest[..8])
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

pub fn format_bytes(bytes: u64) -> String {
    if bytes >= 1 << 20 {
        format!("{:.1} MB", bytes as f64 / (1 << 20) as f64)
    } else if bytes >= 1 << 10 {
        format!("{:.1} KB", bytes as f64 / (1 << 10) as f64)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    use crate::scrape::{SOURCE_BROWSER, SOURCE_HTTP};
    use crate::types::Page;

    use super::PageCache;

    static NEXT_CACHE_ID: AtomicUsize = AtomicUsize::new(0);

    fn test_cache(ttl: Duration) -> PageCache {
        let id = NEXT_CACHE_ID.fetch_add(1, Ordering::Relaxed);
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("cache-tests")
            .join(format!("{}-{id}.json", std::process::id()));
        PageCache { path, ttl }
    }

    #[test]
    fn puts_then_gets_pages() {
        let cache = test_cache(Duration::from_secs(60 * 60));
        let page = Page {
            url: "https://example.com/test".to_string(),
            title: "Test Page".to_string(),
            markdown: "# Hello\n\nWorld".to_string(),
            ..Page::default()
        };

        cache.put("https://example.com/test", &page, SOURCE_HTTP);

        let (got, source) = cache
            .get("https://example.com/test")
            .expect("page is cached");
        assert_eq!(source, SOURCE_HTTP);
        assert_eq!(got.url, "https://example.com/test");
        assert_eq!(got.title, "Test Page");
        assert_eq!(got.markdown, "# Hello\n\nWorld");
    }

    #[test]
    fn round_trips_fetch_source() {
        let cache = test_cache(Duration::from_secs(60 * 60));
        let page = Page {
            url: "https://example.com/browser".to_string(),
            title: "Rendered".to_string(),
            ..Page::default()
        };

        cache.put("https://example.com/browser", &page, SOURCE_BROWSER);

        let (_, source) = cache
            .get("https://example.com/browser")
            .expect("page is cached");
        assert_eq!(source, SOURCE_BROWSER);
    }

    #[test]
    fn expires_entries_by_ttl() {
        let cache = test_cache(Duration::from_nanos(1));
        let page = Page {
            url: "https://example.com/expired".to_string(),
            title: "Old".to_string(),
            ..Page::default()
        };

        cache.put("https://example.com/expired", &page, SOURCE_HTTP);
        std::thread::sleep(Duration::from_millis(10));

        assert!(cache.get("https://example.com/expired").is_none());
        let stale = cache
            .get_any("https://example.com/expired")
            .expect("expired entry is still inspectable");
        assert!(stale.expired);
        assert_eq!(stale.page.title, "Old");
        assert_eq!(stale.source, SOURCE_HTTP);
    }

    #[test]
    fn keeps_entries_within_ttl() {
        let cache = test_cache(Duration::from_secs(24 * 60 * 60));
        let page = Page {
            url: "https://example.com/valid".to_string(),
            title: "Fresh".to_string(),
            ..Page::default()
        };

        cache.put("https://example.com/valid", &page, SOURCE_HTTP);

        let (got, _) = cache
            .get("https://example.com/valid")
            .expect("page is cached");
        assert_eq!(got.title, "Fresh");
    }

    #[test]
    fn reports_stats_and_clears_entries() {
        let cache = test_cache(Duration::from_secs(60 * 60));
        assert_eq!(cache.stats().0, 0);

        cache.put(
            "https://example.com/a",
            &Page {
                url: "https://example.com/a".to_string(),
                title: "A".to_string(),
                ..Page::default()
            },
            SOURCE_HTTP,
        );
        cache.put(
            "https://example.com/b",
            &Page {
                url: "https://example.com/b".to_string(),
                title: "B".to_string(),
                ..Page::default()
            },
            SOURCE_HTTP,
        );

        let (entries, bytes) = cache.stats();
        assert_eq!(entries, 2);
        assert!(bytes > 0);

        cache.clear().expect("cache clears");
        assert_eq!(cache.stats().0, 0);
        assert!(cache.get("https://example.com/a").is_none());
    }

    #[test]
    fn keys_different_urls_separately() {
        let cache = test_cache(Duration::from_secs(60 * 60));
        cache.put(
            "https://example.com/page1",
            &Page {
                url: "https://example.com/page1".to_string(),
                title: "Page 1".to_string(),
                markdown: "Content 1".to_string(),
                ..Page::default()
            },
            SOURCE_HTTP,
        );
        cache.put(
            "https://example.com/page2",
            &Page {
                url: "https://example.com/page2".to_string(),
                title: "Page 2".to_string(),
                markdown: "Content 2".to_string(),
                ..Page::default()
            },
            SOURCE_HTTP,
        );

        assert_eq!(
            cache
                .get("https://example.com/page1")
                .expect("page1 is cached")
                .0
                .title,
            "Page 1"
        );
        assert_eq!(
            cache
                .get("https://example.com/page2")
                .expect("page2 is cached")
                .0
                .title,
            "Page 2"
        );
        assert!(cache.get("https://example.com/nonexistent").is_none());
    }

    #[test]
    fn concurrent_writes_keep_all_entries() {
        let cache = Arc::new(test_cache(Duration::from_secs(60 * 60)));
        let writers = 32;
        let barrier = Arc::new(Barrier::new(writers));
        let mut handles = Vec::new();

        for index in 0..writers {
            let cache = Arc::clone(&cache);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                let url = format!("https://example.com/page-{index}");
                cache.put(
                    &url,
                    &Page {
                        url: url.clone(),
                        title: format!("Page {index}"),
                        markdown: format!("Content {index}"),
                        ..Page::default()
                    },
                    SOURCE_HTTP,
                );
            }));
        }

        for handle in handles {
            handle.join().expect("cache writer exits");
        }

        assert_eq!(cache.stats().0, writers);
        for index in 0..writers {
            let url = format!("https://example.com/page-{index}");
            let (page, source) = cache.get(&url).expect("page is cached");
            assert_eq!(source, SOURCE_HTTP);
            assert_eq!(page.title, format!("Page {index}"));
        }
    }
}
