use crate::{CurlInstall, Distro, auto_select_best_package,
            confirm_sudo_operation, extract_fname,
            sort_assets, write_desktop_file};
use indicatif::{ProgressBar, ProgressStyle};
use tokio::io::AsyncWriteExt;

/// Verify SHA-256 checksum of a downloaded file.
async fn verify_checksum(
    client: &reqwest::Client, file_path: &str, checksum_url: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    use sha2::{Digest, Sha256};
    let resp = match client.get(checksum_url).send().await {
        Ok(r) => r, Err(_) => return Ok(false),
    };
    let text = match resp.text().await { Ok(t) => t, Err(_) => return Ok(false) };

    let expected_hash = text.lines().find_map(|line| {
        let parts: Vec<&str> = line.trim().splitn(2, ' ').collect();
        if parts.len() == 2 && parts[0].len() == 64 { Some(parts[0].to_lowercase()) } else { None }
    });
    let expected_hash = match expected_hash {
        Some(h) => h,
        None => { println!("⚠  Checksum format unrecognised — skipping."); return Ok(false); }
    };

    let data = std::fs::read(file_path)?;
    let actual_hash = format!("{:x}", Sha256::digest(&data));
    if actual_hash == expected_hash { println!("✅ Checksum verified."); Ok(true) }
    else { Err(format!("Checksum MISMATCH!\n   expected: {}\n   actual:   {}", expected_hash, actual_hash).into()) }
}

pub async fn pick_install(
    client: &reqwest::Client, links: &[(String, String)], curl_installs: &[CurlInstall], distro: &Distro,
) -> Result<(), Box<dyn std::error::Error>> {
    let total = links.len() + curl_installs.len();
    if total == 0 { return Ok(()); }

    let mut sorted = links.to_vec();
    sort_assets(&mut sorted, distro);
    let best_index = if sorted.is_empty() { None } else { Some(auto_select_best_package(&sorted, distro)) };

    let mut names: Vec<String> = Vec::new();
    let mut choices: Vec<(String, String, String)> = Vec::new();

    for (i, (fname, furl)) in sorted.iter().enumerate() {
        let check = if best_index == Some(i) { ">" } else { " " };
        let icon = if fname.to_lowercase().ends_with(".appimage") { " [AppImage]" } else { " [pkg]" };
        names.push(format!("{}{}{}", check, fname, icon));
        choices.push(("download".to_string(), fname.clone(), furl.clone()));
    }
    for ci in curl_installs {
        names.push(format!("  {}", ci.display_name));
        choices.push(("curl".to_string(), ci.display_name.clone(), ci.url.clone()));
    }

    if total == 1 {
        let (kind, fname, url) = &choices[0];
        println!("🎯 {}", names[0]);
        return dispatch_install(client, kind, fname, url, distro).await;
    }

    println!();
    let sel = dialoguer::Select::with_theme(&dialoguer::theme::ColorfulTheme::default())
        .with_prompt("Select installation method").items(&names).default(best_index.unwrap_or(0)).interact()?;
    let (kind, fname, url) = &choices[sel];
    dispatch_install(client, kind, fname, url, distro).await
}

async fn dispatch_install(
    client: &reqwest::Client, kind: &str, fname: &str, url: &str, distro: &Distro,
) -> Result<(), Box<dyn std::error::Error>> {
    match kind {
        "download" => {
            let app_name = fname.split('_').next().and_then(|s| s.split('-').next()).unwrap_or("app").to_string();
            execute_install(client, fname, url, &app_name, distro, None).await
        }
        "curl" => execute_curl_install(&CurlInstall { url: url.to_string(), display_name: fname.to_string() }).await,
        _ => Ok(()),
    }
}

pub async fn execute_curl_install(install: &CurlInstall) -> Result<(), Box<dyn std::error::Error>> {
    let script_fname = extract_fname(&install.url);
    let tmp_dir = "/tmp/sit-downloads";
    std::fs::create_dir_all(tmp_dir)?;
    let dest = format!("{}/{}", tmp_dir, script_fname);

    println!("⬇  Downloading install script...");
    let dl_ok = std::process::Command::new("curl")
        .args(["-fsSL", "-o", &dest, &install.url])
        .status().map(|s| s.success()).unwrap_or(false);

    if !dl_ok || !std::path::Path::new(&dest).exists() {
        println!("❌ Failed to download script from {}", install.url);
        return Ok(());
    }

    if let Ok(content) = std::fs::read_to_string(&dest) {
        println!("\n📜 Script content ({} lines):", content.lines().count());
        println!("───────────────────────────────────────");
        for (i, line) in content.lines().enumerate() {
            println!("{:>4} │ {}", i + 1, line);
        }
        println!("───────────────────────────────────────");
    }

    println!("\nWould run: sudo bash {}", dest);
    if !dialoguer::Confirm::with_theme(&dialoguer::theme::ColorfulTheme::default())
        .with_prompt("Execute this script?").default(false).interact()?
    { println!("🚫 Cancelled. Script kept at {}", dest); return Ok(()); }

    println!("🚀 Running script...");
    match std::process::Command::new("sudo").args(["bash", &dest]).status() {
        Ok(s) if s.success() => { std::fs::remove_file(&dest).ok(); println!("✅ Script completed."); }
        Ok(s) => println!("❌ Script exited with: {}\n   Kept at {}", s, dest),
        Err(e) => println!("❌ Failed to run: {}\n   Kept at {}", e, dest),
    }
    Ok(())
}

