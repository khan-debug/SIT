use crate::{CurlInstall, Distro, extract_fname, is_valid_asset, resolve_url};
use regex::Regex;
use scraper::{Html, Selector};

pub async fn fetch_page(client: &reqwest::Client, url: &str) -> Result<(String, String, String), ()> {
    let resp = client.get(url).send().await.map_err(|_| ())?;
    let final_url = resp.url().to_string();
    let content_type = resp.headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let html = resp.text().await.map_err(|_| ())?;
    Ok((final_url, content_type, html))
}

pub fn extract_github_repo(html: &str) -> Option<String> {
    let re = Regex::new(r#"github\.com/([A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+)"#).ok()?;
    for cap in re.captures_iter(html) {
        let repo = cap[1].to_string();
        if repo.starts_with("github/")
            || repo.contains("topics/") || repo.contains("sponsors/")
            || repo.ends_with(".git")
        { continue; }
        return Some(repo);
    }
    None
}

pub fn scrape_download_links(html: &str, page_url: &str, distro: &Distro) -> Vec<(String, String)> {
    let document = Html::parse_document(html);
    let mut links: Vec<(String, String)> = Vec::new();

    const URL_ATTRS: &[&str] = &["href","data-href","data-url","data-download","data-src","data-download-url"];

    if let Ok(sel) = Selector::parse("[href],[data-href],[data-url],[data-download],[data-src],[data-download-url],[onclick]") {
        let onclick_re = Regex::new(r#"https?://[^\s'"\)]+\.(?i:AppImage|deb|rpm|tar\.gz|tar\.xz|tar\.bz2|zip)"#).unwrap();

        for el in document.select(&sel) {
            let os_ok = el.value().attr("data-os")
                .map(|v| v.to_lowercase().contains("linux"))
                .unwrap_or(true);

            for attr in URL_ATTRS {
                if !os_ok && *attr != "href" { continue; }
                if let Some(val) = el.value().attr(attr) {
                    if let Some(abs) = resolve_url(page_url, val) {
                        let fname = extract_fname(&abs);
                        if is_valid_asset(&fname, distro) && !links.iter().any(|(n, _)| n == &fname) {
                            links.push((fname, abs));
                        }
                    }
                }
            }

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

    let url_re = Regex::new(r#"https?://[^\s"'<>]+\.(?i:AppImage|deb|rpm|tar\.gz|tar\.xz|tar\.bz2|zip)"#).unwrap();
    for cap in url_re.captures_iter(html) {
        let abs = cap[0].to_string();
        let fname = extract_fname(&abs);
        if is_valid_asset(&fname, distro) && !links.iter().any(|(n, _)| n == &fname) {
            links.push((fname, abs));
        }
    }

    links
}

pub fn extract_curl_installs(html: &str, base_url: &str) -> Vec<CurlInstall> {
    let mut installs: Vec<CurlInstall> = Vec::new();
    let document = Html::parse_document(html);

    // 1. <a href="*.sh"> links
    if let Ok(a_sel) = Selector::parse("a[href]") {
        for el in document.select(&a_sel) {
            if let Some(href) = el.value().attr("href") {
                if href.to_lowercase().ends_with(".sh") || href.to_lowercase().ends_with(".bash") {
                    if let Some(abs) = resolve_url(base_url, href) {
                        if !installs.iter().any(|i| i.url == abs) {
                            installs.push(CurlInstall {
                                display_name: format!("curl -fsSL {} | bash", abs),
                                url: abs,
                            });
                        }
                    }
                }
            }
        }
    }

    // 2. Regex patterns for install commands
    let patterns = [
        r#"(?:curl|wget)\s+(?:-[a-zA-Z0-9\s]+\s+)?https?://[^\s|<>"']+(?:\s*\|[^\s<>"']*)*\s*\|\s*(?:sudo\s+)?(?:ba)?sh"#,
        r#"(?:ba)?sh\s*<\s*\(\s*(?:curl|wget)\s+(?:-[a-zA-Z0-9\s]+)?(https?://[^\s)|"']+)"#,
        r#"(?:ba)?sh\s+-c\s*["']\s*(?:\$)?\(?(?:curl|wget)\s+(?:-[a-zA-Z0-9\s]+)?(https?://[^\s)"'|]+)"#,
    ];

    let url_re = Regex::new(r#"https?://[^\s|<>"']+"#).unwrap();
    for pattern in &patterns {
        if let Ok(re) = Regex::new(pattern) {
            for cap in re.captures_iter(html) {
                let full = cap[0].trim().to_string();
                let script_url = cap.get(1)
                    .map(|m| m.as_str().to_string())
                    .or_else(|| url_re.captures(&full).map(|u| u[0].to_string()))
                    .unwrap_or_default();
                if !script_url.is_empty() && !installs.iter().any(|i| i.url == script_url) {
                    installs.push(CurlInstall { display_name: full, url: script_url });
                }
            }
        }
    }

    // 3. <code>/<pre>/<samp> blocks
    for tag in &["code", "pre", "samp"] {
        if let Ok(sel) = Selector::parse(tag) {
            for el in document.select(&sel) {
                let text = el.text().collect::<String>();
                if !text.contains("curl") && !text.contains("wget") { continue; }
                if !text.contains("bash") && !text.contains("sh") { continue; }
                if let Some(url_cap) = url_re.captures(&text) {
                    let script_url = url_cap[0].to_string();
                    if !installs.iter().any(|i| i.url == script_url) {
                        installs.push(CurlInstall {
                            display_name: text.trim().lines().next().unwrap_or(&text).trim().to_string(),
                            url: script_url,
                        });
                    }
                }
            }
        }
    }

    installs
}

pub async fn handle_direct_url(
    client: &reqwest::Client,
    url: &str,
    distro: &Distro,
) -> Result<(), Box<dyn std::error::Error>> {
    let (final_url, content_type, html) = match fetch_page(client, url).await {
        Ok(v) => v,
        Err(_) => { println!("❌ Failed to fetch page: {}", url); return Ok(()); }
    };

    // Non-HTML response (e.g. get.docker.com returns shell script directly)
    if !content_type.contains("text/html") && !html.trim().is_empty() {
        if html.trim_start().starts_with("#!/") || html.trim_start().starts_with("# ") {
            println!("📜 Detected install script — offering as curl|bash install");
            return crate::install::execute_curl_install(&CurlInstall {
                url: final_url.clone(),
                display_name: format!("curl -fsSL {} | bash", final_url),
            }).await;
        }
    }

    println!("📄 Fetched: {}", final_url);

    let mut links        = scrape_download_links(&html, &final_url, distro);
    let mut curl_installs = extract_curl_installs(&html, &final_url);

    // Follow a /download sub-page if nothing found
    if links.is_empty() && curl_installs.is_empty() {
        println!("  ↪ Looking for a download page...");
        let document = Html::parse_document(&html);
        if let Ok(a_sel) = Selector::parse("a[href]") {
            let mut sub_url: Option<String> = None;
            for el in document.select(&a_sel) {
                if let Some(href) = el.value().attr("href") {
                    let text = el.text().collect::<String>().to_lowercase();
                    let hl = href.to_lowercase();
                    if (text.contains("download") || hl.contains("download") || hl.contains("releases"))
                        && !hl.contains("reddit") && !hl.contains("youtube")
                    {
                        if let Some(abs) = resolve_url(&final_url, href) {
                            if let (Ok(b), Ok(l)) = (url::Url::parse(&final_url), url::Url::parse(&abs)) {
                                if b.host_str() == l.host_str() { sub_url = Some(abs); break; }
                            }
                        }
                    }
                }
            }
            if let Some(ref dl_url) = sub_url {
                if let Ok((_, _, h2)) = fetch_page(client, dl_url).await {
                    links = scrape_download_links(&h2, dl_url, distro);
                    curl_installs = extract_curl_installs(&h2, dl_url);
                }
            }
        }
    }

    // Common download paths
    if links.is_empty() && curl_installs.is_empty() && !final_url.to_lowercase().contains("download") {
        println!("  ↪ Trying common download paths...");
        for suffix in ["/download", "/downloads", "/Download"] {
            let try_url = format!("{}{}", final_url.trim_end_matches('/'), suffix);
            if let Ok((_, _, h2)) = fetch_page(client, &try_url).await {
                links = scrape_download_links(&h2, &try_url, distro);
                curl_installs = extract_curl_installs(&h2, &try_url);
                if !links.is_empty() || !curl_installs.is_empty() { break; }
            }
        }
    }

    // Last resort: scan raw text for curl commands
    if links.is_empty() && curl_installs.is_empty() {
        let scan_re = Regex::new(r#"curl\s+(?:-[a-zA-Z0-9\s]+)?https?://[^\s<>"']+"#).unwrap();
        let url_re = Regex::new(r#"https?://[^\s|<>"']+"#).unwrap();
        for cap in scan_re.captures_iter(&html) {
            let cmd = cap[0].trim().to_string();
            if let Some(u) = url_re.captures(&cmd).map(|m| m[0].to_string()) {
                if !curl_installs.iter().any(|i| i.url == u) {
                    curl_installs.push(CurlInstall { display_name: cmd, url: u });
                }
            }
        }
        if !curl_installs.is_empty() {
            println!("  ↪ Found {} curl install command(s) in page text", curl_installs.len());
        }
    }

    // VS Code special case
    if links.is_empty() && curl_installs.is_empty()
        && (final_url.contains("code.visualstudio.com") || url.contains("code.visualstudio.com"))
    {
        println!("  ↪ Detected VS Code — using direct download URL");
        let (ext, dl) = match distro {
            Distro::Ubuntu  => ("deb",    "https://update.code.visualstudio.com/latest/linux-deb-x64/stable"),
            Distro::Fedora  => ("rpm",    "https://update.code.visualstudio.com/latest/linux-rpm-x64/stable"),
            Distro::Unknown => ("tar.gz", "https://update.code.visualstudio.com/latest/linux-x64/stable"),
        };
        links.push((format!("code.{}", ext), dl.to_string()));
    }

    // Firefox special case
    if links.is_empty() && curl_installs.is_empty()
        && (final_url.contains("firefox.com") || final_url.contains("mozilla.org"))
    {
        println!("  ↪ Detected Firefox — using direct download URL");
        links.push(("firefox.tar.xz".to_string(), "https://download.mozilla.org/?product=firefox-latest-ssl&os=linux64&lang=en-US".to_string()));
    }

    // GitHub pivot
    if links.is_empty() && curl_installs.is_empty() {
        if let Some(repo) = extract_github_repo(&html) {
            println!("  ↪ GitHub repo found: {}", repo);
            return crate::github::handle_github(client, &format!("https://api.github.com/repos/{}/releases/latest", repo), &repo, distro).await;
        }
        println!("❌ No installable packages or scripts found on this page.");
        println!("   Tip: paste a direct download URL instead.");
        return Ok(());
    }

    crate::install::pick_install(client, &links, &curl_installs, distro).await
}
