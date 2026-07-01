#!/usr/bin/env bash
# Release script for macOS and Linux.
# Builds the native binary for the current platform and publishes it as a GitHub Release.
# Version is auto-bumped (patch) only when the tag for the current version doesn't exist yet.
#
# Usage: bash release.sh [--draft] [--minor]
#   --draft  Create the release as a draft (default: published)
#   --minor  Bump the minor version instead of the patch version (default: patch)
#
# Branch mode: on any branch other than the default (main/master), this instead
# builds a local dev binary named "reettier-<branch>" (installed to ~/.local/bin)
# and skips all versioning/tagging/publishing — so you can test & compare a branch
# build side-by-side with the released "reettier". No gh auth required.
# Pass --release-branch to override this and force a real publish from a branch.
#
# Prerequisites:
#   - gh CLI (https://cli.github.com) — authenticated via `gh auth login`
#   - git
#
# Workflow (run on each machine after pushing code):
#   1. macOS (first): bash release.sh    → bumps version, creates tag + release, uploads
#   2. Linux:          bash release.sh    → builds, uploads to existing release
#   3. Windows:        .\release.ps1     → builds, uploads to existing release

set -euo pipefail

APP="reettier"

# ──────────────────────────────────────────────
# Branch mode: on any branch other than the default (main/master),
# build a suffixed dev binary for local test & compare instead of releasing.
# ──────────────────────────────────────────────

branch="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo "")"
default_branch="$(git symbolic-ref --short refs/remotes/origin/HEAD 2>/dev/null | sed 's#^origin/##')"
default_branch="${default_branch:-main}"

# --release-branch forces a real publish from a non-default branch (escape hatch).
release_branch=false
for arg in "$@"; do
	[ "$arg" = "--release-branch" ] && release_branch=true
done

if [ "$release_branch" = false ] && [ -n "$branch" ] && [ "$branch" != "$default_branch" ] && [ "$branch" != "master" ]; then
	# Sanitize branch name for use in a filename (feat/foo → feat-foo).
	suffix="$(echo "$branch" | sed 's#[^A-Za-z0-9._-]#-#g')"
	dev_binary="${APP}-${suffix}"

	echo "═══ reettier dev build ($branch) ═══"
	echo "  (Not on '$default_branch' — building '$dev_binary' locally, no release.)"
	echo ""
	echo "→ Building native release binary..."
	cargo build --release

	install_dir="$HOME/.local/bin"
	mkdir -p "$install_dir"
	cp "./target/release/$APP" "$install_dir/$dev_binary"
	chmod +x "$install_dir/$dev_binary"
	echo ""
	echo "✅ Installed $install_dir/$dev_binary"

	if ! echo ":$PATH:" | grep -q ":$install_dir:"; then
		echo "  Note: $install_dir is not on your PATH — run it directly or add it."
	fi
	echo "  Compare:  reettier <file>   vs   $dev_binary <file>"
	exit 0
fi

# ──────────────────────────────────────────────
# Validate prerequisites
# ──────────────────────────────────────────────

if ! command -v gh &>/dev/null; then
	echo "ERROR: gh CLI not found. Install it from https://cli.github.com/" >&2
	exit 1
fi

if ! gh auth status &>/dev/null; then
	echo "ERROR: gh CLI is not authenticated. Run: gh auth login" >&2
	exit 1
fi

# ──────────────────────────────────────────────
# Parse flags
# ──────────────────────────────────────────────

draft_flag=""
minor_bump=false
force=false

for arg in "$@"; do
	case "$arg" in
		--draft) draft_flag="--draft" ;;
		--minor) minor_bump=true ;;
		--force) force=true ;;
	esac
done

if [ -n "$draft_flag" ]; then
	echo "  (Draft mode)"
fi
if [ "$minor_bump" = true ]; then
	echo "  (Minor bump)"
fi
if [ "$force" = true ]; then
	echo "  (Force mode)"
fi

# ──────────────────────────────────────────────
# Read current version from Cargo.toml
# ──────────────────────────────────────────────

