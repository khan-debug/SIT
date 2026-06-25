use crate::{Distro, is_valid_asset, sort_assets};

pub async fn handle_github(
    client: &reqwest::Client,
    releases_url: &str,
    repo_label: &str,
    distro: &Distro,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("📦 Fetching GitHub release assets...");
    let resp = client.get(releases_url).send().await?;
    let json = resp.json::<serde_json::Value>().await?;

    let release = if json.is_array() {
        json.as_array().and_then(|a| a.first()).cloned().unwrap_or(serde_json::Value::Null)
    } else { json };

    let assets = release["assets"].as_array().cloned().unwrap_or_default();
    if assets.is_empty() { println!("❌ No release assets found."); return Ok(()); }

    let mut valid: Vec<(String, String)> = Vec::new();
    let mut checksum_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    for a in &assets {
        let name = match a["name"].as_str() { Some(s) => s, None => continue };
        let url  = match a["browser_download_url"].as_str() { Some(s) => s, None => continue };
        if name.ends_with(".sha256") || name.ends_with(".sha512") {
            checksum_map.insert(name.trim_end_matches(".sha256").trim_end_matches(".sha512").to_string(), url.to_string());
            continue;
        }
        if is_valid_asset(name, distro) { valid.push((name.to_string(), url.to_string())); }
    }

    if valid.is_empty() {
        println!("❌ No compatible x86_64 binaries found in this release.");
        return Ok(());
    }

    sort_assets(&mut valid, distro);

    let names: Vec<String> = valid.iter()
        .map(|(n, _)| if checksum_map.contains_key(n) { format!("{} ✓sha256", n) } else { n.clone() })
        .collect();

    let sel = dialoguer::Select::with_theme(&dialoguer::theme::ColorfulTheme::default())
        .with_prompt("Select format to install")
        .items(&names).default(0).interact()?;

    let (fname, furl) = &valid[sel];
    let app_name = repo_label.split('/').next_back().unwrap_or("app").to_string();
    let csum = checksum_map.get(fname).cloned();
    crate::install::execute_install(client, fname, furl, &app_name, distro, csum.as_deref()).await
}