/// Download with a coloured progress bar.
async fn download_file(
    client: &reqwest::Client, url: &str, dest: &str, label: &str,
) -> Result<u64, Box<dyn std::error::Error>> {
    let mut resp = client.get(url).send().await?;
    let total = resp.content_length().unwrap_or(0);

    let pb = ProgressBar::new(total);
    if total > 0 {
        pb.set_style(ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")?
            .progress_chars("#>-"));
    } else {
        pb.set_style(ProgressStyle::default_spinner()
            .template("{spinner:.green} {msg}")?);
    }
    pb.set_message(format!("⬇ {}", label));

    let mut file = tokio::fs::File::create(dest).await?;
    let mut downloaded: u64 = 0;
    while let Some(chunk) = resp.chunk().await? {
        file.write_all(&chunk).await?;
        downloaded += chunk.len() as u64;
        if total > 0 { pb.set_position(downloaded); }
    }
    pb.finish_and_clear();
    Ok(downloaded)
}

pub async fn execute_install(
    client: &reqwest::Client, name: &str, url: &str,
    app_short_name: &str, distro: &Distro, checksum_url: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let tmp_dir = "/tmp/sit-downloads";
    std::fs::create_dir_all(tmp_dir)?;
    let dest = format!("{}/{}", tmp_dir, name);

    if let Err(e) = download_file(client, url, &dest, name).await {
        println!("❌ Download failed: {}", e); return Ok(());
    }

    if let Some(csum_url) = checksum_url {
        println!("🔒 Verifying checksum...");
        match verify_checksum(client, &dest, csum_url).await {
            Ok(true) => {}
            Ok(false) => println!("⚠  No checksum available."),
            Err(e) => { std::fs::remove_file(&dest).ok(); return Err(e); },
        }
    } else { println!("⚠  No checksum found — proceeding without verification."); }

    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let local_bin = format!("{}/.local/bin", home);
    let apps_dir  = format!("{}/.local/share/applications", home);
    std::fs::create_dir_all(&local_bin)?;
    std::fs::create_dir_all(&apps_dir)?;

    let n = name.to_lowercase();
    if n.ends_with(".appimage") { install_appimage(&dest, app_short_name, &local_bin, &apps_dir)?; }
    else if n.ends_with(".deb") { install_deb(&dest, distro)?; }
    else if n.ends_with(".rpm") { install_rpm(&dest, distro)?; }
    else if n.ends_with(".tar.gz") || n.ends_with(".tar.xz") || n.ends_with(".tar.bz2") || n.ends_with(".zip") {
        install_archive(&dest, app_short_name, &home, &local_bin, &apps_dir)?;
    }

    std::process::Command::new("update-desktop-database").args([&apps_dir]).status().ok();
    Ok(())
}

fn install_appimage(name: &str, app_short_name: &str, local_bin: &str, apps_dir: &str) -> Result<(), Box<dyn std::error::Error>> {
    std::process::Command::new("chmod").args(["+x", name]).status()?;
    let target = format!("{}/{}", local_bin, app_short_name);
    std::fs::rename(name, &target)?;
    write_desktop_file(apps_dir, app_short_name, &target)?;
    println!("✅ AppImage installed to {}.", target);
    Ok(())
}

fn install_deb(name: &str, distro: &Distro) -> Result<(), Box<dyn std::error::Error>> {
    println!("🔐 Sudo required for .deb installation:");
    if !confirm_sudo_operation("install this .deb package")? { println!("🚫 Cancelled."); return Ok(()); }
    let ok = match distro {
        Distro::Ubuntu => std::process::Command::new("sudo").args(["apt","install","-y",name]).status().map(|s| s.success()).unwrap_or(false),
        _ => {
            let ok = std::process::Command::new("sudo").args(["dpkg","-i",name]).status().map(|s| s.success()).unwrap_or(false);
            if !ok { std::process::Command::new("sudo").args(["apt","-f","install","-y"]).status().ok(); }
            ok
        }
    };
    if ok { std::fs::remove_file(name).ok(); println!("✅ .deb installed."); }
    else { println!("❌ .deb installation failed. File kept at {}", name); }
    Ok(())
}