bump_patch() {
	local current="$1"
	local major="${current%%.*}"
	local rest="${current#*.}"
	local minor="${rest%%.*}"
	local patch="${rest#*.}"
	local new_patch=$((patch + 1))
	echo "$major.$minor.$new_patch"
}

bump_minor() {
	local current="$1"
	local major="${current%%.*}"
	local rest="${current#*.}"
	local minor="${rest%%.*}"
	local new_minor=$((minor + 1))
	echo "$major.$new_minor.0"
}

# Returns 0 (true) if $1 is a greater version than $2
version_gt() {
	local a_major="${1%%.*}"; local a_rest="${1#*.}"
	local a_minor="${a_rest%%.*}"; local a_patch="${a_rest#*.}"
	local b_major="${2%%.*}"; local b_rest="${2#*.}"
	local b_minor="${b_rest%%.*}"; local b_patch="${b_rest#*.}"

	[ "$a_major" -gt "$b_major" ] && return 0
	[ "$a_major" -lt "$b_major" ] && return 1
	[ "$a_minor" -gt "$b_minor" ] && return 0
	[ "$a_minor" -lt "$b_minor" ] && return 1
	[ "$a_patch" -gt "$b_patch" ]
}

version=$(awk -F'"' '/^version = /{print $2; exit}' Cargo.toml)
if [ -z "$version" ]; then
	echo "ERROR: Could not find version in Cargo.toml" >&2
	exit 1
fi

os="$(uname -s)"
arch="$(uname -m)"

# ──────────────────────────────────────────────
# Determine targets and native binary for this platform
# ──────────────────────────────────────────────

case "$os" in
	Darwin)
		targets=(
			"aarch64-apple-darwin:${APP}-macos-arm64"
			"x86_64-apple-darwin:${APP}-macos-x64"
		)
		case "$arch" in
			arm64|aarch64) native_binary="${APP}-macos-arm64" ;;
			x86_64)        native_binary="${APP}-macos-x64" ;;
			*)             echo "Unsupported arch: $arch" >&2; exit 1 ;;
		esac
		;;
	Linux)
		targets=(
			"x86_64-unknown-linux-gnu:${APP}-linux-x64"
			"aarch64-unknown-linux-gnu:${APP}-linux-arm64"
		)
		case "$arch" in
			x86_64|amd64)  native_binary="${APP}-linux-x64" ;;
			arm64|aarch64) native_binary="${APP}-linux-arm64" ;;
			*)             echo "Unsupported arch: $arch" >&2; exit 1 ;;
		esac
		;;
	*)
		echo "Unsupported OS: $os" >&2
		exit 1
		;;
esac

# ──────────────────────────────────────────────
# Detect code changes since last release
# ──────────────────────────────────────────────

git fetch --tags 2>/dev/null || true
latest_tag=$(git describe --tags --abbrev=0 --match 'v*' 2>/dev/null || echo "")

if [ -n "$latest_tag" ]; then
	# Verify local version matches the latest tag before proceeding.
	# If you run the release script without pulling first, versions will diverge.
	tag_version="${latest_tag#v}"
	if [ "$tag_version" != "$version" ]; then
		if version_gt "$tag_version" "$version"; then
			# Tag is ahead of Cargo.toml → secondary machine, use tag version
			echo "  (Note: latest tag is $tag_version, Cargo.toml has $version — using tag version)"
			version="$tag_version"
		else
			# Cargo.toml is ahead of the tag → partially completed prior run or manual bump
			if [ "$force" = true ]; then
				echo "  (Force: using Cargo.toml version $version, skipping bump)"
			else
				echo "ERROR: Cargo.toml version ($version) is ahead of latest tag ($tag_version)." >&2
				echo "  Did you forget to create a tag? Use --force to release with the current version." >&2
				exit 1
			fi
		fi
	fi

	new_commits=$(git rev-list HEAD "^$latest_tag" --count 2>/dev/null || echo "0")
else
	# No prior tag → this is the first release ever
	new_commits=1
fi

tag="v$version"

