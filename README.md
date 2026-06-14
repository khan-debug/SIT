# SIT — Software Installer Tool

SIT is a streamlined command-line tool for Fedora and Ubuntu-based systems that simplifies searching for and installing software across multiple sources (DNF/APT, Flatpak, AppImage, etc.).

## Installation

Run the following command to install SIT directly on your system:

```bash
curl -sSL https://raw.githubusercontent.com/khan-debug/SIT/main/install.sh | bash
```

## Features

- **Unified Search:** Searches for packages across system repositories and Flathub.
- **Direct URL Support:** Install AppImages or binaries directly from a URL.
- **Automated Setup:** Handles dependency resolution and path configuration.

## Usage

```bash
sit zed                      # Search & install Zed editor
sit "visual studio code"      # Multi-word search
sit https://zen-browser.app  # Install directly from URL
```
