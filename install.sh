#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  sit — Software Installer Tool
#  One-shot installer: downloads pre-built binary (or builds from source),
#  sets up PATH, and installs axel (the multi-connection download accelerator).
#  Supports: Fedora / RHEL / Rocky  ·  Ubuntu / Debian / Mint / Pop!_OS
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

# ── Colours ──────────────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'
CYAN='\033[0;36m'; BOLD='\033[1m'; RESET='\033[0m'

info()    { echo -e "${CYAN}${BOLD}[sit]${RESET} $*"; }
success() { echo -e "${GREEN}${BOLD}[✓]${RESET} $*"; }
warn()    { echo -e "${YELLOW}${BOLD}[!]${RESET} $*"; }
die()     { echo -e "${RED}${BOLD}[✗]${RESET} $*" >&2; exit 1; }

# ── Banner ────────────────────────────────────────────────────────────────────
echo -e "
${BOLD}${CYAN}
  ███████╗██╗████████╗
  ██╔════╝██║╚══██╔══╝
  ███████╗██║   ██║
  ╚════██║██║   ██║
  ███████║██║   ██║
  ╚══════╝╚═╝   ╚═╝
${RESET}${BOLD}  Software Installer Tool — setup script${RESET}
"

# ── Detect distro ─────────────────────────────────────────────────────────────
detect_distro() {
    if [[ ! -f /etc/os-release ]]; then
        die "Cannot detect distro — /etc/os-release not found."
    fi
    # shellcheck disable=SC1091
    source /etc/os-release
    local id="${ID:-}" id_like="${ID_LIKE:-}"
    local combined="${id} ${id_like}"

    if echo "$combined" | grep -qiE 'fedora|rhel|centos|rocky|alma'; then
        echo "fedora"
    elif echo "$combined" | grep -qiE 'debian|ubuntu|mint|pop|elementary|kali|zorin'; then
        echo "ubuntu"
    else
        die "Unsupported distro: ${PRETTY_NAME:-unknown}. Only Fedora/RHEL and Debian/Ubuntu families are supported."
    fi
}

DISTRO=$(detect_distro)
info "Detected distro family: ${BOLD}${DISTRO}${RESET}"

# ── Require sudo ──────────────────────────────────────────────────────────────
if ! command -v sudo &>/dev/null; then
    die "sudo is required but not installed."
fi
info "Some steps need sudo — you may be prompted for your password."
sudo -v

# Keep sudo alive for the duration of the script
( while true; do sudo -n true; sleep 50; done ) 2>/dev/null &
SUDO_KEEPER_PID=$!
trap 'kill "$SUDO_KEEPER_PID" 2>/dev/null; exit' EXIT INT TERM

# ── Helpers ──────────────────────────────────────────────────────────────────
pkg_installed() { command -v "$1" &>/dev/null; }

LOCAL_BIN="${HOME}/.local/bin"
mkdir -p "$LOCAL_BIN"

# ─────────────────────────────────────────────────────────────────────────────
#  STEP 1 — Try pre-built binary from GitHub Releases
# ─────────────────────────────────────────────────────────────────────────────
info "Step 1/3 — Downloading pre-built binary..."

BINARY="${LOCAL_BIN}/sit"
DOWNLOAD_OK=false

if curl -fsSL -o "$BINARY" "https://github.com/khan-debug/SIT/releases/latest/download/sit" 2>/dev/null; then
    chmod +x "$BINARY"
    if "$BINARY" --version &>/dev/null; then
        DOWNLOAD_OK=true
    fi
fi

if $DOWNLOAD_OK; then
    export PATH="${LOCAL_BIN}:${PATH}"
    success "sit installed to ${LOCAL_BIN}/sit ($("$BINARY" --version))."
