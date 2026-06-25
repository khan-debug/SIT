use crate::{AppMatch, resolve_search_url};
use scraper::{Html, Selector};

/// Known tutorial/blog/aggregator sites — not actual download sources.
const TUTORIAL_SITES: &[&str] = &[
    "linuxcapable.com", "itsfoss.com", "linuxconfig.org", "pkgs.org",
    "libtechnophile.blogspot.com", "tecmint.com", "fosslinux.com",
    "linuxbite.com", "debugpoint.com", "addictivetips.com",
    "cyberpanel.net", "support.brave.app",
    "alternativeto.net", "slant.co", "producthunt.com", "fosshub.com",
];

/// Parse DuckDuckGo HTML search results (used as fallback).
pub fn parse_search_html(html: &str, query: &str) -> Vec<AppMatch> {
    let document = Html::parse_document(html);
    let mut seen_urls: Vec<String> = Vec::new();
    let query_lower = query.to_lowercase();
    let query_words: Vec<&str> = query_lower.split_whitespace().collect();
    let mut scored: Vec<(i32, AppMatch)> = Vec::new();

    let link_sel = Selector::parse("a[href]").unwrap();
    for el in document.select(&link_sel) {
        let raw_href = el.value().attr("data-href")
            .or_else(|| el.value().attr("href"))
            .unwrap_or("")
            .to_string();

        let href = resolve_search_url(&raw_href).unwrap_or_else(|| {
            if raw_href.starts_with("//") { format!("https:{}", raw_href) } else { raw_href }
        });

        if href.is_empty()
            || href.contains("duckduckgo.com") || href.contains("bing.com")
            || href.contains("github.com") || href.contains("youtube.com")
            || href.contains("reddit.com") || href.contains("stackoverflow.com")
            || href.contains("wikipedia.org") || href.contains("searx") || href.contains("yacy")
        { continue; }
        if !href.starts_with("http") { continue; }

        let parsed = match url::Url::parse(&href) { Ok(u) => u, Err(_) => continue };
        let domain = parsed.host_str().unwrap_or("").to_string();
        if domain.is_empty() { continue; }

        let title = el.text().collect::<Vec<_>>().join(" ").trim().to_string();
        let lower_title = title.to_lowercase();

        let skip_keywords = ["sign in","sign up","login","register","cart",
            "shopping","pricing","subscribe","jobs","careers","contact"];
        if skip_keywords.iter().any(|k| lower_title.contains(k)) { continue; }
        if seen_urls.contains(&href) { continue; }
        seen_urls.push(href.clone());

        let mut score: i32 = 0;
        for w in &query_words {
            if lower_title.contains(*w) { score += 10; }
            if href.to_lowercase().contains(*w) { score += 5; }
        }
        let path = parsed.path().to_lowercase();
        if lower_title.contains("download") || path.contains("download") { score += 15; }
        if lower_title.contains("install")  || path.contains("install")  { score += 5; }
        if lower_title.contains("linux")    || href.to_lowercase().contains("linux") { score += 5; }
        if query_words.iter().any(|w| domain.contains(w)) { score += 10; }

        let domain_parts: Vec<&str> = domain.split('.').collect();
        if domain_parts.len() >= 2 {
            let sld = domain_parts[domain_parts.len() - 2];
            if query_words.iter().any(|w| w.eq_ignore_ascii_case(sld)) { score += 20; }
        }

        if path == "/" || path == "/download" || path == "/downloads" { score += 5; }
        if TUTORIAL_SITES.iter().any(|s| domain.contains(s)) { score -= 30; }

        let display_name = if !title.is_empty() && title.len() > 3 {
            let t = if title.chars().count() > 55 {
                format!("{}…", title.chars().take(52).collect::<String>())
            } else { title.clone() };
            format!("{} ({})", t, domain)
        } else {
            let clean = domain.replace(".com","").replace(".org","")
                .replace(".io","").replace("-"," ").trim().to_string();
            let cap = clean.split_whitespace()
                .map(|w| { let mut c = w.chars(); match c.next() { None => String::new(), Some(f) => f.to_uppercase().collect::<String>() + c.as_str() } })
                .collect::<Vec<_>>().join(" ");
            format!("{} ({})", cap, domain)
        };

        scored.push((score, AppMatch { name: display_name, url: href, platform: "Web".to_string() }));
    }

    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.truncate(5);
    scored.into_iter().map(|(_, m)| m).collect()
}

/// Parse Bing search results from <li class="b_algo"> elements.
pub fn parse_bing_search(html: &str) -> Vec<AppMatch> {
    let document = Html::parse_document(html);
    let mut matches = Vec::new();
    let mut seen_urls = Vec::new();

    if let Ok(algo_sel) = Selector::parse("li.b_algo") {
        if let Ok(link_sel) = Selector::parse("h2 a[href]") {
            for el in document.select(&algo_sel) {
                if let Some(link) = el.select(&link_sel).next() {
                    let raw_href = link.value().attr("href").unwrap_or("");
                    let resolved = resolve_search_url(raw_href).unwrap_or_default();
                    if resolved.is_empty() || !resolved.starts_with("http") { continue; }

                    let title = link.text().collect::<String>().trim().to_string();
                    if title.len() <= 3 { continue; }
                    if TUTORIAL_SITES.iter().any(|s| resolved.contains(s)) { continue; }
                    if seen_urls.contains(&resolved) { continue; }
                    seen_urls.push(resolved.clone());

                    let display = if title.chars().count() > 55 {
                        format!("{}…", title.chars().take(52).collect::<String>())
                    } else { title };

                    let domain = url::Url::parse(&resolved)
                        .ok().and_then(|u| u.host_str().map(String::from))
                        .unwrap_or_default();

                    matches.push(AppMatch { name: format!("{} ({})", display, domain), url: resolved, platform: "Web".to_string() });
                    if matches.len() >= 5 { break; }
                }
            }
        }
    }
    matches
}
