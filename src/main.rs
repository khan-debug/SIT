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
    headers.insert(ACCEPT, "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8".parse()?);
    headers.insert(ACCEPT_LANGUAGE, "en-US,en;q=0.5".parse()?);

    let client = reqwest::Client::builder()
        .default_headers(headers)
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()?;
        
    println!("Searching GitHub, System Flathub, and Web (Yahoo) concurrently for '{}'...", search_query);

    // 1. GitHub API
    let gh_url = format!("https://api.github.com/search/repositories?q={}", search_query);
    let gh_request = client.get(&gh_url).send();

    // 2. Web (Yahoo Search - lenient on bot blocking)
    let web_url = format!("https://search.yahoo.com/search?p={}+official+linux+download", search_query.replace(' ', "+"));
    let web_request = client.get(&web_url).send();

    // 3. System Flathub (Using native CLI instead of flaky APIs)
    let sq_clone = search_query.clone();
    let flatpak_task = tokio::task::spawn_blocking(move || {
        let output = std::process::Command::new("flatpak").args(["search", &sq_clone]).output().ok()?;
        Some(String::from_utf8_lossy(&output.stdout).to_string())
    });

    let (gh_res, web_res, flatpak_res) = tokio::join!(gh_request, web_request, flatpak_task);

    let mut all_matches: Vec<AppMatch> = Vec::new();

    // Parse GitHub
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

    // Parse Local Flathub Command Output
    if let Ok(Some(stdout)) = flatpak_res {
        let mut count = 0;
        for line in stdout.lines().skip(1) { // Skip header row
            if count >= 3 { break; }
            let cols: Vec<&str> = line.split('\t').collect();
            if cols.len() >= 3 {
                let id = cols[2].trim().to_string();
                all_matches.push(AppMatch {
                    name: id.clone(),
                    url: id,
                    platform: "Flathub".to_string(),
                });
                count += 1;
            }
        }
    }

    // Parse Web (Yahoo HTML Redirects)
    if let Ok(resp) = web_res {
        if let Ok(html) = resp.text().await {
            let mut count = 0;
            let mut seen_domains = Vec::new();

            // Yahoo wraps real URLs inside an RU= parameter
            for token in html.split("RU=") {
                if count >= 3 { break; }
                if let Some(encoded_url) = token.split("/RK=").next() {
                    if let Ok(decoded) = urlencoding::decode(encoded_url) {
                        let final_url = decoded.to_string();
                        if final_url.starts_with("http") 
                            && !final_url.contains("yahoo.com") 
                            && !final_url.contains("github.com") 
                            && !final_url.contains("flathub.org") 
                            && !final_url.contains("youtube.com") 
                        {
                            let simple_name = final_url.replace("https://", "").replace("http://", "").replace("www.", "");
                            let domain = simple_name.split('/').next().unwrap_or(&simple_name).to_string();
                            
                            if !seen_domains.contains(&domain) {
                                seen_domains.push(domain.clone());
                                all_matches.push(AppMatch {
                                    name: format!("{} (Web)", domain),
                                    url: final_url,
                                    platform: "Web".to_string(),
                                });
                                count += 1;
                            }
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
                    println!("💡 Tip: Try selecting the [Flathub] option if available.");
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
        let _ = std::fs::remove_dir_all(&opt_dir);
        std::fs::create_dir_all(&opt_dir)?;
        
        std::process::Command::new("tar").args(["-xzf", name, "-C", &opt_dir]).status()?;
        std::fs::remove_file(name)?;

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