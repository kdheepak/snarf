use std::sync::OnceLock;

use color_eyre::eyre;
use regex::Regex;
use scraper::{ElementRef, Html, Selector};

#[derive(Debug, Clone, Default)]
pub struct Extracted {
    pub title: String,
    pub markdown: String,
}

pub fn extract(_page_url: &str, raw_html: &str) -> eyre::Result<Extracted> {
    let document = Html::parse_document(raw_html);
    let title = title_from_document(&document);

    for selector in ["article", "main", "[role=main]"] {
        if let Some(markdown) = best_markdown_for_selector(&document, selector) {
            return Ok(Extracted { title, markdown });
        }
    }

    if let Some(markdown) = best_content_candidate_markdown(&document) {
        return Ok(Extracted { title, markdown });
    }

    if let Some(markdown) = best_markdown_for_selector(&document, "body") {
        return Ok(Extracted { title, markdown });
    }

    let markdown = html2md::parse_html(raw_html).trim().to_string();
    Ok(Extracted { title, markdown })
}

fn best_markdown_for_selector(document: &Html, selector: &str) -> Option<String> {
    let selector = Selector::parse(selector).expect("static selector is valid");
    let mut best = None;

    for element in document.select(&selector) {
        let markdown = html2md::parse_html(&element.html()).trim().to_string();
        if markdown.is_empty() {
            continue;
        }
        let score = normalize_whitespace(&element.text().collect::<Vec<_>>().join(" "))
            .chars()
            .count();
        let replace = best
            .as_ref()
            .map(|(best_score, _)| score > *best_score)
            .unwrap_or(true);
        if replace {
            best = Some((score, markdown));
        }
    }

    best.map(|(_, markdown)| markdown)
}

fn best_content_candidate_markdown(document: &Html) -> Option<String> {
    let candidate_selector = Selector::parse("section, div").expect("static selector is valid");
    let link_selector = Selector::parse("a[href]").expect("static selector is valid");
    let block_selector =
        Selector::parse("p, pre, blockquote, table, ul, ol").expect("static selector is valid");
    let mut best = None;

    for element in document.select(&candidate_selector) {
        let Some(score) = score_content_candidate(&element, &link_selector, &block_selector) else {
            continue;
        };
        let markdown = html2md::parse_html(&element.html()).trim().to_string();
        if markdown.is_empty() {
            continue;
        }
        let replace = best
            .as_ref()
            .map(|(best_score, _)| score > *best_score)
            .unwrap_or(true);
        if replace {
            best = Some((score, markdown));
        }
    }

    best.map(|(_, markdown)| markdown)
}

fn score_content_candidate(
    element: &ElementRef<'_>,
    link_selector: &Selector,
    block_selector: &Selector,
) -> Option<i64> {
    let text = normalize_whitespace(&element.text().collect::<Vec<_>>().join(" "));
    let char_count = text.chars().count();
    let word_count = text.split_whitespace().count();
    if char_count < 40 && word_count < 8 {
        return None;
    }

    let identity = candidate_identity(element);
    let positive_hint = contains_any(&identity, POSITIVE_CONTENT_HINTS);
    let negative_hint = contains_any(&identity, NEGATIVE_CONTENT_HINTS);
    if negative_hint && !positive_hint {
        return None;
    }

    let link_chars: usize = element
        .select(link_selector)
        .map(|link| {
            normalize_whitespace(&link.text().collect::<Vec<_>>().join(" "))
                .chars()
                .count()
        })
        .sum();
    if char_count > 0 && link_chars as f64 / char_count as f64 > 0.45 && !positive_hint {
        return None;
    }

    let block_count = element.select(block_selector).count();
    let mut score = char_count as i64 + (block_count as i64 * 40);
    if positive_hint {
        score += 300;
    }
    if negative_hint {
        score -= 250;
    }
    score -= link_chars as i64 * 2;
    (score > 0).then_some(score)
}

