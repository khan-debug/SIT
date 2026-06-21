<p align="center">
  <img src="https://img.shields.io/badge/Rust-1.96%2B-orange?logo=rust" alt="Rust">
  <img src="https://img.shields.io/badge/Linux-x86__64-blue?logo=linux" alt="Linux">
  <img src="https://img.shields.io/github/license/khan-debug/SIT" alt="License">
  <img src="https://img.shields.io/badge/status-stable-brightgreen" alt="Status">
</p>

<h1 align="center">🚀 SIT</h1>
<h3 align="center"><i>Not a package manager — but can manage packages.</i></h3>

<p align="center">
  <b>S</b>oftware <b>I</b>nstaller <b>T</b>ool — one command to search, download, and install<br>
  Linux software from GitHub releases, direct URLs, and the web.
</p>

---

## ✨ Why SIT?

**apt** has Ubuntu repos. **dnf** has Fedora repos. **SIT** has *everything* else.

```
sit brave          # Searches GitHub + web, fetches the latest release, installs it
sit zen-browser    # Downloads the AppImage, places it, creates a launcher
sit vscode         # Picks the right .deb or .rpm for your distro automatically
```

### 🔥 Blazing fast

Built in **Rust** — compiled, not interpreted. No runtime, no VM, no nonsense.

### ⚡ Fast downloads

Uses **axel** (multi-connection accelerator) when available for big files, falls back to **curl** otherwise. You get the fastest path every time.

### 🧠 Knows your system

Detects your distro (Debian/Ubuntu, Fedora/RHEL, or generic), picks the right package format, and installs it the right way — `.deb` via `apt`, `.rpm` via `dnf`, AppImages extracted and symlinked, archives unrolled to `~/.local/opt/` with desktop shortcuts created.

## 📦 Installation

```bash
curl -sSL https://raw.githubusercontent.com/khan-debug/SIT/main/install.sh | bash
```

That's it. Single binary in `~/.local/bin/sit`, ready to go.

## 🧰 Usage

```bash
# Search and install from GitHub
sit brave

# Multi-word search
sit visual studio code

# Install directly from a URL
sit https://zen-browser.app

# Direct GitHub repo URL
sit https://github.com/obsidianmd/obsidian-releases
```

### What happens:

1. **Search** — queries GitHub API (sorted by stars) and Brave Search simultaneously
2. **Select** — pick from a clean interactive menu
3. **Fetch** — downloads via axel (multi-connection) or curl
4. **Install** — auto-detects format and installs:
   - `.deb` → `apt install`
   - `.rpm` → `dnf install`
   - `.AppImage` → extracted to `~/.local/bin/`
   - `.tar.gz` / `.tar.xz` / `.zip` → extracted to `~/.local/opt/`
   - Desktop launcher created automatically

## 🗺️ Roadmap

- [x] GitHub release search & install
- [x] Direct URL scraping
- [x] Web search fallback
- [x] Distro-aware package selection
- [x] Axel multi-connection downloads

## ⚖️ License

MIT
