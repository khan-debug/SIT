use dialoguer::{theme::ColorfulTheme, Select};
use reqwest::header::{HeaderMap, USER_AGENT, ACCEPT, ACCEPT_LANGUAGE};
use std::env;

#[derive(Clone)]
struct AppMatch {
    name: String,
    platform: String,
    url: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let search_query = if args.len() < 2 {
        println!("Enter application keyword:");
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        input.trim().to_string()
    } else {
        args[1].clone()
    };

    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, "Mozilla/5.0 (X11; Linux x86_64; rv:126.0) Gecko/20100101 Firefox/126.0".parse()?);
    headers.insert(ACCEPT, "application/json, text/html, */*".parse()?);
    headers.insert(ACCEPT_LANGUAGE, "en-US,en;q=0.5".parse()?);

    let client = reqwest::Client::builder()
        .default_headers(headers)
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()?;
        
    println!("Searching GitHub, Flathub, and SearXNG (Web) concurrently for '{}'...", search_query);

    let gh_url = format!("https://api.github.com/search/repositories?q={}", search_query);
    let flathub_url = format!("https://flathub.org/api/v2/search/{}", search_query);
    let searx_url = format!("https://searx.be/search?q={}+official+linux+download&format=json", search_query);

    let gh_request = client.get(&gh_url).send();
    let flathub_request = client.get(&flathub_url).send();
    let web_request = client.get(&searx_url).send();

    let (gh_res, flathub_res, web_res) = tokio::join!(gh_request, flathub_request, web_request);

    let mut all_matches: Vec<AppMatch> = Vec::new();

    if let Ok(resp) = gh_res {
        if let Ok(json) = resp.json::<serde_json::Value>().await {
            if let Some(items) = json["items"].as_array() {
                for item in items.iter().take(3) {
                    let name = item["full_name"].as_str().unwrap_or("").to_string();
                    all_matches.push(AppMatch {
                        url: format!("https://api.github.com/repos/{}/releases/latest", name),
                        name,
                        platform: "GitHub".to_string(),
                    });
                }
            }
        }
    }

    if let Ok(resp) = flathub_res {
        if let Ok(json) = resp.json::<serde_json::Value>().await {
            if let Some(hits) = json["hits"].as_array() {
                for hit in hits.iter().take(3) {
                    let id = hit["id"].as_str().unwrap_or("").to_string();
                    all_matches.push(AppMatch {
                        name: id.clone(),
                        url: id,
                        platform: "Flathub".to_string(),
                    });
                }
            }
        }
    }

    if let Ok(resp) = web_res {
        if let Ok(json) = resp.json::<serde_json::Value>().await {
            if let Some(results) = json["results"].as_array() {
                let mut count = 0;
                for result in results {
                    if count >= 4 { break; }
                    if let (Some(url), Some(title)) = (result["url"].as_str(), result["title"].as_str()) {
                        if !url.contains("github.com") && !url.contains("flathub.org") && !url.contains("youtube.com") {
                            let simple_name = url.replace("https://", "").replace("www.", "");
                            let short_name = simple_name.split('/').next().unwrap_or(&simple_name);
                            let clean_title = if title.len() > 30 { format!("{}...", &title[..27]) } else { title.to_string() };
                            
                            all_matches.push(AppMatch {
                                name: format!("{} ({})", short_name, clean_title),
                                url: url.to_string(),
                                platform: "Web".to_string(),
                            });
                            count += 1;
                        }
                    }
                }
            }
        }
    }

    if all_matches.is_empty() {
        println!("❌ No sources returned from any platform.");
        return Ok(());
    }

    let display_options: Vec<String> = all_matches
        .iter()
        .map(|app| format!("[{}] {}", app.platform, app.name))
        .collect();

    let selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Select source to install from")
        .items(&display_options)
        .default(0)
        .interact()?;

    let chosen = &all_matches[selection];
    println!("\nYou selected {} via {}.", chosen.name, chosen.platform);

    if chosen.platform == "GitHub" {
        println!("Fetching latest GitHub binary release...");
        if let Ok(resp) = client.get(&chosen.url).send().await {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                if let Some(assets) = json["assets"].as_array() {
                    let mut valid_assets = Vec::new();
                    for asset in assets {
                        let name = asset["name"].as_str().unwrap_or("");
                        let download_url = asset["browser_download_url"].as_str().unwrap_or("");
                        let name_lower = name.to_lowercase();
                        
                        // Strict architecture filtering to avoid architecture mismatch errors
                        if name_lower.contains("arm64") || name_lower.contains("aarch64") || name_lower.contains("armv7") {
                            continue;
                        }

                        if name.ends_with(".AppImage") || name.ends_with(".rpm") || name.ends_with(".tar.gz") {
                            valid_assets.push((name.to_string(), download_url.to_string()));
                        }
                    }
                    handle_github_install(&client, valid_assets, &chosen.name).await?;
                }
            }
        }
    } else if chosen.platform == "Flathub" {
        println!("Running system Flatpak installation...");
        std::process::Command::new("flatpak").args(["install", "flathub", &chosen.url, "-y"]).status()?;
    } else if chosen.platform == "Web" {
        println!("Scanning website landing page HTML for direct download binaries...");
        if let Ok(resp) = client.get(&chosen.url).send().await {
            if let Ok(page_html) = resp.text().await {
                let mut found_links = Vec::new();
                for token in page_html.split("href=\"") {
                    if let Some(link) = token.split('"').next() {
                        let link_lower = link.to_lowercase();
                        if link_lower.contains("arm64") || link_lower.contains("aarch64") {
                            continue;
                        }
                        if link.ends_with(".rpm") || link.ends_with(".AppImage") || link.ends_with(".tar.gz") {
                            let absolute_link = if link.starts_with("http") {
                                link.to_string()
                            } else {
                                format!("{}{}", chosen.url.trim_end_matches('/'), link)
                            };
                            let file_name = absolute_link.split('/').last().unwrap_or("downloaded_asset").to_string();
                            if !found_links.iter().any(|(n, _)| n == &file_name) {
                                found_links.push((file_name, absolute_link));
                            }
                        }
                    }
                }

                if found_links.is_empty() {
                    println!("❌ No direct binaries (.rpm, .AppImage, .tar.gz) exposed inside this page's root HTML.");
                    return Ok(());
                }

                let asset_names: Vec<String> = found_links.iter().map(|(name, _)| name.clone()).collect();
                let asset_selection = Select::with_theme(&ColorfulTheme::default())
                    .with_prompt("Select binary found on website")
                    .items(&asset_names)
                    .default(0)
                    .interact()?;

                let (chosen_name, chosen_url) = &found_links[asset_selection];
                execute_download_and_install(&client, chosen_name, chosen_url, "web-app").await?;
            }
        }
    }

    Ok(())
}