fn candidate_identity(element: &ElementRef<'_>) -> String {
    ["id", "class", "role", "itemprop"]
        .iter()
        .filter_map(|name| element.value().attr(name))
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

const POSITIVE_CONTENT_HINTS: &[&str] = &[
    "article",
    "body",
    "content",
    "doc",
    "documentation",
    "entry",
    "main",
    "markdown",
    "post",
    "prose",
    "story",
];

const NEGATIVE_CONTENT_HINTS: &[&str] = &[
    "ad-",
    "ads",
    "advert",
    "aside",
    "banner",
    "breadcrumb",
    "comment",
    "cookie",
    "footer",
    "header",
    "menu",
    "modal",
    "nav",
    "pagination",
    "promo",
    "related",
    "share",
    "sidebar",
    "social",
];

pub fn extract_selector(raw_html: &str, selector: &str) -> eyre::Result<String> {
    let html = extract_selector_html(raw_html, selector)?;
    if html.is_empty() {
        return Ok(String::new());
    }

    Ok(html2md::parse_html(&html).trim().to_string())
}

pub fn extract_selector_html(raw_html: &str, selector: &str) -> eyre::Result<String> {
    let document = Html::parse_document(raw_html);
    let selector =
        Selector::parse(selector).map_err(|_| eyre::eyre!("invalid selector: {selector}"))?;

    let mut html = String::new();
    for element in document.select(&selector) {
        if !html.is_empty() {
            html.push_str("\n\n");
        }
        html.push_str(&element.html());
    }

    if html.is_empty() {
        return Ok(String::new());
    }

    Ok(html.trim().to_string())
}

pub fn title(raw_html: &str) -> String {
    let document = Html::parse_document(raw_html);
    title_from_document(&document)
}

fn title_from_document(document: &Html) -> String {
    let selector = Selector::parse("title").expect("static selector is valid");
    document
        .select(&selector)
        .next()
        .map(|element| normalize_whitespace(&element.text().collect::<Vec<_>>().join(" ")))
        .unwrap_or_default()
}

pub fn detect_js_shell(raw_html: &str) -> &'static str {
    let document = Html::parse_document(raw_html);
    detect_js_shell_from_document(&document, raw_html)
}

fn detect_js_shell_from_document(document: &Html, raw_html: &str) -> &'static str {
    let visible = scan_visible(document);
    if visible.text.len() >= 200 {
        if is_js_loading_page(&visible.text) {
            return "likely_shell";
        }
        return "static";
    }

    if visible.meaningful_blocks > 2 {
        return "ambiguous";
    }

    if has_corrobator(document, &visible, raw_html) {
        "likely_shell"
    } else {
        "ambiguous"
    }
}

#[derive(Debug, Default)]
struct VisibleStats {
    text: String,
    meaningful_blocks: usize,
}

fn scan_visible(document: &Html) -> VisibleStats {
    let selector = Selector::parse(
        "p, article, main, section, h1, h2, h3, h4, h5, h6, li, td, th, dd, dt, blockquote",
    )
    .expect("static selector is valid");

    let mut stats = VisibleStats::default();
    let mut visible = Vec::new();
    for element in document.select(&selector) {
        let text = normalize_whitespace(&element.text().collect::<Vec<_>>().join(" "));
        if text.is_empty() {
            continue;
        }
        let name = element.value().name();
        if matches!(
            name,
            "p" | "li" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "td" | "blockquote" | "dd"
        ) && text.len() > 20
        {
            stats.meaningful_blocks += 1;
        }
        visible.push(text);
    }
    stats.text = visible.join(" ");
    stats
}

fn has_corrobator(document: &Html, visible: &VisibleStats, raw_html: &str) -> bool {
    if noscript_mentions_js(document) || body_requires_javascript(document) {
        return true;
    }
    let lower_html = raw_html.to_ascii_lowercase();
    has_spa_shell_marker(&lower_html)
        || has_low_text_app_shell_marker(&lower_html)
        || high_script_to_text_ratio(document, &visible.text)
}

