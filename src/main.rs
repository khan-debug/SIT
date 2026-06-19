use dialoguer::{theme::ColorfulTheme, Select};
use reqwest::header::{ACCEPT, ACCEPT_LANGUAGE, AUTHORIZATION, USER_AGENT, HeaderMap};
use scraper::{Html, Selector};
use std::env;
use regex::Regex;

// ─── Distro Detection ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Distro {
    Fedora,   // dnf  → prefers .rpm
    Ubuntu,   // apt  → prefers .deb
    Unknown,  // fall back to .AppImage / .tar.gz only
}

fn detect_distro() -> Distro {
    let content = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let content_lower = content.to_lowercase();

    // ID_LIKE catches derivatives: "fedora" covers RHEL/CentOS/Rocky
    // "debian" covers Ubuntu, Mint, Pop!_OS, etc.
    if content_lower.contains("id_like=\"fedora\"")
        || content_lower.contains("id_like=fedora")
        || content_lower.contains("id=\"fedora\"")
        || content_lower.contains("id=fedora")
        || content_lower.contains("rhel")
        || content_lower.contains("centos")
        || content_lower.contains("rocky")
    {
        Distro::Fedora
    } else if content_lower.contains("id_like=\"debian\"")
        || content_lower.contains("id_like=debian")
        || content_lower.contains("id=\"ubuntu\"")
        || content_lower.contains("id=ubuntu")
        || content_lower.contains("id=\"debian\"")
        || content_lower.contains("id=debian")
        || content_lower.contains("ubuntu")
        || content_lower.contains("debian")
        || content_lower.contains("mint")
        || content_lower.contains("pop!_os")
        || content_lower.contains("elementary")
        || content_lower.contains("kali")
    {
        Distro::Ubuntu
    } else {
        Distro::Unknown
    }
}

fn distro_label(distro: &Distro) -> &'static str {
    match distro {
        Distro::Fedora  => "Fedora/RHEL (dnf)",
        Distro::Ubuntu  => "Ubuntu/Debian (apt)",
        Distro::Unknown => "Unknown (generic)",
    }
}

// ─── Asset Filtering ─────────────────────────────────────────────────────────

/// Returns true when a filename is a valid, installable Linux x86_64 binary.
fn is_valid_asset(name: &str, distro: &Distro) -> bool {
    let n = name.to_lowercase();

    // Hard-reject non-Linux / non-x86_64 targets
    if n.contains("arm64")
        || n.contains("aarch64")
        || n.contains("armv7")
        || n.contains("armhf")
        || n.contains("darwin")
        || n.contains("macos")
        || n.contains("mac")
        || n.contains("win")
        || n.contains(".exe")
        || n.contains(".msi")
        || n.contains(".dmg")
        || n.contains(".pkg")
        || n.contains("musl")           // musl-static builds rarely work on glibc distros
        || n.contains("i686")           // 32-bit
        || n.contains("i386")
        || n.ends_with(".sha256")
        || n.ends_with(".sha512")
        || n.ends_with(".sig")
        || n.ends_with(".asc")
        || n.ends_with(".json")
        || n.ends_with(".txt")
        || n.ends_with(".blockmap")
    {
        return false;
    }

    // Special case: Allow .zip files from common sources that package Linux binaries in zip
    // (e.g., VS Code, JetBrains IDEs)
    if n.ends_with(".zip") {
        // Check if the filename suggests it's a Linux package
        if n.contains("linux") || n.contains("x64") || n.contains("amd64") {
            return true;
        }
        return false;
    }

    // Special case: Reject "setup" and "installer" only if they're not part of a valid package name
    // (e.g., reject "setup.exe" but allow "vscode-setup.tar.gz")
    if (n.contains("setup") || n.contains("installer")) && !n.contains("linux") && !n.contains("x64") && !n.contains("amd64") {
        return false;
    }

    // Require at least one accepted extension
    let accepted = match distro {
        Distro::Fedora  => vec![".rpm", ".appimage", ".tar.gz", ".tar.xz", ".tar.bz2", ".zip"],
        Distro::Ubuntu  => vec![".deb", ".appimage", ".tar.gz", ".tar.xz", ".tar.bz2", ".zip"],
        Distro::Unknown => vec![".appimage", ".tar.gz", ".tar.xz", ".tar.bz2", ".zip"],
    };
    accepted.iter().any(|ext| n.ends_with(ext))
}

/// Sort assets so the distro-native format bubbles to the top.
fn sort_assets(assets: &mut Vec<(String, String)>, distro: &Distro) {
    assets.sort_by_key(|(name, _)| {
        let n = name.to_lowercase();
        match distro {
            Distro::Fedora => {
                if n.ends_with(".rpm")      { 0 }
                else if n.ends_with(".appimage") { 1 }
                else { 2 }
            }
            Distro::Ubuntu => {
                if n.ends_with(".deb")      { 0 }
                else if n.ends_with(".appimage") { 1 }
                else { 2 }
            }
            Distro::Unknown => {
                if n.ends_with(".appimage") { 0 }
                else { 1 }
            }
        }
    });
}

// ─── GitHub Pivot: extract repo from HTML ────────────────────────────────────

