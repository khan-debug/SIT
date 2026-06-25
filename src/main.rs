use dialoguer::{theme::ColorfulTheme, Select};
use reqwest::header::{ACCEPT, ACCEPT_LANGUAGE, AUTHORIZATION, USER_AGENT, HeaderMap};
use std::env;
use base64::Engine;

mod search;
mod scraper;
mod github;
mod install;

// ─── Shared Types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum Distro {
    Fedora,
    Ubuntu,
    Unknown,
}

#[derive(Clone)]
pub struct AppMatch {
    pub name:     String,
    pub platform: String,
    pub url:      String,
}

pub struct CurlInstall {
    pub url:          String,
    pub display_name: String,
}

// ─── Distro Detection ─────────────────────────────────────────────────────────

fn detect_distro() -> Distro {
    let c = std::fs::read_to_string("/etc/os-release").unwrap_or_default().to_lowercase();

    if c.contains("id_like=\"fedora\"") || c.contains("id_like=fedora")
        || c.contains("id=\"fedora\"") || c.contains("id=fedora")
        || c.contains("rhel") || c.contains("centos") || c.contains("rocky")
    { Distro::Fedora }
    else if c.contains("id_like=\"debian\"") || c.contains("id_like=debian")
        || c.contains("id=\"ubuntu\"") || c.contains("id=ubuntu")
        || c.contains("id=\"debian\"") || c.contains("id=debian")
        || c.contains("ubuntu") || c.contains("debian") || c.contains("mint")
        || c.contains("pop!_os") || c.contains("element") || c.contains("kali")
    { Distro::Ubuntu }
    else { Distro::Unknown }
}

fn distro_label(distro: &Distro) -> &'static str {
    match distro {
        Distro::Fedora  => "Fedora/RHEL (dnf)",
        Distro::Ubuntu  => "Ubuntu/Debian (apt)",
        Distro::Unknown => "Unknown (generic)",
    }
}

// ─── Asset Utilities ─────────────────────────────────────────────────────────

pub fn is_valid_asset(name: &str, distro: &Distro) -> bool {
    let n = name.to_lowercase();

    if n.contains("arm64") || n.contains("aarch64") || n.contains("armv7") || n.contains("armhf")
        || n.contains("darwin") || n.contains("macos") || n.contains("mac")
        || n.contains("win") || n.contains(".exe") || n.contains(".msi") || n.contains(".dmg") || n.contains(".pkg")
        || (n.contains("musl") && matches!(distro, Distro::Fedora | Distro::Ubuntu))
        || n.contains("i686") || n.contains("i386")
        || n.ends_with(".sha256") || n.ends_with(".sha512") || n.ends_with(".sig")
        || n.ends_with(".asc") || n.ends_with(".json") || n.ends_with(".txt")
        || n.ends_with(".blockmap") || n.ends_with(".zsync")
    { return false; }

    if n.ends_with(".zip") { return n.contains("linux") || n.contains("x64") || n.contains("amd64"); }
    if (n.contains("setup") || n.contains("installer")) && !n.contains("linux") && !n.contains("x64") && !n.contains("amd64") { return false; }

    let accepted: &[&str] = match distro {
        Distro::Fedora  => &[".rpm", ".appimage", ".tar.gz", ".tar.xz", ".tar.bz2", ".zip"],
        Distro::Ubuntu  => &[".deb", ".appimage", ".tar.gz", ".tar.xz", ".tar.bz2", ".zip"],
        Distro::Unknown => &[".appimage", ".tar.gz", ".tar.xz", ".tar.bz2", ".zip"],
    };
    accepted.iter().any(|ext| n.ends_with(ext))
}

pub fn sort_assets(assets: &mut Vec<(String, String)>, distro: &Distro) {
    assets.sort_by_key(|(name, _)| {
        let n = name.to_lowercase();
        match distro {
            Distro::Fedora  => if n.ends_with(".rpm") { 0 } else if n.ends_with(".appimage") { 1 } else { 2 },
            Distro::Ubuntu  => if n.ends_with(".deb") { 0 } else if n.ends_with(".appimage") { 1 } else { 2 },
            Distro::Unknown => if n.ends_with(".appimage") { 0 } else { 1 },
        }
    });
}

/// Guess download OS platform from URL / filename.
pub fn detect_os(url: &str, name: &str) -> &'static str {
    let s = format!("{} {}", url, name).to_lowercase();
    if s.contains("windows") || s.contains(".exe") || s.contains(".msi") { "Windows" }
    else if s.contains("macos") || s.contains("darwin") || s.contains(".dmg") { "macOS" }
    else { "Linux" }
}

