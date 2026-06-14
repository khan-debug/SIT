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
        || n.contains("setup")
        || n.contains("installer")
        || n.contains("i686")           // 32-bit
        || n.contains("i386")
        || n.ends_with(".sha256")
        || n.ends_with(".sha512")
        || n.ends_with(".sig")
        || n.ends_with(".asc")
        || n.ends_with(".json")
        || n.ends_with(".txt")
        || n.ends_with(".zip")          // zip is almost never a Linux native pkg
        || n.ends_with(".blockmap")
    {
        return false;
    }

    // Require at least one accepted extension
    let accepted = match distro {
        Distro::Fedora  => vec![".rpm", ".appimage", ".tar.gz", ".tar.xz", ".tar.bz2"],
        Distro::Ubuntu  => vec![".deb", ".appimage", ".tar.gz", ".tar.xz", ".tar.bz2"],
        Distro::Unknown => vec![".appimage", ".tar.gz", ".tar.xz", ".tar.bz2"],
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
    if href.starts_with("http://") || href.starts_with("https://") {
        return Some(href.to_string());
    }
    if href.starts_with("//") {
        return Some(format!("https:{}", href));
    }
    if href.starts_with('/') {
        // absolute path — prepend scheme + host
        let parsed = url::Url::parse(base).ok()?;
        return Some(format!("{}://{}{}", parsed.scheme(), parsed.host_str()?, href));
    }
    // relative path
    let parsed = url::Url::parse(base).ok()?;
    let base_dir = parsed.as_str().rsplitn(2, '/').last()?;
    Some(format!("{}/{}", base_dir, href))
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
                    let fname = abs.split('/').last().unwrap_or("").split('?').next().unwrap_or("").to_string();
                    if is_valid_asset(&fname, distro) {
                        if !links.iter().any(|(n, _)| n == &fname) {
                            links.push((fname, abs));
                        }
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
                        let fname = abs.split('/').last().unwrap_or("").split('?').next().unwrap_or("").to_string();
                        if is_valid_asset(&fname, distro) {
                            if !links.iter().any(|(n, _)| n == &fname) {
                                links.push((fname, abs));
                            }
                        }
                    }
                }
            }
        }
    }

    // 3. Regex scan raw HTML for anything that looks like a binary URL
    //    Catches URLs embedded in <script> blocks, onclick="...", window.location, etc.
    let url_re = Regex::new(
        r#"https?://[^\s"'<>]+\.(?:AppImage|deb|rpm|tar\.gz|tar\.xz|tar\.bz2)"#
    ).unwrap();
    for cap in url_re.captures_iter(html) {
        let abs = cap[0].to_string();
        let fname = abs.split('/').last().unwrap_or("").split('?').next().unwrap_or("").to_string();
        if is_valid_asset(&fname, distro) && !links.iter().any(|(n, _)| n == &fname) {
            links.push((fname, abs));
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

    println!("🔍 Searching GitHub (stars), Flathub, and Web for '{}'...", search_query);

    // ── Concurrent search ─────────────────────────────────────────────────────

    // 1. GitHub — sorted by stars so popular/official repos bubble first
    let gh_url = format!(
        "https://api.github.com/search/repositories?q={}&sort=stars&order=desc",
        urlencoding::encode(&search_query)
    );
    let gh_req = client.get(&gh_url).send();

    // 2. DuckDuckGo HTML endpoint — no JS needed, much more scraper-friendly than Yahoo
    let ddg_url = format!(
        "https://html.duckduckgo.com/html/?q={}+official+linux+download+site",
        urlencoding::encode(&search_query)
    );
    let web_req = client.get(&ddg_url).send();

    // 3. Flatpak CLI
    let sq_clone = search_query.clone();
    let flatpak_task = tokio::task::spawn_blocking(move || {
        std::process::Command::new("flatpak")
            .args(["search", "--columns=application", &sq_clone])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
    });

    let (gh_res, web_res, flatpak_out) = tokio::join!(gh_req, web_req, flatpak_task);

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

    // ── Parse Flatpak ─────────────────────────────────────────────────────────
    if let Ok(Some(stdout)) = flatpak_out {
        let app_id_re = Regex::new(r"^[a-zA-Z][a-zA-Z0-9_-]+\.[a-zA-Z][a-zA-Z0-9_.-]+$").unwrap();
        let mut count = 0;
        for line in stdout.lines() {
            if count >= 3 { break; }
            let id = line.trim().to_string();
            if app_id_re.is_match(&id) {
                all_matches.push(AppMatch {
                    name:     id.clone(),
                    url:      id,
                    platform: "Flathub".to_string(),
                });
                count += 1;
            }
        }
    }

    // ── Parse DuckDuckGo HTML ─────────────────────────────────────────────────
    if let Ok(resp) = web_res {
        if let Ok(html) = resp.text().await {
            let document = Html::parse_document(&html);
            let mut seen_domains: Vec<String> = Vec::new();
            let mut count = 0;

            // DDG HTML results: result links are in <a class="result__a"> or <a class="result__url">
            // Real URL is in data-href or the href itself (not redirect-wrapped like Yahoo)
            let link_sel = Selector::parse("a.result__a, a.result__url").unwrap();
            for el in document.select(&link_sel) {
                if count >= 3 { break; }

                // Try data-href first (DDG sometimes puts the real URL there)
                let href = el.value().attr("data-href")
                    .or_else(|| el.value().attr("href"))
                    .unwrap_or("")
                    .to_string();

                if href.is_empty()
                    || href.contains("duckduckgo.com")
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

                let final_url = if href.starts_with("//duckduckgo.com/l/?uddg=") {
                    // DDG redirect — decode the uddg= param
                    href.split("uddg=")
                        .nth(1)
                        .and_then(|s| s.split('&').next())
                        .and_then(|enc| urlencoding::decode(enc).ok())
                        .map(|s| s.to_string())
                        .unwrap_or(href.clone())
                } else {
                    href
                };

                if !final_url.starts_with("http") { continue; }

                let domain = url::Url::parse(&final_url)
                    .ok()
                    .and_then(|u| u.host_str().map(|h| h.to_string()))
                    .unwrap_or_default();

                if domain.is_empty() || seen_domains.contains(&domain) { continue; }
                seen_domains.push(domain.clone());

                all_matches.push(AppMatch {
                    name:     format!("{} (Web)", domain),
                    url:      final_url,
                    platform: "Web".to_string(),
                });
                count += 1;
            }
        }
    }

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
        "Flathub" => {
            println!("📦 Running flatpak install...");
            std::process::Command::new("flatpak")
                .args(["install", "flathub", &chosen.url, "-y"])
                .status()?;
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

    // Step 3: GitHub Pivot — if still empty, look for a github.com/owner/repo in the HTML
    if links.is_empty() {
        println!("  ↳ No binaries found. Attempting GitHub Pivot...");
        if let Some(repo) = extract_github_repo(&html) {
            println!("  ↳ Found GitHub repo: {}. Fetching its releases...", repo);
            let releases_url = format!("https://api.github.com/repos/{}/releases/latest", repo);
            handle_github(client, &releases_url, &repo, distro).await?;
            return Ok(());
        }
        println!("❌ Could not find any installable binary for this page.");
        println!("   Try: sit https://github.com/<owner>/<repo>");
        return Ok(());
    }

    // Step 4: Present found links
    sort_assets(&mut links, distro);
    let names: Vec<String> = links.iter().map(|(n, _)| n.clone()).collect();
    let sel = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Select binary to install")
        .items(&names)
        .default(0)
        .interact()?;

    let (fname, furl) = &links[sel];
    let app_name = fname
        .split('_').next()
        .and_then(|s| s.split('-').next())
        .unwrap_or("app")
        .to_string();

    execute_install(client, fname, furl, &app_name, distro).await
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
    let app_name = repo_label.split('/').last().unwrap_or("app").to_string();
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

    let dl_status = std::process::Command::new("axel")
        .args(["-n", "8", "-a", url, "-o", &dest])
        .status();

    match dl_status {
        Ok(s) if s.success() => {}
        _ => {
            println!("⚠  axel failed or not installed. Falling back to curl...");
            let curl_ok = std::process::Command::new("curl")
                .args(["-L", "--progress-bar", "-o", &dest, url])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !curl_ok {
                println!("❌ Download failed.");
                return Ok(());
            }
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
        install_tarball(&dest, app_short_name, &home, &local_bin, &apps_dir)?;
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
        println!("❌ .deb installation failed. File kept at ./{}", name);
    }
    Ok(())
}

fn install_rpm(
    name: &str,
    distro: &Distro,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("🔐 Sudo password required for .rpm installation:");

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
        println!("❌ .rpm installation failed. File kept at ./{}", name);
    }
    Ok(())
}

fn install_tarball(
    name: &str,
    app_short_name: &str,
    home: &str,
    local_bin: &str,
    apps_dir: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let opt_dir = format!("{}/.local/opt/{}", home, app_short_name);
    let _ = std::fs::remove_dir_all(&opt_dir);
    std::fs::create_dir_all(&opt_dir)?;

    // Pick the right tar flag for the compression type
    let compress_flag = if name.ends_with(".tar.gz") { "-xzf" }
        else if name.ends_with(".tar.xz")  { "-xJf" }
        else { "-xjf" }; // .tar.bz2

    std::process::Command::new("tar")
        .args([compress_flag, name, "-C", &opt_dir, "--strip-components=1"])
        .status()?;
    std::fs::remove_file(name).ok();

    // Find the primary executable: prefer a file matching the app name in a bin/ dir,
    // then any executable that isn't a library/script
    let find_cmd = format!(
        r#"find {opt} -type f \( -executable -o -perm /111 \) \
            ! -name '*.so' ! -name '*.so.*' ! -name '*.desktop' \
            ! -name '*.txt'  ! -name '*.md'  ! -name '*.sh' \
            ! -name '*.py'   ! -name '*.rb'  ! -name '*.pl' \
            | sort | grep -iE '(bin/|/{name})' \
            | head -n 1 || \
          find {opt} -maxdepth 3 -type f \( -executable -o -perm /111 \) \
            ! -name '*.so' ! -name '*.so.*' ! -name '*.desktop' \
            ! -name '*.txt' ! -name '*.md' \
            | head -n 1"#,
        opt = opt_dir,
        name = app_short_name,
    );

    let out = std::process::Command::new("sh").args(["-c", &find_cmd]).output()?;
    let bin_path = String::from_utf8_lossy(&out.stdout).trim().to_string();

    if !bin_path.is_empty() {
        let symlink = format!("{}/{}", local_bin, app_short_name);
        // Remove stale symlink before creating a new one
        std::fs::remove_file(&symlink).ok();
        std::process::Command::new("ln")
            .args(["-sf", &bin_path, &symlink])
            .status()?;

        write_desktop_file(apps_dir, app_short_name, &bin_path)?;
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