fn noscript_mentions_js(document: &Html) -> bool {
    let selector = Selector::parse("noscript").expect("static selector is valid");
    document.select(&selector).any(|element| {
        element
            .text()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase()
            .contains("javascript")
    })
}

fn body_requires_javascript(document: &Html) -> bool {
    let selector = Selector::parse("body").expect("static selector is valid");
    let text = document
        .select(&selector)
        .next()
        .map(|element| element.text().collect::<Vec<_>>().join(" "))
        .unwrap_or_default();
    requires_javascript(&text.to_ascii_lowercase())
}

fn high_script_to_text_ratio(document: &Html, visible_text: &str) -> bool {
    let selector = Selector::parse("script").expect("static selector is valid");
    let script_bytes: usize = document
        .select(&selector)
        .map(|element| element.text().collect::<Vec<_>>().join(" ").len())
        .sum();
    script_bytes > visible_text.len().max(1) * 3
}

fn is_js_loading_page(visible_text: &str) -> bool {
    let lower = visible_text.to_ascii_lowercase();
    lower.contains("loading") && requires_javascript(&lower)
}

fn requires_javascript(lower: &str) -> bool {
    lower.contains("enable javascript")
        || lower.contains("requires javascript")
        || lower.contains("ensure javascript")
        || lower.contains("javascript is required")
        || lower.contains("javascript is disabled")
}

fn has_spa_shell_marker(lower_html: &str) -> bool {
    [
        "id=\"__next\"",
        "id='__next'",
        "id=\"__nuxt\"",
        "id='__nuxt'",
        "data-reactroot",
        "ng-version=",
        "<app-root",
        "id=\"___gatsby\"",
        "id='___gatsby'",
        "__next_data__",
        "__nuxt__",
    ]
    .iter()
    .any(|marker| lower_html.contains(marker))
}

fn has_low_text_app_shell_marker(lower_html: &str) -> bool {
    lower_html.contains("id=\"app\"") || lower_html.contains("id='app'")
}

fn normalize_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn strip_markdown(input: &str) -> String {
    let mut fenced = Vec::new();
    let mut output = re_fenced()
        .replace_all(input, |captures: &regex::Captures<'_>| {
            let index = fenced.len();
            fenced.push(captures[0].to_string());
            format!("\0FC{index}\0")
        })
        .to_string();

    for (regex, replacement) in [
        (re_bold(), "$1"),
        (re_bold_alt(), "$1"),
        (re_italic(), "$1"),
        (re_italic_alt(), "$1"),
        (re_image(), ""),
        (re_link(), "$1"),
        (re_heading(), "$1"),
        (re_code(), "$1"),
    ] {
        output = regex.replace_all(&output, replacement).to_string();
    }

    for (index, block) in fenced.iter().enumerate() {
        output = output.replace(&format!("\0FC{index}\0"), block);
    }

    output
}

pub fn post_process(markdown: &str, trim: bool, max_chars: usize) -> String {
    let mut output = if trim {
        strip_markdown(markdown)
    } else {
        markdown.to_string()
    };
    if max_chars > 0 && output.chars().count() > max_chars {
        output = format!(
            "{}\n\n[truncated]",
            output.chars().take(max_chars).collect::<String>()
        );
    }
    output
}

fn re_fenced() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?s)```.*?```").unwrap())
}

fn re_bold() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\*\*(.+?)\*\*").unwrap())
}

fn re_bold_alt() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"__(.+?)__").unwrap())
}

fn re_italic() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\*([^\s*\n][^*\n]*?)\*").unwrap())
}

fn re_italic_alt() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"_([^_\n]+?)_").unwrap())
}

fn re_image() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"!\[[^\]]*\]\([^)]*\)").unwrap())
}

fn re_link() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\[([^\]]+)\]\([^)]*\)").unwrap())
}