/// Scans HTML text for a github.com/owner/repo pattern and returns the first one.
fn extract_github_repo(html: &str) -> Option<String> {
    let re = Regex::new(r#"github\.com/([A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+)"#).ok()?;
    for cap in re.captures_iter(html) {
        let repo = cap[1].to_string();
        // Exclude github infra paths
        if repo.starts_with("github/")
            || repo.contains("topics/")
            || repo.contains("sponsors/")
            || repo.ends_with(".git")
        {
            continue;
        }
        return Some(repo);
    }
    None
}

// ─── Web Scraper: extract download links from a page ─────────────────────────

/// Resolve a potentially-relative link to an absolute URL given the page base.
fn resolve_url(base: &str, href: &str) -> Option<String> {
    let base_url = url::Url::parse(base).ok()?;
    Some(base_url.join(href).ok()?.to_string())
}

/// Extract filename from a URL, stripping query params.
fn extract_fname(url: &str) -> String {
    url.rsplit('/').next().unwrap_or("").split('?').next().unwrap_or("").to_string()
}

/// Scrapes a page for installable binary links.
/// Returns (filename, absolute_url) pairs.
fn scrape_download_links(html: &str, page_url: &str, distro: &Distro) -> Vec<(String, String)> {
    let document = Html::parse_document(html);
    let mut links: Vec<(String, String)> = Vec::new();

    // 1. All <a href="..."> tags
    if let Ok(a_sel) = Selector::parse("a[href]") {
        for el in document.select(&a_sel) {
            if let Some(href) = el.value().attr("href") {
                if let Some(abs) = resolve_url(page_url, href) {
                    let fname = extract_fname(&abs);
                    if is_valid_asset(&fname, distro)
                        && !links.iter().any(|(n, _)| n == &fname) {
                            links.push((fname, abs));
                        }
                }
            }
        }
    }

    // 2. data-href / data-url / data-download attributes (common in modern download buttons)
    // Pre-build selector strings so they outlive the Selector borrows.
    let data_attrs: &[(&str, &str)] = &[
        ("data-href",     "[data-href]"),
        ("data-url",      "[data-url]"),
        ("data-download", "[data-download]"),
        ("data-src",      "[data-src]"),
    ];
    for (attr, sel_str) in data_attrs {
        if let Ok(sel) = Selector::parse(sel_str) {
            for el in document.select(&sel) {
                if let Some(href) = el.value().attr(attr) {
                    if let Some(abs) = resolve_url(page_url, href) {
                        let fname = extract_fname(&abs);
                        if is_valid_asset(&fname, distro)
                            && !links.iter().any(|(n, _)| n == &fname) {
                                links.push((fname, abs));
                            }
                    }
                }
            }
        }
    }

    // 3. Regex scan raw HTML for anything that looks like a binary URL
    //    Catches URLs embedded in <script> blocks, onclick="...", window.location, etc.
    let url_re = Regex::new(
        r#"https?://[^\s"'<>]+\.(?:AppImage|deb|rpm|tar\.gz|tar\.xz|tar\.bz2|zip)"#
    ).unwrap();
    for cap in url_re.captures_iter(html) {
        let abs = cap[0].to_string();
        let fname = extract_fname(&abs);
        if is_valid_asset(&fname, distro) && !links.iter().any(|(n, _)| n == &fname) {
            links.push((fname, abs));
        }
    }

    // 4. Download button attributes + class/ID selectors
    let onclick_re = Regex::new(r#"https?://[^\s'"\)]+\.(?:AppImage|deb|rpm|tar\.gz|tar\.xz|tar\.bz2|zip)"#).unwrap();

    let button_sels: &[&str] = &[
        "[onclick]", "[data-url]", "[data-download-url]",
        "[data-os]", "[data-href]", "[data-download]", "[data-src]",
        "button[class*='download']", "a[class*='download']",
        "button[id*='download']",  "a[id*='download']",
    ];

    for sel_str in button_sels {
        if let Ok(sel) = Selector::parse(sel_str) {
            for el in document.select(&sel) {
                // For data-os="linux" elements, look at href + data-download-url
                if let Some(os_val) = el.value().attr("data-os") {
                    if os_val.to_lowercase().contains("linux") {
                        for attr in &["href", "data-download-url"] {
                            if let Some(href) = el.value().attr(attr) {
                                if let Some(abs) = resolve_url(page_url, href) {
                                    let fname = extract_fname(&abs);
                                    if is_valid_asset(&fname, distro) && !links.iter().any(|(n, _)| n == &fname) {
                                        links.push((fname, abs));
                                    }
                                }
                            }
                        }
                    }
                }
                // Try all URL-bearing data attributes
                for attr in &["data-href", "data-url", "data-download", "data-src", "data-download-url", "href"] {
                    if let Some(val) = el.value().attr(attr) {
                        if let Some(abs) = resolve_url(page_url, val) {
                            let fname = extract_fname(&abs);
                            if is_valid_asset(&fname, distro) && !links.iter().any(|(n, _)| n == &fname) {
                                links.push((fname, abs));
                            }
                        }
                    }
                }
                // Extract URL from onclick handlers
                if let Some(val) = el.value().attr("onclick") {
                    if let Some(cap) = onclick_re.captures(val) {
                        let abs = cap[0].to_string();
                        let fname = extract_fname(&abs);
                        if is_valid_asset(&fname, distro) && !links.iter().any(|(n, _)| n == &fname) {
                            links.push((fname, abs));
                        }
                    }
                }
            }
        }
    }

    links
}

// ─── Structs ──────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct AppMatch {
    name:     String,
    platform: String,
    url:      String,
}

/// A shell install script detected on a page (e.g. `curl ... | bash`).
struct CurlInstall {
    url:          String,
    display_name: String,
}

/// Returns true if the page is a DuckDuckGo CAPTCHA challenge (not real results).
fn is_ddg_captcha(html: &str) -> bool {
    html.contains("anomaly-modal") && html.contains("challenge")
}

/// Scrape a page for curl/wget install scripts.
/// Looks for: links to .sh files, and `curl/wget ... | bash` patterns in code blocks.
fn extract_curl_installs(html: &str, base_url: &str) -> Vec<CurlInstall> {
    let mut installs: Vec<CurlInstall> = Vec::new();
    let document = Html::parse_document(html);

    // 1. <a href="..."> pointing to .sh scripts
    if let Ok(a_sel) = Selector::parse("a[href]") {
        for el in document.select(&a_sel) {
            if let Some(href) = el.value().attr("href") {
                let href_lower = href.to_lowercase();
                if href_lower.ends_with(".sh") {
                    if let Some(abs) = resolve_url(base_url, href) {
                        if !installs.iter().any(|i| i.url == abs) {
                            installs.push(CurlInstall {
                                display_name: format!("curl {} | bash", abs),
                                url:          abs,
                            });
                        }
                    }
                }
            }
        }
    }

    // 2. Regex: curl/wget ... | bash/zsh patterns in code blocks
    let cmd_re = Regex::new(
        r#"(?:curl|wget)\s+[^\s|<>"]+\s*(?:\|[^\s<>"]*)*\s*\|\s*(?:sudo\s+)?(?:ba)?sh"#
    ).unwrap();
    let url_re = Regex::new(r#"(https?://[^\s|<>"]+)"#).unwrap();
    for cap in cmd_re.captures_iter(html) {
        let cmd = cap[0].trim().to_string();
        // Extract the URL from the command
        if let Some(url_cap) = url_re.captures(&cmd) {
            let script_url = url_cap[1].to_string();
            if !installs.iter().any(|i| i.url == script_url) {
                installs.push(CurlInstall {
                    display_name: cmd,
                    url:          script_url,
                });
            }
        }
    }

    installs
}

// ─── Search Parser ──────────────────────────────────────────────────────────

fn parse_search_html(html: &str, query: &str) -> Vec<AppMatch> {
    let document = Html::parse_document(html);
    let mut seen_urls: Vec<String> = Vec::new();
    let query_lower = query.to_lowercase();
    let query_words: Vec<&str> = query_lower.split_whitespace().collect();

    // Sites that are tutorials, not actual download pages
    let tutorial_sites = [
        "linuxcapable.com", "itsfoss.com", "linuxconfig.org", "pkgs.org",
        "libtechnophile.blogspot.com", "tecmint.com", "fosslinux.com",
        "linuxbite.com", "debugpoint.com", "addictivetips.com",
        "cyberpanel.net", "support.brave.app",
    ];

    // Collect (score, AppMatch) for sorting
    let mut scored: Vec<(i32, AppMatch)> = Vec::new();

    let link_sel = Selector::parse("a[href]").unwrap();
    for el in document.select(&link_sel) {
        // DDG uses data-href, Brave uses href — try both
        let href = el.value().attr("data-href")
            .or_else(|| el.value().attr("href"))
            .unwrap_or("")
            .to_string();

        if href.is_empty()
            || href.contains("duckduckgo.com")
            || href.contains("search.brave.com")
            || href.contains("search.brave.")
            || href.contains("github.com")
            || href.contains("flathub.org")
            || href.contains("youtube.com")
            || href.contains("reddit.com")
            || href.contains("stackoverflow.com")
            || href.contains("wikipedia.org")
            || href.contains("snapcraft.io")
        {
            continue;
        }

        if !href.starts_with("http") { continue; }

        let parsed = match url::Url::parse(&href) {
            Ok(u) => u,
            Err(_) => continue,
        };
        let domain = parsed.host_str().unwrap_or("").to_string();
        if domain.is_empty() { continue; }

        // Extract link text (the visible page title from the search result)
        let title = el.text().collect::<Vec<_>>().join(" ").trim().to_string();

        // Filter: skip if title contains obvious non-download signals
        let lower_title = title.to_lowercase();
        let skip_keywords = ["sign in", "sign up", "login", "register", "cart",
            "shopping", "pricing", "subscribe", "jobs", "careers", "contact"];
        if skip_keywords.iter().any(|k| lower_title.contains(k)) {
            continue;
        }

        let dedup_key = if href.contains("github.com") {
            href.clone()
        } else {
            domain.clone()
        };
        if seen_urls.contains(&dedup_key) { continue; }
        seen_urls.push(dedup_key);

        // ── Score relevance ─────────────────────────────────────────────────
        let mut score: i32 = 0;

        // +10 per query word found in title or URL
        for w in &query_words {
            if lower_title.contains(*w) { score += 10; }
            if href.to_lowercase().contains(*w) { score += 5; }
        }

        // +15 if the URL path or title contains "download"
        let path = parsed.path().to_lowercase();
        if lower_title.contains("download") || path.contains("download") {
            score += 15;
        }
        if lower_title.contains("install") || path.contains("install") {
            score += 5;
        }

        // +10 if the domain name contains the query (e.g. obsidian.md for "obsidian")
        if query_words.iter().any(|w| domain.contains(w)) {
            score += 10;
        }

        // +5 for a "/" or "/download" path (home page or download page)
        if path == "/" || path == "/download" || path == "/downloads" {
            score += 5;
        }

        // -30 for known tutorial/blog sites
        if tutorial_sites.iter().any(|s| domain.contains(s)) {
            score -= 30;
        }

        let display_name = if !title.is_empty() && title.len() > 3 {
            let t = if title.chars().count() > 55 {
                format!("{}…", title.chars().take(52).collect::<String>())
            } else {
                title.clone()
            };
            format!("🌐 {} ({})", t, domain)
        } else {
            let clean = domain.replace(".com", "").replace(".org", "")
                .replace(".io", "").replace("-", " ").trim().to_string();
            let cap = clean.split_whitespace()
                .map(|w| {
                    let mut c = w.chars();
                    match c.next() {
                        None => String::new(),
                        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
            format!("🌐 {} ({})", cap, domain)
        };

        scored.push((score, AppMatch {
            name:     display_name,
            url:      href,
            platform: "Web".to_string(),
        }));
    }

    // Sort by score descending
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.truncate(5);
    scored.into_iter().map(|(_, m)| m).collect()
}

// ─── Main ─────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let distro = detect_distro();
    println!("🖥  Detected system: {}", distro_label(&distro));

    let args: Vec<String> = env::args().collect();
    let raw_input = if args.len() < 2 {
        println!("Enter application keyword or URL:");
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        input.trim().to_string()
    } else {
        args[1..].join(" ")
    };

    // ── Direct URL mode ───────────────────────────────────────────────────────
    if raw_input.starts_with("http://") || raw_input.starts_with("https://") {
        println!("🌐 Direct URL mode — scraping {}", raw_input);
        let client = build_client()?;
        handle_direct_url(&client, &raw_input, &distro).await?;
        return Ok(());
    }

    let search_query = raw_input;

    // ── Build HTTP client ─────────────────────────────────────────────────────
    let client = build_client()?;

    println!("🔍 Searching GitHub and Web for '{}'...", search_query);

    // ── Concurrent search ─────────────────────────────────────────────────────

    // 1. GitHub — search by name so irrelevant repos don't leak in
    let gh_url = format!(
        "https://api.github.com/search/repositories?q={}+in:name&sort=stars&order=desc",
        urlencoding::encode(&search_query)
    );
    let gh_req = client.get(&gh_url).send();

    // 2. Web search — concurrent with GitHub
    let brave_url1 = format!(
        "https://search.brave.com/search?q={}+linux+download&hl=en",
        urlencoding::encode(&search_query)
    );
    let web_req = client.get(&brave_url1).send();

    let (gh_res, web_res) = tokio::join!(gh_req, web_req);

    let mut all_matches: Vec<AppMatch> = Vec::new();

    // ── Parse GitHub ──────────────────────────────────────────────────────────
    if let Ok(resp) = gh_res {
        if let Ok(json) = resp.json::<serde_json::Value>().await {
            if let Some(items) = json["items"].as_array() {
                for item in items.iter().take(5) {
                    let name  = item["full_name"].as_str().unwrap_or("").to_string();
                    let stars = item["stargazers_count"].as_u64().unwrap_or(0);
                    if name.is_empty() { continue; }
                    all_matches.push(AppMatch {
                        url:      format!("https://api.github.com/repos/{}/releases/latest", name),
                        name:     format!("{} (★{})", name, stars),
                        platform: "GitHub".to_string(),
                    });
                }
            }
        }
    }

    // ── Parse Web Search (Brave first, retry with simpler query, DDG fallback) ─
    let mut web_matches = Vec::new();

    // Brave attempt 1 (concurrent with GitHub)
    match web_res {
        Ok(resp) => match resp.text().await {
            Ok(html) => { web_matches = parse_search_html(&html, &search_query); }
            Err(_) => {}
        },
        Err(_) => {}
    }

    // Brave attempt 2 (retry with simpler query if 1st was empty)
    if web_matches.is_empty() {
        let url = format!(
            "https://search.brave.com/search?q={}+download&hl=en",
            urlencoding::encode(&search_query)
        );
        if let Ok(resp) = client.get(&url).send().await {
            if let Ok(html) = resp.text().await {
                web_matches = parse_search_html(&html, &search_query);
            }
        }
    }

    if web_matches.is_empty() {
        let ddg_url = format!(
            "https://html.duckduckgo.com/html/?q={}+linux+download+install",
            urlencoding::encode(&search_query)
        );
        match client.get(&ddg_url).send().await {
            Ok(resp) => match resp.text().await {
                Ok(html) => {
                    if !is_ddg_captcha(&html) {
                        web_matches = parse_search_html(&html, &search_query);
                    }
                }
                Err(_) => {}
            },
            Err(_) => {}
        }
    }
    all_matches.extend(web_matches);

    if all_matches.is_empty() {
        println!("❌ No results found. Try a more specific name or paste the direct URL.");
        return Ok(());
    }

    // ── User selection ────────────────────────────────────────────────────────
    let display_options: Vec<String> = all_matches
        .iter()
        .map(|a| format!("[{}] {}", a.platform, a.name))
        .collect();

    let selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Select source to install from")
        .items(&display_options)
        .default(0)
        .interact()?;

    let chosen = &all_matches[selection];
    println!("\n→ {} via {}", chosen.name, chosen.platform);

    // ── Dispatch to handler ───────────────────────────────────────────────────
    match chosen.platform.as_str() {
        "GitHub" => {
            handle_github(&client, &chosen.url, &chosen.name, &distro).await?;
        }
        "Web" => {
            handle_direct_url(&client, &chosen.url, &distro).await?;
        }
        _ => {}
    }

    Ok(())
}

// ─── Direct URL Handler ───────────────────────────────────────────────────────

async fn handle_direct_url(
    client: &reqwest::Client,
    url: &str,
    distro: &Distro,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("📄 Fetching page: {}", url);
    let resp = client.get(url).send().await?;
    let final_url = resp.url().to_string(); // follow redirects
    let html = resp.text().await?;

    // Step 1: Try scraping the landing page directly
    let mut links = scrape_download_links(&html, &final_url, distro);

    // Step 2: No links found — look for a "download" sub-page link
    if links.is_empty() {
        println!("  ↳ No binaries on landing page. Looking for a /download page...");
        let document = Html::parse_document(&html);
        let a_sel = Selector::parse("a[href]").unwrap();
        let mut download_page_url: Option<String> = None;

        for el in document.select(&a_sel) {
            if let Some(href) = el.value().attr("href") {
                let text = el.text().collect::<String>().to_lowercase();
                let href_lower = href.to_lowercase();
                if text.contains("download") || href_lower.contains("download") || href_lower.contains("releases") {
                    if let Some(abs) = resolve_url(&final_url, href) {
                        // Avoid external sites
                        if let (Ok(base_u), Ok(link_u)) = (url::Url::parse(url), url::Url::parse(&abs)) {
                            if base_u.host_str() == link_u.host_str() {
                                download_page_url = Some(abs);
                                break;
                            }
                        }
                    }
                }
            }
        }

        if let Some(dl_url) = download_page_url {
            println!("  ↳ Scraping download page: {}", dl_url);
            if let Ok(resp2) = client.get(&dl_url).send().await {
                if let Ok(html2) = resp2.text().await {
                    links = scrape_download_links(&html2, &dl_url, distro);
                }
            }
        }
    }

    // Step 2b: If still no links, try common download page patterns automatically
    if links.is_empty() {
        println!("  ↳ Trying common download page patterns...");
        let download_urls_to_try = vec![
            format!("{}/download", final_url.trim_end_matches('/')),
            format!("{}/downloads", final_url.trim_end_matches('/')),
            format!("{}/Download", final_url.trim_end_matches('/')),
        ];

        for dl_url in download_urls_to_try {
            println!("  ↳ Checking: {}", dl_url);
            if let Ok(resp2) = client.get(&dl_url).send().await {
                if resp2.status().is_success() {
                    if let Ok(html2) = resp2.text().await {
                        links = scrape_download_links(&html2, &dl_url, distro);
                        if !links.is_empty() {
                            println!("  ↳ Found {} binaries on download page!", links.len());
                            break;
                        }
                    }
                }
            }
        }
    }

    // Step 3: Special handling for well-known sites
    if links.is_empty() {
        println!("  ↳ Trying special handling for well-known sites...");
        if final_url.contains("code.visualstudio.com") || url.contains("code.visualstudio.com") {
            println!("  ↳ Detected VS Code website — using direct download URLs");
            // VS Code download URLs follow a known pattern
            let arch = if cfg!(target_arch = "x86_64") { "x64" } else { "arm64" };

            // Try to get the latest version from the updates API
            let versions_url = "https://update.code.visualstudio.com/api/releases/stable";
            if let Ok(resp) = client.get(versions_url).send().await {
                if let Ok(versions_json) = resp.json::<serde_json::Value>().await {
                    // The API returns a JSON array like ["1.xx.x"]
                    if let Some(version) = versions_json.as_array()
                        .and_then(|arr| arr.first())
                        .and_then(|v| v.as_str())
                    {
                        let base_url = format!(
                            "https://update.code.visualstudio.com/{}/linux-{}/stable",
                            version, arch
                        );

                        // Add both .deb and .rpm URLs
                        let deb_url = format!("{}/code-stable-{}-linux-{}.deb", base_url, version, arch);
                        let rpm_url = format!("{}/code-stable-{}-linux-{}.rpm", base_url, version, arch);

                        links.push((format!("code-stable-{}-linux-{}.deb", version, arch), deb_url));
                        links.push((format!("code-stable-{}-linux-{}.rpm", version, arch), rpm_url));

                        println!("  ↳ Found VS Code {} for Linux {}", version, arch);
                    }
                }
            }
        }
    }

    // Step 4: GitHub Pivot — if still empty, look for a github.com/owner/repo in the HTML
    if links.is_empty() {
        println!("  ↳ No binaries found. Attempting GitHub Pivot...");
        if let Some(repo) = extract_github_repo(&html) {
            println!("  ↳ Found GitHub repo: {}. Fetching its releases...", repo);
            let releases_url = format!("https://api.github.com/repos/{}/releases/latest", repo);
            handle_github(client, &releases_url, &repo, distro).await?;
            return Ok(());
        }

        // Step 5: Curl install script detection
        println!("  ↳ No binaries or GitHub repo found. Checking for install scripts...");
        let curl_installs = extract_curl_installs(&html, &final_url);
        if !curl_installs.is_empty() {
            println!("  ↳ Found {} install script(s).", curl_installs.len());
            let options: Vec<String> = curl_installs.iter().map(|c| format!("📜 {}", c.display_name)).collect();
            let sel = Select::with_theme(&ColorfulTheme::default())
                .with_prompt("Select install script")
                .items(&options)
                .default(0)
                .interact()?;
            return execute_curl_install(&curl_installs[sel]).await;
        }

        println!("❌ Could not find any installable binary for this page.");
        println!("   Try: sit https://github.com/<owner>/<repo>");
        return Ok(());
    }

    // Step 4: Auto-select best package or present choices
    sort_assets(&mut links, distro);

    let (fname, furl) = if links.is_empty() {
        println!("❌ No installable packages found.");
        return Ok(());
    } else if links.len() == 1 {
        // Only one option - auto-select it
        println!("🎯 Found single package: {}", links[0].0);
        (&links[0].0, &links[0].1)
    } else {
        // Multiple options - try to auto-select the best one
        let best_index = auto_select_best_package(&links, distro);
        let best_package = &links[best_index].0;

        println!("🤖 Auto-selected best package: {}", best_package);
        println!("   (You can change this selection if needed)");

        // Show all options with the best one pre-selected
        let names: Vec<String> = links.iter().enumerate().map(|(i, (n, _))| {
            let n_lower = n.to_lowercase();
            let mut display_name = String::new();

            if i == best_index {
                display_name.push_str("✓ ");
            } else {
                display_name.push_str("  ");
            }

            if n_lower.ends_with(".appimage") {
                display_name.push_str(&format!("{} 🐧", n));
            } else if n_lower.ends_with(".deb") {
                display_name.push_str(&format!("{} 📦", n));
            } else if n_lower.ends_with(".rpm") {
                display_name.push_str(&format!("{} 📦", n));
            } else {
                display_name.push_str(&format!("{} 📦", n));
            }

            display_name
        }).collect();

        let sel = Select::with_theme(&ColorfulTheme::default())
            .with_prompt("Select binary to install")
            .items(&names)
            .default(best_index)
            .interact()?;
        (&links[sel].0, &links[sel].1)
    };
    let app_name = fname
        .split('_').next()
        .and_then(|s| s.split('-').next())
        .unwrap_or("app")
        .to_string();

    execute_install(client, fname, furl, &app_name, distro).await
}

// ─── Curl Install Script Handler ─────────────────────────────────────────────

async fn execute_curl_install(install: &CurlInstall) -> Result<(), Box<dyn std::error::Error>> {
    use dialoguer::Confirm;

    let script_fname = extract_fname(&install.url);
    let tmp_dir = "/tmp/sit-downloads";
    std::fs::create_dir_all(tmp_dir)?;
    let dest = format!("{}/{}", tmp_dir, script_fname);

    println!("⬇  Downloading install script...");
    let dl_ok = std::process::Command::new("curl")
        .args(["-fsSL", "-o", &dest, &install.url])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !dl_ok || !std::path::Path::new(&dest).exists() {
        println!("❌ Failed to download script from {}", install.url);
        return Ok(());
    }

    // Show the script content with line numbers so user can review
    if let Ok(content) = std::fs::read_to_string(&dest) {
        println!("\n📜 Script content ({} lines):", content.lines().count());
        println!("───────────────────────────────────────");
        for (i, line) in content.lines().enumerate() {
            println!("{:>4} │ {}", i + 1, line);
        }
        println!("───────────────────────────────────────");
    }

    println!("\nWould run: sudo bash {}", dest);
    if !Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("Execute this script?")
        .default(false)
        .interact()?
    {
        println!("🚫 Cancelled. Script kept at {}", dest);
        return Ok(());
    }

    println!("🚀 Running script...");
    let status = std::process::Command::new("sudo")
        .args(["bash", &dest])
        .status();

    match status {
        Ok(s) if s.success() => {
            std::fs::remove_file(&dest).ok();
            println!("✅ Install script completed successfully.");
        }
        Ok(s) => {
            println!("❌ Script exited with status: {}", s);
            println!("   Script kept at {} for inspection.", dest);
        }
        Err(e) => {
            println!("❌ Failed to run script: {}", e);
            println!("   Script kept at {} for inspection.", dest);
        }
    }

    Ok(())
}

// ─── GitHub Release Handler ───────────────────────────────────────────────────

async fn handle_github(
    client: &reqwest::Client,
    releases_url: &str,
    repo_label: &str,
    distro: &Distro,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("📦 Fetching GitHub release assets...");
    let resp = client.get(releases_url).send().await?;
    let json = resp.json::<serde_json::Value>().await?;

    // Might be a list (search) or a single release object (direct latest)
    let release = if json.is_array() {
        json.as_array()
            .and_then(|arr| arr.first())
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    } else {
        json
    };

    let assets = release["assets"].as_array().cloned().unwrap_or_default();
    if assets.is_empty() {
        println!("❌ No release assets found for this repo.");
        return Ok(());
    }

    let mut valid: Vec<(String, String)> = assets
        .iter()
        .filter_map(|a| {
            let name = a["name"].as_str()?;
            let url  = a["browser_download_url"].as_str()?;
            if is_valid_asset(name, distro) {
                Some((name.to_string(), url.to_string()))
            } else {
                None
            }
        })
        .collect();

    if valid.is_empty() {
        println!("❌ No compatible x86_64 binaries found in this release.");
        println!("   (checked: .deb, .rpm, .AppImage, .tar.gz, .tar.xz, .tar.bz2)");
        return Ok(());
    }

    sort_assets(&mut valid, distro);

    let names: Vec<String> = valid.iter().map(|(n, _)| n.clone()).collect();
    let sel = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Select format to install")
        .items(&names)
        .default(0)
        .interact()?;

    let (fname, furl) = &valid[sel];
    let app_name = repo_label.split('/').next_back().unwrap_or("app").to_string();
    execute_install(client, fname, furl, &app_name, distro).await
}

// ─── Download + Install ───────────────────────────────────────────────────────

async fn execute_install(
    _client: &reqwest::Client,
    name: &str,
    url: &str,
    app_short_name: &str,
    distro: &Distro,
) -> Result<(), Box<dyn std::error::Error>> {
    // Always download to a known absolute directory so every subsequent
    // operation can refer to a stable path regardless of cwd.
    let tmp_dir = "/tmp/sit-downloads";
    std::fs::create_dir_all(tmp_dir)?;
    let dest = format!("{}/{}", tmp_dir, name);

    println!("⬇  Downloading {} ...", name);
    println!("   → {}", dest);

    // ponytail: curl first (reliable single-connection), axel only for large files
    // where multi-connection actually helps. GitHub CDN throttles axel.
    let curl_ok = std::process::Command::new("curl")
        .args(["-L", "--progress-bar", "-o", &dest, url])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !curl_ok {
        // Fallback: try axel with fewer connections
        println!("⚠  curl failed. Trying axel...");
        let axel_ok = std::process::Command::new("axel")
            .args(["-n", "4", "-a", url, "-o", &dest])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !axel_ok {
            println!("❌ Download failed.");
            return Ok(());
        }
    }

    // Confirm the file actually landed on disk
    if !std::path::Path::new(&dest).exists() {
        println!("❌ Download reported success but file not found at {}", dest);
        return Ok(());
    }
    println!("   ✓ Download complete ({} bytes)",
        std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0));

    let home      = env::var("HOME").unwrap_or_else(|_ | "/root".to_string());
    let local_bin = format!("{}/.local/bin", home);
    let apps_dir  = format!("{}/.local/share/applications", home);
    std::fs::create_dir_all(&local_bin)?;
    std::fs::create_dir_all(&apps_dir)?;

    let n = name.to_lowercase();

    if n.ends_with(".appimage") {
        install_appimage(&dest, app_short_name, &local_bin, &apps_dir)?;

    } else if n.ends_with(".deb") {
        install_deb(&dest, distro)?;

    } else if n.ends_with(".rpm") {
        install_rpm(&dest, distro)?;

    } else if n.ends_with(".tar.gz") || n.ends_with(".tar.xz") || n.ends_with(".tar.bz2") {
        install_archive(&dest, app_short_name, &home, &local_bin, &apps_dir)?;

    } else if n.ends_with(".zip") {
        install_archive(&dest, app_short_name, &home, &local_bin, &apps_dir)?;
    }

    // Refresh desktop DB so the app appears in GNOME / KDE menus
    std::process::Command::new("update-desktop-database")
        .args([&apps_dir])
        .status()
        .ok();

    Ok(())
}

// ─── Format-specific installers ───────────────────────────────────────────────

fn install_appimage(
    name: &str,
    app_short_name: &str,
    local_bin: &str,
    apps_dir: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    std::process::Command::new("chmod").args(["+x", name]).status()?;
    let target = format!("{}/{}", local_bin, app_short_name);
    std::fs::rename(name, &target)?;

    write_desktop_file(apps_dir, app_short_name, &target)?;
    println!("✅ AppImage installed to {}. Launcher shortcut created.", target);
    Ok(())
}

fn install_deb(
    name: &str,
    distro: &Distro,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("🔐 Sudo password required for .deb installation:");

    // Show package details and ask for confirmation
    if let Ok(metadata) = std::fs::metadata(name) {
        println!("   Package: {}", name.split('/').next_back().unwrap_or(name));
        println!("   Size: {} bytes", metadata.len());
    }

    if !confirm_sudo_operation("install this .deb package")? {
        println!("🚫 Installation cancelled by user.");
        return Ok(());
    }

    // `name` is already an absolute path (e.g. /tmp/sit-downloads/foo.deb).
    // apt needs either an absolute path or a "./"-prefixed relative path to
    // recognise it as a local file rather than a repo package name.
    // Since we have an absolute path, pass it directly — no "./" prefix needed.
    let success = match distro {
        Distro::Ubuntu => {
            std::process::Command::new("sudo")
                .args(["apt", "install", "-y", name])
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        }
        // Non-Ubuntu system with a .deb — dpkg then fix deps
        _ => {
            let dpkg_ok = std::process::Command::new("sudo")
                .args(["dpkg", "-i", name])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !dpkg_ok {
                std::process::Command::new("sudo")
                    .args(["apt", "-f", "install", "-y"])
                    .status()
                    .ok();
            }
            dpkg_ok
        }
    };

    if success {
        std::fs::remove_file(name).ok();
        println!("✅ .deb installed successfully. Source file removed.");
    } else {
        println!("❌ .deb installation failed. File kept at {}", name);
    }
    Ok(())
}

fn install_rpm(
    name: &str,
    distro: &Distro,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("🔐 Sudo password required for .rpm installation:");

    // Show package details and ask for confirmation
    if let Ok(metadata) = std::fs::metadata(name) {
        println!("   Package: {}", name.split('/').next_back().unwrap_or(name));
        println!("   Size: {} bytes", metadata.len());
    }

    if !confirm_sudo_operation("install this .rpm package")? {
        println!("🚫 Installation cancelled by user.");
        return Ok(());
    }

    let success = match distro {
        Distro::Fedora => {
            std::process::Command::new("sudo")
                .args(["dnf", "install", "-y", name])
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        }
        _ => {
            // Try rpm directly as fallback on unknown distros
            std::process::Command::new("sudo")
                .args(["rpm", "-i", name])
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        }
    };

    if success {
        std::fs::remove_file(name).ok();
        println!("✅ .rpm installed successfully. Source file removed.");
    } else {
        println!("❌ .rpm installation failed. File kept at {}", name);
    }
    Ok(())
}

fn install_archive(
    name: &str,
    app_short_name: &str,
    home: &str,
    local_bin: &str,
    apps_dir: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let opt_dir = format!("{}/.local/opt/{}", home, app_short_name);

    // Clean up existing installation if it exists
    if std::path::Path::new(&opt_dir).exists() {
        std::fs::remove_dir_all(&opt_dir)?;
    }
    std::fs::create_dir_all(&opt_dir)?;

    println!("📦 Extracting {} to {}", name, opt_dir);

    // Extract based on format
    let n = name.to_lowercase();
    let status = if n.ends_with(".zip") {
        std::process::Command::new("unzip")
            .args(["-q", name, "-d", &opt_dir])
            .status()?
    } else {
        // tar: pick the right compression flag
        let flag = if n.ends_with(".tar.gz") { "-xzf" }
            else if n.ends_with(".tar.xz")  { "-xJf" }
            else { "-xjf" }; // .tar.bz2
        std::process::Command::new("tar")
            .args([flag, name, "-C", &opt_dir, "--strip-components=1"])
            .status()?
    };

    if !status.success() {
        return Err(format!("Failed to extract: {}", name).into());
    }

    std::fs::remove_file(name)?;

    // Find the primary executable, create symlink + desktop file
    let bin_path = find_executable_in_directory(&opt_dir, app_short_name);

    if let Some(ref bin_path) = bin_path {
        let symlink = format!("{}/{}", local_bin, app_short_name);

        if std::path::Path::new(&symlink).exists() {
            std::fs::remove_file(&symlink)?;
        }
        std::os::unix::fs::symlink(bin_path, &symlink)?;

        write_desktop_file(apps_dir, app_short_name, bin_path)?;
        println!("✅ Extracted to {}.", opt_dir);
        println!("   Symlink: {} → {}", symlink, bin_path);
        println!("   Launcher shortcut created.");
    } else {
        println!("✅ Extracted to {}.", opt_dir);
        println!("⚠  Could not locate main executable — no symlink or shortcut created.");
        println!("   Browse {} and run the binary manually.", opt_dir);
    }
    Ok(())
}

/// Find an executable file in the extracted directory
/// Returns the path to the executable if found, None otherwise
fn find_executable_in_directory(dir: &str, app_name: &str) -> Option<String> {
    use std::fs;

    let mut candidates = Vec::new();

    // Recursive function to find executables
    fn find_executables(path: &str, app_name: &str, candidates: &mut Vec<(i32, String)>) {
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                let entry_path = entry.path();
                let entry_path_str = entry_path.to_string_lossy().into_owned();

                if entry_path.is_dir() {
                    find_executables(&entry_path_str, app_name, candidates);
                } else if entry_path.is_file() {
                    // Check if file is executable
                    if let Ok(metadata) = fs::metadata(&entry_path) {
                        use std::os::unix::fs::PermissionsExt;
                        if metadata.permissions().mode() & 0o111 != 0 {
                            // Skip common non-binary files
                            let file_name = entry_path.file_name().unwrap_or_default();
                            let file_name_str = file_name.to_string_lossy();

                            if !file_name_str.starts_with("lib") &&
                               !file_name_str.ends_with(".so") &&
                               !file_name_str.ends_with(".so.") &&
                               !file_name_str.ends_with(".desktop") &&
                               !file_name_str.ends_with(".txt") &&
                               !file_name_str.ends_with(".md") &&
                               !file_name_str.ends_with(".sh") &&
                               !file_name_str.ends_with(".py") &&
                               !file_name_str.ends_with(".rb") &&
                               !file_name_str.ends_with(".pl") {

                                // Prioritize files in bin/ directory or matching app name
                                let mut score = 0;
                                if entry_path_str.contains("/bin/") {
                                    score += 2;
                                }
                                if entry_path_str.contains(&format!("/{}", app_name)) {
                                    score += 1;
                                }

                                candidates.push((score, entry_path_str));
                            }
                        }
                    }
                }
            }
        }
    }

    find_executables(dir, app_name, &mut candidates);

    // Sort by score (higher is better) and return the best candidate
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    candidates.into_iter().map(|(_, path)| path).next()
}

// ─── Desktop Entry Writer ─────────────────────────────────────────────────────

fn write_desktop_file(
    apps_dir: &str,
    app_name: &str,
    exec_path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let display_name = capitalise(app_name);
    let desktop = format!(
        "[Desktop Entry]\nVersion=1.0\nType=Application\nName={name}\nComment={name} (installed by sit)\nExec={exec}\nIcon={name}\nTerminal=false\nCategories=Utility;\n",
        name = display_name,
        exec = exec_path,
    );
    std::fs::write(format!("{}/{}.desktop", apps_dir, app_name), desktop)?;
    Ok(())
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

/// Ask user for confirmation before performing sudo operations
/// Returns true if user confirms, false if cancelled
fn confirm_sudo_operation(action: &str) -> Result<bool, Box<dyn std::error::Error>> {
    use dialoguer::Confirm;

    let confirmed = Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt(format!("Are you sure you want to {}?", action))
        .default(true)
        .interact()?;

    Ok(confirmed)
}

/// Auto-select the best package based on common naming patterns
/// Returns the index of the best package, or 0 if no clear winner
fn auto_select_best_package(links: &[(String, String)], distro: &Distro) -> usize {
    let mut scores: Vec<(usize, i32)> = links.iter().enumerate().map(|(i, (name, _))| {
        let name_lower = name.to_lowercase();
        let mut score = 0;

        // Score based on distro preference
        match distro {
            Distro::Fedora => {
                if name_lower.ends_with(".rpm") { score += 10; }
            }
            Distro::Ubuntu => {
                if name_lower.ends_with(".deb") { score += 10; }
            }
            Distro::Unknown => {
                if name_lower.ends_with(".appimage") { score += 10; }
            }
        }

        // Score based on common patterns
        if name_lower.contains("stable") { score += 5; }
        if name_lower.contains("latest") { score += 3; }
        if name_lower.contains("release") { score += 3; }
        if name_lower.contains("x64") || name_lower.contains("x86_64") || name_lower.contains("amd64") { score += 2; }

        // Penalize pre-release versions
        if name_lower.contains("alpha") || name_lower.contains("beta") || name_lower.contains("rc") || name_lower.contains("dev") {
            score -= 5;
        }

        (i, score)
    }).collect();

    // Sort by score and return the highest
    scores.sort_by(|a, b| b.1.cmp(&a.1));
    scores.first().map(|(i, _)| *i).unwrap_or(0)
}

// ─── HTTP Client Builder ──────────────────────────────────────────────────────

fn build_client() -> Result<reqwest::Client, Box<dyn std::error::Error>> {
    let mut headers = HeaderMap::new();
    headers.insert(
        USER_AGENT,
        "Mozilla/5.0 (X11; Linux x86_64; rv:126.0) Gecko/20100101 Firefox/126.0".parse()?,
    );
    headers.insert(
        ACCEPT,
        "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8".parse()?,
    );
    headers.insert(ACCEPT_LANGUAGE, "en-US,en;q=0.5".parse()?);

    // GitHub requires a User-Agent and benefits from Accept: application/vnd.github+json
    // We set both globally; GitHub ignores the HTML accept header gracefully
    headers.insert(
        "X-GitHub-Api-Version",
        "2022-11-28".parse()?,
    );

    // Optional: read GITHUB_TOKEN from env to raise API rate limit from 60 → 5000 req/hr
    if let Ok(token) = env::var("GITHUB_TOKEN") {
        headers.insert(AUTHORIZATION, format!("Bearer {}", token).parse()?);
    }

    Ok(reqwest::Client::builder()
        .default_headers(headers)
        .redirect(reqwest::redirect::Policy::limited(10))
        .timeout(std::time::Duration::from_secs(30))
        .build()?)
}