# When --force is used and Cargo.toml is already ahead of the tag, skip the bump
# (the version was already bumped by a prior partial run)
force_skip_bump=false
if [ "$force" = true ] && [ -n "$latest_tag" ]; then
	tag_version="${latest_tag#v}"
	if version_gt "$version" "$tag_version"; then
		force_skip_bump=true
	fi
fi

if [ "$new_commits" -gt 0 ] && [ "$force_skip_bump" = false ]; then
	# Code has changed since last release → bump version
	if [ "${minor_bump:-false}" = true ]; then
		new_version=$(bump_minor "$version")
		bump_type="minor"
	else
		new_version=$(bump_patch "$version")
		bump_type="patch"
	fi
	echo "═══ reettier release $new_version for $os ($arch) ═══"
	echo "  (Bumping $bump_type from $version → $new_version, $new_commits commits since $latest_tag)"

	# Update Cargo.toml
	sed -i '' "s/version = \"$version\"/version = \"$new_version\"/" Cargo.toml 2>/dev/null || \
	sed -i "s/version = \"$version\"/version = \"$new_version\"/" Cargo.toml

	version="$new_version"
	tag="v$version"
	do_bump=true

	# Update CHANGELOG.md with a new version heading
	if [ -f CHANGELOG.md ]; then
		today=$(date +%Y-%m-%d)
		if ! grep -q "^## \\[$version\\]" CHANGELOG.md 2>/dev/null; then
			first_version_line=$(grep -n "^## \\[" CHANGELOG.md | head -1 | cut -d: -f1)
			if [ -n "$first_version_line" ]; then
				{
					head -n $((first_version_line - 1)) CHANGELOG.md
					echo ""
					echo "## [$version] - $today"
					echo ""
					tail -n +"$first_version_line" CHANGELOG.md
				} > CHANGELOG.md.tmp && mv CHANGELOG.md.tmp CHANGELOG.md
				echo "  Updated CHANGELOG.md with version $version"
			fi
		fi
	fi
elif [ "$force_skip_bump" = true ]; then
	# --force: version already bumped in Cargo.toml, just commit and release
	echo "═══ reettier release $version for $os ($arch) ═══"
	echo "  (Force: resuming release for $version, $new_commits commits since $latest_tag)"
	do_bump=true

	# Still update CHANGELOG.md if needed
	if [ -f CHANGELOG.md ]; then
		today=$(date +%Y-%m-%d)
		if ! grep -q "^## \\[$version\\]" CHANGELOG.md 2>/dev/null; then
			first_version_line=$(grep -n "^## \\[" CHANGELOG.md | head -1 | cut -d: -f1)
			if [ -n "$first_version_line" ]; then
				{
					head -n $((first_version_line - 1)) CHANGELOG.md
					echo ""
					echo "## [$version] - $today"
					echo ""
					tail -n +"$first_version_line" CHANGELOG.md
				} > CHANGELOG.md.tmp && mv CHANGELOG.md.tmp CHANGELOG.md
				echo "  Updated CHANGELOG.md with version $version"
			fi
		fi
	fi
else
	# No code changes → just upload the binary
	echo "═══ reettier release $version for $os ($arch) ═══"
	echo "  (No new commits since $latest_tag. Uploading binary only.)"
	do_bump=false
fi

# ──────────────────────────────────────────────
# Build (all targets for this platform)
# ──────────────────────────────────────────────

built_assets=()
for entry in "${targets[@]}"; do
	target="${entry%%:*}"
	binary_name="${entry##*:}"
	echo ""
	echo "→ Building $binary_name ($target)..."
	rustup target add "$target" 2>/dev/null || true
	if [ "$os" = "Linux" ] && [ "$target" = "aarch64-unknown-linux-gnu" ]; then
		if ! command -v aarch64-linux-gnu-gcc &>/dev/null; then
			echo "  WARNING: aarch64-linux-gnu-gcc not found — skipping $binary_name."
			echo "  To enable ARM64 builds: sudo apt-get install gcc-aarch64-linux-gnu"
			continue
		fi
		CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
			cargo build --release --target "$target"
	else
		cargo build --release --target "$target"
	fi
	cp "./target/$target/release/$APP" "./$binary_name"
	file "./$binary_name"
	built_assets+=("./$binary_name#$binary_name")
