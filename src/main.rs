use reqwest::header::USER_AGENT;
use std::env;
use std::fs::File;
use std::io::copy;

fn main() -> Result<(), Box> {
    let args: Vec = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: sit ");
        std::process::exit(1);
    }
    let repo = &args[1];
    let url = format!("https://api.github.com/repos/{}/releases/latest", repo);

    let client = reqwest::blocking::Client::new();
    let response: serde_json::Value = client
        .get(&url)
        .header(USER_AGENT, "sit-cli")
        .send()?
        .json()?;

    let assets = response["assets"].as_array().expect("No assets found or rate limited");
    println!("Found {} assets.", assets.len());

    for asset in assets {
        let name = asset["name"].as_str().unwrap_or("");
        let download_url = asset["browser_download_url"].as_str().unwrap_or("");

        if name.ends_with(".AppImage") || name.ends_with(".rpm") || name.ends_with(".tar.gz") {
            println!("Downloading: {}", name);
            let mut resp = client.get(download_url).header(USER_AGENT, "sit-cli").send()?;
            let mut out = File::create(name)?;
            copy(&mut resp, &mut out)?;
            println!("Download complete.");
            break;
        }
    }
    Ok(())
}
