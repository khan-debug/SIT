#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  sit — installer
#  Supports: Fedora / RHEL / Rocky  and  Ubuntu / Debian / Mint / Pop!_OS
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
# Pre-cache sudo credentials once so later calls don't prompt mid-install
info "Some steps need sudo — you may be prompted for your password."
sudo -v

# Keep sudo alive for the duration of the script
( while true; do sudo -n true; sleep 50; done ) 2>/dev/null &
SUDO_KEEPER_PID=$!
trap 'kill "$SUDO_KEEPER_PID" 2>/dev/null; exit' EXIT INT TERM

# ── Helper: install system packages ──────────────────────────────────────────
pkg_install() {
    # Usage: pkg_install <pkg-on-fedora> <pkg-on-ubuntu>
    local fedora_pkg="$1" ubuntu_pkg="$2"
    if [[ "$DISTRO" == "fedora" ]]; then
        sudo dnf install -y "$fedora_pkg" &>/dev/null
    else
        sudo apt-get install -y "$ubuntu_pkg" &>/dev/null
    fi
}

pkg_installed() {
    command -v "$1" &>/dev/null
}

# ── Step 1: System build dependencies ────────────────────────────────────────
info "Step 1/5 — Installing system build dependencies..."

if [[ "$DISTRO" == "fedora" ]]; then
    sudo dnf groupinstall -y "Development Tools" &>/dev/null || true
    sudo dnf install -y gcc curl wget openssl-devel pkg-config axel flatpak &>/dev/null
else
    sudo apt-get update -qq
    sudo apt-get install -y build-essential curl wget libssl-dev pkg-config axel flatpak &>/dev/null
fi

success "System packages installed."

# ── Step 2: Rust toolchain ────────────────────────────────────────────────────
info "Step 2/5 — Checking Rust toolchain..."

if pkg_installed cargo; then
    RUST_VER=$(cargo --version 2>/dev/null | awk '{print $2}')
    success "Rust already installed (cargo ${RUST_VER})."
else
    info "  Installing Rust via rustup..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain stable --profile minimal
    # Load cargo into this session immediately
    # shellcheck disable=SC1091
    source "${HOME}/.cargo/env"
    success "Rust installed ($(cargo --version))."
fi

# Make sure cargo env is active for the rest of the script
# (handles the case where rustup was already installed but env not loaded)
if [[ -f "${HOME}/.cargo/env" ]]; then
    # shellcheck disable=SC1091
    source "${HOME}/.cargo/env"
fi

if ! pkg_installed cargo; then
    die "cargo still not found after Rust install — something went wrong."
fi

# ── Step 3: Clone / update sit source ────────────────────────────────────────
info "Step 3/5 — Fetching sit source..."

SIT_SRC="${HOME}/.local/share/sit-src"

if [[ -d "$SIT_SRC/.git" ]]; then
    info "  Existing clone found — pulling latest..."
    git -C "$SIT_SRC" pull --ff-only
else
    # If the user already has sit source in the current directory, use that
    if [[ -f "$(pwd)/Cargo.toml" ]] && grep -q 'name.*=.*"sit"' "$(pwd)/Cargo.toml" 2>/dev/null; then
        info "  Using sit source from current directory: $(pwd)"
        SIT_SRC="$(pwd)"
    else
        # Try to clone from GitHub — update this URL once you push the repo
        REPO_URL="${SIT_REPO_URL:-}"
        if [[ -z "$REPO_URL" ]]; then
            # No git repo yet — use source files from current directory
            if [[ -f "$(pwd)/src/main.rs" ]]; then
                SIT_SRC="$(pwd)"
                info "  Using source from current directory."
            else
                die "No sit source found. Run this script from inside the sit project directory,\n   or set SIT_REPO_URL=https://github.com/you/sit before running."
            fi
        else
            git clone "$REPO_URL" "$SIT_SRC"
            info "  Cloned from ${REPO_URL}."
        fi
    fi
fi

success "Source ready at ${SIT_SRC}."

# ── Step 4: Compile ───────────────────────────────────────────────────────────
info "Step 4/5 — Compiling sit (this takes 1-3 min on first run)..."

cd "$SIT_SRC"
cargo build --release 2>&1 | grep -E '^(error|warning\[|Compiling sit|Finished)' || true

BINARY="${SIT_SRC}/target/release/sit"
if [[ ! -f "$BINARY" ]]; then
    die "Build failed — binary not found at ${BINARY}.\n   Run 'cargo build --release' manually in ${SIT_SRC} and check errors."
fi

success "Build complete."

# ── Step 5: Install binary + shell setup ─────────────────────────────────────
info "Step 5/5 — Installing sit to ~/.local/bin ..."

LOCAL_BIN="${HOME}/.local/bin"
mkdir -p "$LOCAL_BIN"
cp "$BINARY" "${LOCAL_BIN}/sit"
chmod +x "${LOCAL_BIN}/sit"

success "Binary installed to ${LOCAL_BIN}/sit."

# ── PATH setup ────────────────────────────────────────────────────────────────
# Add ~/.local/bin to PATH in every shell rc the user has
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

# Also patch .bash_profile if it exists and sources .bashrc (common on Fedora)
if [[ -f "${HOME}/.bash_profile" ]] && ! grep -q '.local/bin' "${HOME}/.bash_profile" 2>/dev/null; then
    echo "" >> "${HOME}/.bash_profile"
    echo "# sit — added by installer" >> "${HOME}/.bash_profile"
    echo "$PATH_LINE" >> "${HOME}/.bash_profile"
    SHELLS_PATCHED+=("${HOME}/.bash_profile")
fi

# Apply to the current session immediately
export PATH="${LOCAL_BIN}:${PATH}"

if [[ ${#SHELLS_PATCHED[@]} -gt 0 ]]; then
    success "Added ~/.local/bin to PATH in: ${SHELLS_PATCHED[*]}"
else
    info "~/.local/bin already in PATH — no changes needed."
fi

# ── Flatpak remote setup ───────────────────────────────────────────────────────
info "Ensuring Flathub remote is configured for flatpak..."
if pkg_installed flatpak; then
    flatpak remote-add --if-not-exists --user flathub \
        https://dl.flathub.org/repo/flathub.flatpakrepo 2>/dev/null || true
    success "Flathub remote ready."
else
    warn "flatpak not found — Flathub search will be unavailable."
fi

# ── Verify ────────────────────────────────────────────────────────────────────
echo ""
echo -e "${BOLD}────────────────────────────────────────────${RESET}"
if command -v sit &>/dev/null || [[ -x "${LOCAL_BIN}/sit" ]]; then
    SIT_PATH=$(command -v sit 2>/dev/null || echo "${LOCAL_BIN}/sit")
    success "sit is ready!  (${SIT_PATH})"
else
    warn "sit installed but not yet in PATH for this session."
fi

echo ""
echo -e "${BOLD}  Usage:${RESET}"
echo -e "    ${CYAN}sit zed${RESET}                      — search & install Zed editor"
echo -e "    ${CYAN}sit \"visual studio code\"${RESET}      — multi-word search"
echo -e "    ${CYAN}sit https://zen-browser.app${RESET}  — install directly from URL"
echo ""
echo -e "${BOLD}  Reload your shell to use sit from anywhere:${RESET}"
echo -e "    ${CYAN}source ~/.bashrc${RESET}   (bash)"
echo -e "    ${CYAN}source ~/.zshrc${RESET}    (zsh)"
echo ""
echo -e "${BOLD}────────────────────────────────────────────${RESET}"