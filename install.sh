#!/usr/bin/env bash
# Install reettier from the latest GitHub Release.
# Usage: curl -fsSL https://raw.githubusercontent.com/reepolee/reettier/main/install.sh | bash

set -euo pipefail

APP="reettier"
OWNER="reepolee"
REPO="reettier"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"

# ──────────────────────────────────────────────
# Detect platform
# ──────────────────────────────────────────────

os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
	Darwin)
		case "$arch" in
			arm64|aarch64) asset_name="${APP}-macos-arm64" ;;
			x86_64)       asset_name="${APP}-macos-x64" ;;
			*)            echo "Unsupported macOS architecture: $arch" >&2; exit 1 ;;
		esac
		;;
	Linux)
		case "$arch" in
			x86_64|amd64)  asset_name="${APP}-linux-x64" ;;
			arm64|aarch64) asset_name="${APP}-linux-arm64" ;;
			*)             echo "Unsupported Linux architecture: $arch" >&2; exit 1 ;;
		esac
		;;
	*)
		echo "Unsupported OS: $os" >&2
		echo "Windows users: run the PowerShell install script instead:" >&2
		echo "  irm https://raw.githubusercontent.com/$OWNER/$REPO/main/install.ps1 | iex" >&2
		exit 1
		;;
esac

# ──────────────────────────────────────────────
# Download
# ──────────────────────────────────────────────

download_url="https://github.com/$OWNER/$REPO/releases/latest/download/$asset_name"

echo "→ Downloading $asset_name..."
if command -v curl &>/dev/null; then
	curl -fsSL "$download_url" -o "/tmp/$asset_name"
elif command -v wget &>/dev/null; then
	wget -q "$download_url" -O "/tmp/$asset_name"
else
	echo "ERROR: Neither curl nor wget found." >&2
	exit 1
fi

chmod +x "/tmp/$asset_name"

# ──────────────────────────────────────────────
# Install
# ──────────────────────────────────────────────

mkdir -p "$INSTALL_DIR"
cp "/tmp/$asset_name" "$INSTALL_DIR/$APP"
rm "/tmp/$asset_name"

echo "  Installed to $INSTALL_DIR/$APP"

# ──────────────────────────────────────────────
# PATH check
# ──────────────────────────────────────────────

if ! echo ":$PATH:" | grep -q ":$INSTALL_DIR:"; then
	shell_rc=""
	if [ -n "${ZSH_VERSION:-}" ]; then
		shell_rc="$HOME/.zshrc"
	elif [ -n "${BASH_VERSION:-}" ]; then
		shell_rc="$HOME/.bashrc"
	else
		shell_rc="$HOME/.profile"
	fi

	if ! grep -Fq "$INSTALL_DIR" "$shell_rc" 2>/dev/null; then
		{
			echo
			echo "export PATH=\"$INSTALL_DIR:\$PATH\""
		} >> "$shell_rc"
		echo "  Added $INSTALL_DIR to PATH in $shell_rc"
	fi

	echo ""
	echo "Restart your shell or run:"
	echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
fi

echo ""
echo "✅ reettier installed!"
"$INSTALL_DIR/$APP" --version
