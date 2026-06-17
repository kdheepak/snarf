# snarf

Fast CLI for search, scraping, code search, and site crawling.

`snarf` is built for CLI workflows: it returns text by default, supports JSON output with `--json`,
caches scraped pages, and can use a browser when plain HTTP fetches are not enough.

## Examples

Search the web:

```sh
snarf search "rust reqwest timeout"
snarf search "sqlite fts5" --json
```

Scrape a page to markdown:

```sh
snarf scrape https://example.com
```

Search code:

```sh
snarf code "buffer unordered" --lang rust
snarf code "fn main" --backend sourcegraph --regex
```

Crawl a site:

```sh
snarf crawl https://example.com --depth 2
snarf crawl https://example.com --sitemap --background
snarf crawl status
```

## Configuration

Create a config file:

```sh
snarf config init
```

Show the effective config and available backends:

```sh
snarf config --json
```

Set common options:

```sh
snarf config set backend ddg
snarf config set searxng_url http://localhost:8081
snarf config set brave_api_key YOUR_KEY
snarf config set exa_api_key YOUR_KEY
snarf config set code_backend github
snarf config set github_token YOUR_TOKEN
snarf config set browser chrome
```

Defaults:

- Search backend: `ddg`
- Code backend: `github`
- Result limit: `5`
- Cache TTL: `72h`

## Backends

Search backends:

- `ddg`
- `brave`
- `searxng`
- `exa`

Code backends:

- `grepapp`
- `sourcegraph`
- `github`

## Browser Support

Install Chromium into snarf's cache directory:

```sh
snarf browser install
```

## Cache

```sh
snarf cache
snarf cache clear
snarf scrape https://example.com --no-cache
```