fn re_heading() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?m)^#{1,6}\s+(.+)$").unwrap())
}

fn re_code() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"`([^`]+)`").unwrap())
}

#[cfg(test)]
mod tests {
    use super::{detect_js_shell, extract, post_process, strip_markdown};

    #[test]
    fn strips_markdown_without_mangling_lists() {
        for (input, expected) in [
            ("**bold**", "bold"),
            ("__bold__", "bold"),
            ("*italic*", "italic"),
            ("_italic_", "italic"),
            ("`code`", "code"),
            ("[text](https://example.com)", "text"),
            ("![alt](img.png)", ""),
            ("# Heading", "Heading"),
            ("### Sub", "Sub"),
            ("* item one\n* item two", "* item one\n* item two"),
            ("* see *this* now", "* see this now"),
            ("```\nfoo()\n```", "```\nfoo()\n```"),
            (
                "**bold** and *italic* with [link](url)",
                "bold and italic with link",
            ),
            ("***both***", "both"),
        ] {
            assert_eq!(strip_markdown(input), expected, "input: {input:?}");
        }
    }

    #[test]
    fn extract_allows_empty_valid_html() {
        let extracted = extract("https://example.com/empty", "<html><body></body></html>")
            .expect("empty valid HTML still extracts");

        assert_eq!(extracted.title, "");
        assert_eq!(extracted.markdown, "");
    }

    #[test]
    fn extract_prefers_largest_article_candidate() {
        let html = r#"
            <html>
                <head><title>Example</title></head>
                <body>
                    <article><p>Short related item.</p></article>
                    <article>
                        <h1>Main Story</h1>
                        <p>This is the full article body with enough content to clearly beat a short related item teaser.</p>
                        <p>The second paragraph keeps the selected candidate focused on the main story.</p>
                    </article>
                </body>
            </html>
        "#;

        let extracted = extract("https://example.com/story", html).expect("article extracts");

        assert_eq!(extracted.title, "Example");
        assert!(extracted.markdown.contains("Main Story"));
        assert!(extracted.markdown.contains("full article body"));
        assert!(!extracted.markdown.contains("Short related item"));
    }

    #[test]
    fn extract_prefers_content_div_over_navigation_noise() {
        let html = r#"
            <html>
                <head><title>Article Page</title></head>
                <body>
                    <div class="site-nav">
                        <a href="/">Home</a>
                        <a href="/pricing">Pricing</a>
                        <a href="/login">Log in</a>
                    </div>
                    <div class="article-content">
                        <h1>Rust extraction notes</h1>
                        <p>This paragraph is the real page body with enough text to be recognized as the useful content.</p>
                        <p>A second paragraph gives the extractor a stable content block to score.</p>
                    </div>
                    <div class="footer-links">
                        <a href="/privacy">Privacy</a>
                        <a href="/terms">Terms</a>
                    </div>
                </body>
            </html>
        "#;

        let extracted = extract("https://example.com/post", html).expect("page extracts");

        assert!(extracted.markdown.contains("Rust extraction notes"));
        assert!(extracted.markdown.contains("real page body"));
        assert!(!extracted.markdown.contains("Pricing"));
        assert!(!extracted.markdown.contains("Privacy"));
    }

    #[test]
    fn extract_falls_back_to_body_for_simple_pages() {
        let html = r#"
            <html>
                <head><title>Simple Page</title></head>
                <body>
                    <h1>Simple Body</h1>
                    <p>A small page without article, main, section, or div wrappers still extracts.</p>
                </body>
            </html>
        "#;

        let extracted = extract("https://example.com/simple", html).expect("page extracts");

        assert_eq!(extracted.title, "Simple Page");
        assert!(extracted.markdown.contains("Simple Body"));
        assert!(extracted.markdown.contains("without article"));
    }