pub fn auto_select_best_package(links: &[(String, String)], distro: &Distro) -> usize {
    let mut scores: Vec<(usize, i32)> = links.iter().enumerate().map(|(i, (name, _))| {
        let n = name.to_lowercase();
        let mut s = 0i32;
        match distro { Distro::Fedora => if n.ends_with(".rpm") { s += 10; } Distro::Ubuntu => if n.ends_with(".deb") { s += 10; } Distro::Unknown => if n.ends_with(".appimage") { s += 10; } }
        if n.contains("stable") { s += 5; } if n.contains("latest") { s += 3; } if n.contains("release") { s += 3; }
        if n.contains("x64") || n.contains("x86_64") || n.contains("amd64") { s += 2; }
        if n.contains("alpha") || n.contains("beta") || n.contains("rc") || n.contains("dev") { s -= 5; }
        (i, s)
    }).collect();
    scores.sort_by(|a, b| b.1.cmp(&a.1));
    scores.first().map(|(i, _)| *i).unwrap_or(0)
}

// ─── URL Utilities ─────────────────────────────────────────────────────────────

pub fn resolve_url(base: &str, href: &str) -> Option<String> {
    Some(url::Url::parse(base).ok()?.join(href).ok()?.to_string())
}

pub fn extract_fname(url: &str) -> String {
    url.rsplit('/').next().unwrap_or("").split('?').next().unwrap_or("").to_string()
}

/// Resolve search-engine redirect URLs to the actual destination.
/// Handles DuckDuckGo (uddg param) and Bing (u param, base64 with a1 prefix).
pub fn resolve_search_url(href: &str) -> Option<String> {
    if href.is_empty() { return None; }
    let with_scheme = if href.starts_with("//") { format!("https:{}", href) } else { href.to_string() };
    let parsed = url::Url::parse(&with_scheme).ok()?;
    let host = parsed.host_str()?;

    if host.contains("duckduckgo.com") {
        for (k, v) in parsed.query_pairs() { if k == "uddg" { return Some(v.into_owned()); } }
        return None;
    }
    if host.contains("bing.com") {
        for (k, v) in parsed.query_pairs() {
            if k == "u" {
                let b64 = if v.starts_with("a1") { &v[2..] } else { &*v };
                let pad = (4 - b64.len() % 4) % 4;
                return base64::engine::general_purpose::STANDARD
                    .decode(format!("{}{}", b64, "=".repeat(pad)).as_bytes()).ok()
                    .and_then(|b| String::from_utf8(b).ok());
            }
        }
        return None;
    }
    Some(with_scheme)
}

pub fn is_ddg_captcha(html: &str) -> bool {
    html.contains("anomaly-modal") && html.contains("challenge")
}

// ─── Desktop Entry ─────────────────────────────────────────────────────────────

pub fn write_desktop_file(apps_dir: &str, app_name: &str, exec_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let display = capitalise(app_name);
    std::fs::write(format!("{}/{}.desktop", apps_dir, app_name), format!(
        "[Desktop Entry]\nVersion=1.0\nType=Application\nName={n}\nComment={n} (installed by sit)\nExec={e}\nIcon={n}\nTerminal=false\nCategories=Utility;\n",
        n = display, e = exec_path,
    ))?;
    Ok(())
}

pub fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    match c.next() { None => String::new(), Some(f) => f.to_uppercase().collect::<String>() + c.as_str() }
}

pub fn confirm_sudo_operation(action: &str) -> Result<bool, Box<dyn std::error::Error>> {
    Ok(dialoguer::Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt(format!("Are you sure you want to {}?", action)).default(true).interact()?)
}

// ─── HTTP Client ───────────────────────────────────────────────────────────────

fn build_client() -> Result<reqwest::Client, Box<dyn std::error::Error>> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, "Mozilla/5.0 (X11; Linux x86_64; rv:126.0) Gecko/20100101 Firefox/126.0".parse()?);
    headers.insert(ACCEPT, "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8".parse()?);
    headers.insert(ACCEPT_LANGUAGE, "en-US,en;q=0.5".parse()?);
    headers.insert("X-GitHub-Api-Version", "2022-11-28".parse()?);
    if let Ok(token) = env::var("GITHUB_TOKEN") { headers.insert(AUTHORIZATION, format!("Bearer {}", token).parse()?); }

    Ok(reqwest::Client::builder()
        .default_headers(headers)
        .redirect(reqwest::redirect::Policy::limited(10))
        .timeout(std::time::Duration::from_secs(30))
        .build()?)
}

// ─── Self-Update ───────────────────────────────────────────────────────────────

async fn self_update() -> Result<(), Box<dyn std::error::Error>> {
    let home = env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let local_bin = format!("{}/.local/bin", home);
    let target = format!("{}/sit", local_bin);
    std::fs::create_dir_all(&local_bin)?;

    println!("📥 Downloading latest sit binary...");
    if !std::process::Command::new("curl")
        .args(["-fsSL", "-o", &target, "https://github.com/khan-debug/SIT/releases/latest/download/sit"])
        .status()?.success()
    { println!("❌ Failed to download update."); return Ok(()); }

    std::process::Command::new("chmod").args(["+x", &target]).status()?;
    match std::process::Command::new(&target).arg("--version").output() {
        Ok(o) => println!("✅ sit updated to {}!", String::from_utf8_lossy(&o.stdout).trim()),
        _ => println!("⚠  Binary downloaded but version check failed."),
    }
    Ok(())
}