fn install_rpm(name: &str, distro: &Distro) -> Result<(), Box<dyn std::error::Error>> {
    println!("🔐 Sudo required for .rpm installation:");
    if !confirm_sudo_operation("install this .rpm package")? { println!("🚫 Cancelled."); return Ok(()); }
    let ok = match distro {
        Distro::Fedora => std::process::Command::new("sudo").args(["dnf","install","-y",name]).status().map(|s| s.success()).unwrap_or(false),
        _ => std::process::Command::new("sudo").args(["rpm","-i",name]).status().map(|s| s.success()).unwrap_or(false),
    };
    if ok { std::fs::remove_file(name).ok(); println!("✅ .rpm installed."); }
    else { println!("❌ .rpm installation failed. File kept at {}", name); }
    Ok(())
}

fn install_archive(name: &str, app_short_name: &str, home: &str, local_bin: &str, apps_dir: &str) -> Result<(), Box<dyn std::error::Error>> {
    let opt_dir = format!("{}/.local/opt/{}", home, app_short_name);
    if std::path::Path::new(&opt_dir).exists() { std::fs::remove_dir_all(&opt_dir)?; }
    std::fs::create_dir_all(&opt_dir)?;
    println!("📦 Extracting {} to {}", name, opt_dir);

    let n = name.to_lowercase();
    let strip = if n.ends_with(".zip") { false }
    else {
        let output = std::process::Command::new("tar").args(["-tf", name]).output();
        if let Ok(out) = output {
            let listing = String::from_utf8_lossy(&out.stdout);
            let top: std::collections::HashSet<&str> = listing.lines()
                .filter_map(|l| l.splitn(2, '/').next()).filter(|s| !s.is_empty()).collect();
            top.len() == 1
        } else { true }
    };

    let status = if n.ends_with(".zip") {
        std::process::Command::new("unzip").args(["-q", name, "-d", &opt_dir]).status()?
    } else {
        let flag = if n.ends_with(".tar.gz") { "-xzf" } else if n.ends_with(".tar.xz") { "-xJf" } else { "-xjf" };
        let mut cmd = std::process::Command::new("tar");
        cmd.arg(flag).arg(name).arg("-C").arg(&opt_dir);
        if strip { cmd.arg("--strip-components=1"); }
        cmd.status()?
    };
    if !status.success() { return Err(format!("Failed to extract: {}", name).into()); }
    std::fs::remove_file(name)?;

    let effective_dir = if n.ends_with(".zip") {
        let entries: Vec<_> = std::fs::read_dir(&opt_dir).map(|rd| rd.flatten().collect()).unwrap_or_default();
        if entries.len() == 1 { let single = entries[0].path(); if single.is_dir() { single.to_string_lossy().into_owned() } else { opt_dir.clone() } }
        else { opt_dir.clone() }
    } else { opt_dir.clone() };

    if let Some(bin) = find_executable(&effective_dir, app_short_name) {
        let symlink = format!("{}/{}", local_bin, app_short_name);
        if std::path::Path::new(&symlink).exists() { std::fs::remove_file(&symlink)?; }
        std::os::unix::fs::symlink(&bin, &symlink)?;
        write_desktop_file(apps_dir, app_short_name, &bin)?;
        println!("✅ Extracted to {}. Symlink: {} → {}", opt_dir, symlink, bin);
    } else {
        println!("✅ Extracted to {}.", opt_dir);
        println!("⚠  Could not locate main executable — no symlink created.");
    }
    Ok(())
}

fn find_executable(dir: &str, app_name: &str) -> Option<String> {
    let mut candidates: Vec<(i32, String)> = Vec::new();
    fn walk(path: &str, name: &str, candidates: &mut Vec<(i32, String)>) {
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                let p = entry.path();
                let ps = p.to_string_lossy().into_owned();
                if p.is_dir() { walk(&ps, name, candidates); }
                else if p.is_file() {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(m) = std::fs::metadata(&p) {
                        if m.permissions().mode() & 0o111 != 0 {
                            let fname = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
                            if !fname.starts_with("lib") && !fname.ends_with(".so") && !fname.ends_with(".so.")
                                && !fname.ends_with(".desktop") && !fname.ends_with(".txt")
                                && !fname.ends_with(".md") && !fname.ends_with(".sh")
                                && !fname.ends_with(".py") && !fname.ends_with(".rb")
                            {
                                let mut score = 0;
                                if ps.contains("/bin/") { score += 2; }
                                if ps.contains(&format!("/{}", name)) { score += 1; }
                                candidates.push((score, ps));
                            }
                        }
                    }
                }
            }
        }
    }
    walk(dir, app_name, &mut candidates);
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    candidates.into_iter().map(|(_, p)| p).next()
}