done

# ──────────────────────────────────────────────
# Commit version bump (first machine only)
# ──────────────────────────────────────────────

if [ "$do_bump" = true ]; then
	echo ""
	echo "→ Committing version bump..."
	git add Cargo.toml Cargo.lock CHANGELOG.md
	git commit -m "Bump version to $version"
	echo "  Committed: Bump version to $version"
fi

# ──────────────────────────────────────────────
# Create and push git tag
# ──────────────────────────────────────────────

echo ""
echo "→ Tagging $tag..."

if git rev-parse "$tag" >/dev/null 2>&1; then
	echo "  Tag $tag already exists locally."
else
	git tag "$tag"
	echo "  Created tag $tag locally."
fi

# Push tag and (if bumped) the version bump commit together
if [ "$do_bump" = true ]; then
	echo "  Pushing version bump commit..."
	git push origin HEAD
fi

echo "  Pushing tag $tag to origin..."
git push origin "$tag"

# ──────────────────────────────────────────────
# Create or upload to GitHub Release
# ──────────────────────────────────────────────

echo ""
echo "→ Publishing release $tag..."

if gh release view "$tag" >/dev/null 2>&1; then
	echo "  Release $tag already exists. Uploading assets..."
	gh release upload "$tag" "${built_assets[@]}" --clobber
else
	echo "  Creating release $tag..."
	# Extract changelog entry for release notes
	notes_file=$(mktemp)
	if [ -f CHANGELOG.md ]; then
		awk "BEGIN{found=0} /^## \\[$version\\]/{found=1; next} /^## \\[/ && found{exit} found{print}" CHANGELOG.md > "$notes_file"
	fi
	if [ ! -s "$notes_file" ]; then
		echo "Release $tag" > "$notes_file"
	fi

	gh release create "$tag" \
		"${built_assets[@]}" \
		--title "$tag" \
		--notes-file "$notes_file" \
		$draft_flag

	rm -f "$notes_file"
fi

# ──────────────────────────────────────────────
# Install locally (to PATH)
# ──────────────────────────────────────────────

echo ""
echo "→ Installing locally ($native_binary)..."
install_dir="$HOME/.local/bin"
mkdir -p "$install_dir"
cp "./$native_binary" "$install_dir/$APP"
chmod +x "$install_dir/$APP"

if ! echo ":$PATH:" | grep -q ":$install_dir:"; then
	shell_rc=""
	if [ -n "${ZSH_VERSION:-}" ]; then
		shell_rc="$HOME/.zshrc"
	elif [ -n "${BASH_VERSION:-}" ]; then
		shell_rc="$HOME/.bashrc"
	else
		shell_rc="$HOME/.profile"
	fi

	if ! grep -Fq "$install_dir" "$shell_rc" 2>/dev/null; then
		{
			echo
			echo "export PATH=\"$install_dir:\$PATH\""
		} >> "$shell_rc"
		echo "  Added $install_dir to PATH in $shell_rc"
	fi
	echo "  Restart shell or run: export PATH=\"$install_dir:\$PATH\""
fi

echo "  Installed to $install_dir/$APP"

# Remove stale cargo-installed binary if present (avoids version conflicts)
cargo_bin="$HOME/.cargo/bin/$APP"
if [ -f "$cargo_bin" ]; then
    rm -f "$cargo_bin"
    echo "  Removed stale $cargo_bin"
fi

# ──────────────────────────────────────────────
# Cleanup copied binary from project root
# ──────────────────────────────────────────────

echo ""
echo "→ Cleaning up..."
for entry in "${targets[@]}"; do
	binary_name="${entry##*:}"
	rm -f "./$binary_name"
	echo "  Removed ./$binary_name"
done

# ──────────────────────────────────────────────
# Done
# ──────────────────────────────────────────────

echo ""
echo "✅ Done! Released ${#targets[@]} binaries → $tag"
echo "   View at: https://github.com/$(git remote get-url origin | sed -E 's|.*github.com[/:]||; s|\.git$||')/releases/tag/$tag"