    #[test]
    fn detects_obvious_js_shell() {
        assert_eq!(
            detect_js_shell(
                "<html><body><div id=\"__next\"></div><noscript>Enable JavaScript</noscript></body></html>"
            ),
            "likely_shell"
        );
    }

    #[test]
    fn detects_js_shell_cases() {
        for (name, html, expected) in [
            (
                "minimal real content is static",
                r#"
                <!doctype html>
                <html>
                    <head><title>Article</title></head>
                    <body>
                        <main>
                            <article>
                                <h1>Shipping Notes</h1>
                                <p>This page has actual content for extraction and is meant to look like a conventional server-rendered article rather than a JavaScript bootstrap shell with placeholders.</p>
                                <p>The second paragraph adds enough visible text to exceed the threshold, which should cause the detector to classify the document as static even if the markup is otherwise minimal.</p>
                            </article>
                        </main>
                    </body>
                </html>
                "#,
                "static",
            ),
            (
                "salesforce shell is likely shell",
                r#"
                <!doctype html>
                <html>
                    <head>
                        <title>Lightning</title>
                        <script>window.__BOOTSTRAP__ = {"routes":["a","b","c"],"payload":"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"};</script>
                        <script src="/assets/app.js"></script>
                    </head>
                    <body>
                        <div id="app">Loading</div>
                        <noscript>This app requires JavaScript and redirects when JavaScript is disabled.</noscript>
                    </body>
                </html>
                "#,
                "likely_shell",
            ),
            (
                "react spa shell is likely shell",
                r#"
                <!doctype html>
                <html>
                    <head>
                        <title>App</title>
                        <script id="__NEXT_DATA__" type="application/json">
                            {"buildId":"dev","page":"/","props":{"chunks":["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]}}
                        </script>
                    </head>
                    <body>
                        <div id="root"></div>
                    </body>
                </html>
                "#,
                "likely_shell",
            ),
            (
                "short real page is ambiguous",
                r#"
                <!doctype html>
                <html>
                    <head><title>Not Found</title></head>
                    <body>
                        <main>
                            <h1>Not Found</h1>
                            <p>The page you requested could not be located.</p>
                        </main>
                    </body>
                </html>
                "#,
                "ambiguous",
            ),
            (
                "js loading page with fallback description is likely shell",
                r#"
                <!doctype html>
                <html>
                    <head><title>Flowchart Maker</title></head>
                    <body>
                        <div id="geInfo">
                            <h1>Flowchart Maker and Online Diagram Software</h1>
                            <p>draw.io is free online diagram software. You can use it as a flowchart maker, network diagram software, to create UML online, as an ER diagram tool, to design database schema, and more.</p>
                            <h2>Loading... <img src="spin.gif"/></h2>
                            <p>Please ensure JavaScript is enabled.</p>
                        </div>
                        <script src="js/main.js"></script>
                    </body>
                </html>
                "#,
                "likely_shell",
            ),
            (
                "ssr next page with real content is static",
                r#"
                <!doctype html>
                <html>
                    <head>
                        <title>SSR Next</title>
                        <script id="__NEXT_DATA__" type="application/json">
                            {"page":"/docs","props":{"pageProps":{"title":"SSR"}}}
                        </script>
                    </head>
                    <body>
                        <div id="__next">
                            <main>
                                <article>
                                    <h1>Rendered Content</h1>
                                    <p>The initial HTML already includes the full article body, so the detector should treat the document as static even though the page carries the standard Next.js bootstrap data.</p>
                                    <p>This extra paragraph ensures there is comfortably more than two hundred characters of visible text in the extraction selectors and prevents a false positive.</p>
                                </article>
                            </main>
                        </div>
                    </body>
                </html>
                "#,
                "static",
            ),
        ] {
            assert_eq!(detect_js_shell(html), expected, "{name}");
        }
    }

    #[test]
    fn marks_truncated_content() {
        assert_eq!(post_process("abcdef", false, 3), "abc\n\n[truncated]");
    }
}