async fn handle_github_install(client: &reqwest::Client, valid_assets: Vec<(String, String)>, repo_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    if valid_assets.is_empty() {
        println!("❌ No compatible x86_64 binary formats (.AppImage, .rpm, .tar.gz) found.");
        return Ok(());
    }
    let asset_names: Vec<String> = valid_assets.iter().map(|(name, _)| name.clone()).collect();
    let asset_selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Select format to download")
        .items(&asset_names)
        .default(0)
        .interact()?;

    let (chosen_name, chosen_url) = &valid_assets[asset_selection];
    let app_short_name = repo_name.split('/').last().unwrap_or("app");
    execute_download_and_install(client, chosen_name, chosen_url, app_short_name).await
}

async fn execute_download_and_install(_client: &reqwest::Client, name: &str, url: &str, app_short_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("Downloading {} via axel (8 connections)...", name);
    
    let status = std::process::Command::new("axel").args(["-n", "8", "-a", url, "-o", name]).status();
    if !status.unwrap_or_default().success() {
        println!("❌ Download failed.");
        return Ok(());
    }

    let home = env::var("HOME").unwrap_or_else(|_| String::from("~"));
    let local_bin = format!("{}/.local/bin", home);
    let apps_dir = format!("{}/.local/share/applications", home);
    std::fs::create_dir_all(&local_bin)?;
    std::fs::create_dir_all(&apps_dir)?;

    if name.ends_with(".AppImage") {
        std::process::Command::new("chmod").args(["+x", name]).status()?;
        let target = format!("{}/{}", local_bin, app_short_name);
        std::fs::rename(name, &target)?;
        
        let desktop = format!("[Desktop Entry]\nName={}\nExec={}\nType=Application\nTerminal=false\n", app_short_name, target);
        std::fs::write(format!("{}/{}.desktop", apps_dir, app_short_name), desktop)?;
        println!("✅ Installed to {}. Added to App Launcher.", target);

    } else if name.ends_with(".rpm") {
        println!("Sudo password required for DNF installation:");
        if std::process::Command::new("sudo").args(["dnf", "install", "-y", name]).status()?.success() {
            std::fs::remove_file(name)?;
            println!("✅ RPM installed. Source deleted.");
        }
    } else if name.ends_with(".tar.gz") {
        let opt_dir = format!("{}/.local/opt/{}", home, app_short_name);
        // Wipe old failed extractions to keep directory pristine
        let _ = std::fs::remove_dir_all(&opt_dir);
        std::fs::create_dir_all(&opt_dir)?;
        
        std::process::Command::new("tar").args(["-xzf", name, "-C", &opt_dir]).status()?;
        std::fs::remove_file(name)?;

        // Exclude internal configuration, text, and shortcut metadata files from target matching
        let find_cmd = format!(
            "find {} -type f -executable ! -name '*.desktop' ! -name '*.txt' ! -name '*.sh' | grep -iE 'bin/|/{}' | head -n 1", 
            opt_dir, app_short_name
        );
        let output = std::process::Command::new("sh").args(["-c", &find_cmd]).output()?;
        let bin_path = String::from_utf8_lossy(&output.stdout).trim().to_string();

        if !bin_path.is_empty() {
            let symlink = format!("{}/{}", local_bin, app_short_name);
            std::process::Command::new("ln").args(["-sf", &bin_path, &symlink]).status()?;
            
            let desktop = format!("[Desktop Entry]\nName={}\nExec={}\nType=Application\nTerminal=false\n", app_short_name, bin_path);
            std::fs::write(format!("{}/{}.desktop", apps_dir, app_short_name), desktop)?;
            println!("✅ Extracted to {}. Linked to path and App Launcher.", opt_dir);
        } else {
            println!("✅ Extracted to {}. Could not auto-link binary.", opt_dir);
        }
    }
    std::process::Command::new("update-desktop-database").args([&apps_dir]).status().ok();
    Ok(())
}