else
    warn "No pre-built binary found — building from source."

    # ── Build dependencies ───────────────────────────────────────────────────
    info "  Installing build dependencies..."

    BUILD_DEPS="curl wget pkg-config ca-certificates git"
    FEDORA_DEPS="gcc openssl-devel"
    UBUNTU_DEPS="build-essential libssl-dev"

    if [[ "$DISTRO" == "fedora" ]]; then
        sudo dnf groupinstall -y "Development Tools" &>/dev/null || true
        sudo dnf install -y $BUILD_DEPS $FEDORA_DEPS &>/dev/null
    else
        sudo apt-get update -qq
        sudo apt-get install -y $BUILD_DEPS $UBUNTU_DEPS &>/dev/null
    fi

    # ── Rust toolchain ──────────────────────────────────────────────────────
    if pkg_installed cargo; then
        RUST_VER=$(cargo --version 2>/dev/null | awk '{print $2}')
        success "  Rust already installed (cargo ${RUST_VER})."
    else
        info "  Installing Rust via rustup..."
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
            | sh -s -- -y --default-toolchain stable --profile minimal
        source "${HOME}/.cargo/env"
    fi

    if [[ -f "${HOME}/.cargo/env" ]]; then
        source "${HOME}/.cargo/env"
    fi

    if ! pkg_installed cargo; then
        die "cargo still not found after Rust install — something went wrong."
    fi

    # ── Clone & compile ─────────────────────────────────────────────────────
    SIT_SRC="${HOME}/.local/share/sit-src"

    if [[ -d "$SIT_SRC/.git" ]]; then
        info "  Existing clone found — pulling latest..."
        git -C "$SIT_SRC" pull --ff-only
    elif [[ -f "$(pwd)/Cargo.toml" ]] && grep -q 'name.*=.*"sit"' "$(pwd)/Cargo.toml" 2>/dev/null; then
        info "  Using sit source from current directory: $(pwd)"
        SIT_SRC="$(pwd)"
    elif [[ -f "$(pwd)/src/main.rs" ]]; then
        SIT_SRC="$(pwd)"
        info "  Using source from current directory."
    else
        info "  Cloning sit repository..."
        git clone --depth=1 https://github.com/khan-debug/SIT.git "$SIT_SRC"
    fi

    cd "$SIT_SRC"
    info "  Compiling (this takes 1-3 min on first run)..."
    cargo build --release 2>&1 | grep -E '^(error|warning\[|Compiling sit|Finished)' || true

    if [[ ! -f "${SIT_SRC}/target/release/sit" ]]; then
        die "Build failed — binary not found."
    fi

    cp "${SIT_SRC}/target/release/sit" "$BINARY"
    chmod +x "$BINARY"
    export PATH="${LOCAL_BIN}:${PATH}"

    success "sit built from source and installed to ${LOCAL_BIN}/sit."
fi

# ─────────────────────────────────────────────────────────────────────────────
#  STEP 2 — Install axel (download accelerator used by sit)
# ─────────────────────────────────────────────────────────────────────────────
info "Step 2/3 — Installing axel download accelerator..."

if pkg_installed axel; then
    AXEL_VER=$(axel --version 2>/dev/null | head -1 | grep -oP '\d+\.\d+\.\d+' || echo "available")
    success "axel already installed (${AXEL_VER})."
else
    info "  axel lets sit download packages 2-4× faster using multiple connections."
    if [[ "$DISTRO" == "fedora" ]]; then
        sudo dnf install -y axel &>/dev/null
    else
        sudo apt-get install -y axel &>/dev/null
    fi

    if pkg_installed axel; then
        success "axel installed."
    else
        warn "axel could not be installed — sit will fall back to curl for downloads."
    fi
fi

# ─────────────────────────────────────────────────────────────────────────────
#  STEP 3 — PATH setup
# ─────────────────────────────────────────────────────────────────────────────
info "Step 3/3 — Setting up PATH..."

PATH_LINE='export PATH="$HOME/.local/bin:$PATH"'
SHELLS_PATCHED=()

for RC in "${HOME}/.bashrc" "${HOME}/.zshrc" "${HOME}/.profile"; do
    if [[ -f "$RC" ]]; then
        if ! grep -q '.local/bin' "$RC" 2>/dev/null; then
            echo "" >> "$RC"
            echo "# sit — added by installer" >> "$RC"
            echo "$PATH_LINE" >> "$RC"
            SHELLS_PATCHED+=("$RC")
        fi
    fi
done

if [[ -f "${HOME}/.bash_profile" ]] && ! grep -q '.local/bin' "${HOME}/.bash_profile" 2>/dev/null; then
    echo "" >> "${HOME}/.bash_profile"
    echo "# sit — added by installer" >> "${HOME}/.bash_profile"
    echo "$PATH_LINE" >> "${HOME}/.bash_profile"
    SHELLS_PATCHED+=("${HOME}/.bash_profile")
fi

if [[ ${#SHELLS_PATCHED[@]} -gt 0 ]]; then
    success "Added ~/.local/bin to PATH in: ${SHELLS_PATCHED[*]}"
fi

# ── Flatpak remote (optional) ───────────────────────────────────────────────
if pkg_installed flatpak; then
    flatpak remote-add --if-not-exists --user flathub \
        https://dl.flathub.org/repo/flathub.flatpakrepo 2>/dev/null || true
fi

# ── Verify ────────────────────────────────────────────────────────────────────
echo ""
echo -e "${BOLD}────────────────────────────────────────────${RESET}"

SIT_PATH=$(command -v sit 2>/dev/null || echo "${LOCAL_BIN}/sit")
success "sit is ready!  (${SIT_PATH})"

if pkg_installed axel; then
    success "axel download accelerator ready."
else
    warn "axel not available — download speed may be slower. Install it with your package manager."
fi

echo ""
echo -e "${BOLD}  Usage:${RESET}"
echo -e "    ${CYAN}sit brave${RESET}                    — search & install Brave browser"
echo -e "    ${CYAN}sit \"visual studio code\"${RESET}      — multi-word search"
echo -e "    ${CYAN}sit https://zen-browser.app${RESET}  — install directly from URL"
echo ""
echo -e "${BOLD}  Reload your shell to use sit from anywhere:${RESET}"
echo -e "    ${CYAN}source ~/.bashrc${RESET}   (bash)"
echo -e "    ${CYAN}source ~/.zshrc${RESET}    (zsh)"
echo ""
echo -e "${BOLD}────────────────────────────────────────────${RESET}"
