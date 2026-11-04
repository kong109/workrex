#!/usr/bin/env bash
#
# workrex uninstall script
#
# Removes workrex binary, status tracking hooks, skills, and data
# directories. Handles all install methods.
#
# Usage: curl -fsSL https://raw.githubusercontent.com/kong109/workrex/main/scripts/uninstall-workrex.sh | bash
#
# IMPORTANT: This script must be EXECUTED, never SOURCED.

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log_info() { echo -e "${BLUE}==>${NC} $1"; }
log_success() { echo -e "${GREEN}==>${NC} $1"; }
log_warning() { echo -e "${YELLOW}==>${NC} $1"; }
log_error() { echo -e "${RED}Error:${NC} $1" >&2; }

echo ""
echo "  workrex uninstall"
echo ""

# 1. Detect install method and delegate to package manager if applicable
brew_path=""
if command -v brew &>/dev/null; then
    brew_path=$(brew --prefix workrex 2>/dev/null || true)
fi

cargo_bin="$HOME/.cargo/bin/workrex"
local_bin="$HOME/.local/bin/workrex"
usr_bin="/usr/local/bin/workrex"
workrex_path="$(command -v workrex 2>/dev/null || true)"

if [ -n "$brew_path" ]; then
    log_info "Detected Homebrew installation."
    log_info "Run these commands to finish uninstalling:"
    echo "  brew uninstall workrex"
    echo "  brew untap kong109/workrex"
    echo ""
    log_info "You can also run the remaining cleanup steps:"
fi

if [ "$workrex_path" = "$cargo_bin" ] || [ -f "$cargo_bin" ]; then
    log_info "Detected Cargo installation."
    log_info "Run this command to finish uninstalling:"
    echo "  cargo uninstall workmux --bin workrex"
    echo ""
    log_info "You can also run the remaining cleanup steps:"
fi

# 2. If workrex binary is available, run binary uninstall first
if [ -n "$workrex_path" ]; then
    log_info "Removing status tracking hooks and skills..."
    if ! workrex uninstall 2>&1 | sed 's/^/  /'; then
        log_warning "workrex uninstall encountered issues (see above)"
    fi
    echo ""
fi

# 3. Remove binary from known locations
log_info "Removing workrex binary..."
# WORKMUX_INSTALL_DIR and the PATH lookup cover a custom install directory.
custom_bin=""
if [ -n "${WORKMUX_INSTALL_DIR:-}" ]; then
    custom_bin="$WORKMUX_INSTALL_DIR/workrex"
fi
removed=""
for path in "$usr_bin" "$local_bin" "$cargo_bin" "$custom_bin" "$workrex_path"; do
    if [ -n "$path" ] && [ -f "$path" ] && [[ " $removed " != *" $path "* ]]; then
        removed="$removed $path"
        if [ -w "$(dirname "$path")" ]; then
            rm -f "$path"
            log_success "Removed $path"
        else
            log_info "Removing $path requires sudo..."
            sudo rm -f "$path"
            log_success "Removed $path"
        fi
    fi
done
if remaining="$(command -v workrex 2>/dev/null)"; then
    log_warning "A workrex binary is still on PATH at $remaining. Delete it yourself."
fi
echo ""

# 4. Manual cleanup guidance
log_info "Manual cleanup (check your shell config):"
echo "  - Remove 'eval \"\$(workrex completions bash)\"' or similar from"
echo "    ~/.bashrc, ~/.zshrc, ~/.config/fish/config.fish"
echo "  - Remove 'alias wm=workrex' from your shell config"
echo ""

log_info "Configuration preserved at ~/.config/workmux/"
echo "  It is shared with upstream workmux; remove it manually only if you use neither."
echo ""

log_info "Worktrees remain on disk:"
echo "  Clean up with: git worktree list && git worktree remove <path>"
echo ""

log_success "workrex has been uninstalled."