// ─── Main ──────────────────────────────────────────────────────────────────────

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
    } else { args[1..].join(" ") };

    if raw_input == "--version" || raw_input == "-v" { println!("sit v{}", env!("CARGO_PKG_VERSION")); return Ok(()); }
    if raw_input == "--update" || raw_input == "--upgrade" { return self_update().await; }
    if raw_input.starts_with("http://") || raw_input.starts_with("https://") {
        println!("🌐 Direct URL mode — scraping {}", raw_input);
        let client = build_client()?;
        install::pick_install(&client, &[], &[], &distro).await?; // dummy
        scraper::handle_direct_url(&client, &raw_input, &distro).await?;
        return Ok(());
    }

    let search_query = raw_input;
    let client = build_client()?;

    // ── Source selector ──────────────────────────────────────────────────────
    let src = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Search source")
        .items(&["  Search on GitHub", "  Search on Web"])
        .default(0)
        .interact()?;

    println!("🔍 Searching {} for '{}'...", if src == 0 { "GitHub" } else { "Web" }, search_query);

    let mut all_matches: Vec<AppMatch> = Vec::new();
    let mut search_failures: Vec<&str> = Vec::new();

    if src == 0 {  // GitHub
        let gh_url = format!("https://api.github.com/search/repositories?q={}+in:name&sort=stars&order=desc", urlencoding::encode(&search_query));
        match client.get(&gh_url).send().await {
            Ok(resp) => match resp.json::<serde_json::Value>().await {
                Ok(json) => {
                    if let Some(items) = json["items"].as_array() {
                        for item in items.iter().take(5) {
                            let name = item["full_name"].as_str().unwrap_or("").to_string();
                            if name.is_empty() { continue; }
                            let stars = item["stargazers_count"].as_u64().unwrap_or(0);
                            all_matches.push(AppMatch {
                                url: format!("https://api.github.com/repos/{}/releases/latest", name),
                                name: format!("{} (★{})", name, stars),
                                platform: "GitHub".to_string(),
                            });
                        }
                    }
                }
                Err(e) => { eprintln!("⚠  GitHub API parse error: {}", e); search_failures.push("GitHub"); }
            },
            Err(e) => { eprintln!("⚠  GitHub request failed: {}", e); search_failures.push("GitHub"); }
        }
    }

    // Web search (Bing → DDG)
    if src == 1 {
        let bing_url = format!("https://www.bing.com/search?q={}+linux+download", urlencoding::encode(&search_query));
        match client.get(&bing_url).send().await {
            Ok(resp) => match resp.text().await {
                Ok(html) => {
                    let wm = search::parse_bing_search(&html);
                    if wm.is_empty() { eprintln!("⚠  Bing: no results parsed"); }
                    else { all_matches.extend(wm); }
                }
                Err(e) => eprintln!("⚠  Bing body error: {}", e),
            },
            Err(e) => eprintln!("⚠  Bing request failed: {}", e),
        }

        // DDG fallback
        if !all_matches.iter().any(|m| m.platform == "Web") {
            println!("  ↪ Falling back to DuckDuckGo...");
            let ddg_url = format!("https://html.duckduckgo.com/html/?q={}", urlencoding::encode(&search_query));
            match client.get(&ddg_url).send().await {
                Ok(resp) => match resp.text().await {
                    Ok(html) => {
                        if is_ddg_captcha(&html) { eprintln!("⚠  DDG CAPTCHA — web search unavailable."); search_failures.push("DDG"); }
                        else { all_matches.extend(search::parse_search_html(&html, &search_query)); }
                    }
                    Err(e) => eprintln!("⚠  DDG body error: {}", e),
                },
                Err(e) => { eprintln!("⚠  DDG request failed: {}", e); search_failures.push("DDG"); }
            }
        }
    }

    if all_matches.is_empty() {
        if !search_failures.is_empty() { println!("❌ All search sources failed ({}).", search_failures.join(", ")); }
        else { println!("❌ No results found. Try a more specific name or paste the direct URL."); }
        return Ok(());
    }

    println!("\n  {:<10} │  {}", "Platform", "Name");
    println!("  {}", "─".repeat(10) + "─┼──" + &"─".repeat(62));
    let opts: Vec<String> = all_matches.iter().map(|a| format!("  {:<10} │  {}", a.platform, a.name)).collect();
    let sel = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Select source to install from").items(&opts).default(0).interact()?;

    let chosen = &all_matches[sel];
    println!("\n→ {} via {}", chosen.name, chosen.platform);
    match chosen.platform.as_str() {
        "GitHub" => github::handle_github(&client, &chosen.url, &chosen.name, &distro).await?,
        "Web"    => scraper::handle_direct_url(&client, &chosen.url, &distro).await?,
        _ => {}
    }
    Ok(())
}
