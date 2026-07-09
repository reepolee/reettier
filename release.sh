#!/usr/bin/env bash
# Release script — builds ALL targets from a single machine (the Mac mini) and
# publishes them as one GitHub Release.
#
# reettier is pure Rust (no C/native deps), so every target cross-compiles cleanly
# from macOS:
#   - macOS arm64/x64  → native cargo build
#   - Linux  x64/arm64 → cargo zigbuild (Zig linker, handles glibc versioning)
#   - Windows x64/arm64→ cargo xwin build (auto-downloads the MSVC SDK + CRT)
#
# This replaces the old two-device pipeline (Mac + Windows). There is now a
# single releaser, so there is no release loop to worry about.
#
# Usage: bash release.sh [--draft] [--minor] [--force]
#   --draft  Create the release as a draft (default: published)
#   --minor  Bump the month component instead of the patch version (default: patch)
#   --force  Release the current Cargo.toml version even if it is ahead of the tag
#
# Prerequisites (one-time, on the Mac):
#   brew install zig
#   cargo install cargo-zigbuild cargo-xwin
#   rustup target add x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu \
#                     x86_64-pc-windows-msvc aarch64-pc-windows-msvc
#   gh CLI authenticated via `gh auth login`

set -euo pipefail

# Report the failing command, line, and exit code to stderr so reedash (and any
# other caller) surfaces a real error instead of a truncated header.
trap 'ec=$?; echo "ERROR: release.sh failed at line $LINENO: $BASH_COMMAND (exit $ec)" >&2' ERR

export PATH="$HOME/.cargo/bin:$PATH"

APP="reettier"

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

os="$(uname -s)"
if [ "$os" != "Darwin" ]; then
	echo "ERROR: release.sh cross-builds all targets and must run on macOS (the Mac mini)." >&2
	echo "  This machine is $os. Pull the release the Mac cuts instead of releasing here." >&2
	exit 1
fi

if ! command -v cargo-zigbuild &>/dev/null; then
	echo "ERROR: cargo-zigbuild not found. Install: cargo install cargo-zigbuild (and brew install zig)" >&2
	exit 1
fi

if ! command -v cargo-xwin &>/dev/null; then
	echo "ERROR: cargo-xwin not found. Install: cargo install cargo-xwin" >&2
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
# Version helpers
# ──────────────────────────────────────────────

bump_patch() {
	local current="$1"
	local year="${current%%.*}"
	local rest="${current#*.}"
	local month="${rest%%.*}"
	local patch="${rest#*.}"
	local new_patch=$((10#$patch + 1))
	echo "$year.$month.$new_patch"
}

bump_minor() {
	local current="$1"
	local year="${current%%.*}"
	local rest="${current#*.}"
	local month="${rest%%.*}"
	local patch="${rest#*.}"
	local new_month=$((10#$month + 1))

	if [ "$new_month" -gt 12 ]; then
		year=$((10#$year + 1))
		new_month=1
	fi

	printf "%s.%02d.0\n" "$year" "$new_month"
}

current_release_version() {
	local year month
	year=$(date +%y)
	month=$((10#$(date +%m)))
	echo "$year.$month.0"
}

format_release_version() {
	local current="$1"
	local year month patch
	local rest

	year="${current%%.*}"
	rest="${current#*.}"
	month="${rest%%.*}"
	patch="${rest#*.}"

	printf "%s.%02d.%s\n" "$year" "$((10#$month))" "$patch"
}

is_date_version() {
	[[ "$1" =~ ^[0-9]{2}\.[0-9]+\.[0-9]+$ ]]
}

# Returns 0 (true) if $1 and $2 are the same version numerically
version_eq() {
	local a_year a_month a_patch b_year b_month b_patch
	local a_rest b_rest

	a_year="${1%%.*}"; a_rest="${1#*.}"
	a_month="${a_rest%%.*}"; a_patch="${a_rest#*.}"
	b_year="${2%%.*}"; b_rest="${2#*.}"
	b_month="${b_rest%%.*}"; b_patch="${b_rest#*.}"

	a_year=$((10#$a_year))
	a_month=$((10#$a_month))
	a_patch=$((10#$a_patch))
	b_year=$((10#$b_year))
	b_month=$((10#$b_month))
	b_patch=$((10#$b_patch))

	[ "$a_year" -eq "$b_year" ] && [ "$a_month" -eq "$b_month" ] && [ "$a_patch" -eq "$b_patch" ]
}

# Returns 0 (true) if $1 is a greater version than $2
version_gt() {
	local a_year a_month a_patch b_year b_month b_patch
	local a_rest b_rest

	a_year="${1%%.*}"; a_rest="${1#*.}"
	a_month="${a_rest%%.*}"; a_patch="${a_rest#*.}"
	b_year="${2%%.*}"; b_rest="${2#*.}"
	b_month="${b_rest%%.*}"; b_patch="${b_rest#*.}"

	a_year=$((10#$a_year))
	a_month=$((10#$a_month))
	a_patch=$((10#$a_patch))
	b_year=$((10#$b_year))
	b_month=$((10#$b_month))
	b_patch=$((10#$b_patch))

	[ "$a_year" -gt "$b_year" ] && return 0
	[ "$a_year" -lt "$b_year" ] && return 1
	[ "$a_month" -gt "$b_month" ] && return 0
	[ "$a_month" -lt "$b_month" ] && return 1
	[ "$a_patch" -gt "$b_patch" ]
}

version=$(awk -F'"' '/^version = /{print $2; exit}' Cargo.toml)
if [ -z "$version" ]; then
	echo "ERROR: Could not find version in Cargo.toml" >&2
	exit 1
fi
release_version=$(format_release_version "$version")

# ──────────────────────────────────────────────
# All targets (built from this one Mac)
# ──────────────────────────────────────────────
# Each entry is "target:binary_name:builder", where builder is one of:
#   cargo    → plain cargo build (native macOS)
#   zigbuild → cargo zigbuild (Linux)
#   xwin     → cargo xwin build (Windows MSVC)

targets=(
	"aarch64-apple-darwin:${APP}-macos-arm64:cargo"
	"x86_64-apple-darwin:${APP}-macos-x64:cargo"
	"x86_64-unknown-linux-gnu:${APP}-linux-x64:zigbuild"
	"aarch64-unknown-linux-gnu:${APP}-linux-arm64:zigbuild"
	"x86_64-pc-windows-msvc:${APP}-windows-x64.exe:xwin"
	"aarch64-pc-windows-msvc:${APP}-windows-arm64.exe:xwin"
)

# Native binary for the local install (this Mac's arch)
arch="$(uname -m)"
case "$arch" in
	arm64|aarch64) native_binary="${APP}-macos-arm64" ;;
	x86_64)        native_binary="${APP}-macos-x64" ;;
	*)             echo "Unsupported arch: $arch" >&2; exit 1 ;;
esac

# ──────────────────────────────────────────────
# Detect code changes since last release
# ──────────────────────────────────────────────

git fetch --tags 2>/dev/null || true
latest_tag=$(git describe --tags --abbrev=0 --match 'v*' 2>/dev/null || echo "")

if [ -n "$latest_tag" ]; then
	# Verify local version matches the latest tag before proceeding.
	tag_version="${latest_tag#v}"
	if is_date_version "$version" && ! is_date_version "$tag_version"; then
		echo "  (Migrating from semver tag $tag_version to date-based Cargo.toml version $version)"
	elif version_eq "$tag_version" "$version"; then
		if [ "$tag_version" != "$release_version" ]; then
			echo "  (Normalizing tag $tag_version to zero-padded release version $release_version)"
		fi
	elif [ "$tag_version" != "$version" ]; then
		if version_gt "$tag_version" "$version"; then
			echo "  (Note: latest tag is $tag_version, Cargo.toml has $version — using tag version)"
			version="$tag_version"
		else
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
	new_commits=1
fi

tag="v$release_version"
migration_mode=false
if [ -n "$latest_tag" ]; then
	tag_version="${latest_tag#v}"
	if is_date_version "$version" && ! is_date_version "$tag_version"; then
		migration_mode=true
	fi
fi

# When --force is used and Cargo.toml is already ahead of the tag, skip the bump
force_skip_bump=false
if [ "$force" = true ] && [ -n "$latest_tag" ]; then
	tag_version="${latest_tag#v}"
	if version_gt "$version" "$tag_version"; then
		force_skip_bump=true
	fi
fi

if [ "$new_commits" -gt 0 ] && [ "$force_skip_bump" = false ] && [ "$migration_mode" = false ]; then
	# Code has changed since last release → bump version
	if [ "$minor_bump" = true ]; then
		new_version=$(bump_minor "$version")
		bump_type="month"
	else
		current_version=$(current_release_version)
		if version_gt "$current_version" "$version"; then
			new_version="$current_version"
			bump_type="date bucket"
		else
			new_version=$(bump_patch "$version")
			bump_type="patch"
		fi
	fi
	echo "═══ reettier release $new_version (all targets) ═══"
	echo "  (Bumping $bump_type from $version → $new_version, $new_commits commits since $latest_tag)"

	sed -i '' "s/version = \"$version\"/version = \"$new_version\"/" Cargo.toml 2>/dev/null || \
	sed -i "s/version = \"$version\"/version = \"$new_version\"/" Cargo.toml

	version="$new_version"
	release_version=$(format_release_version "$version")
	tag="v$release_version"
	do_bump=true

	if [ -f CHANGELOG.md ]; then
		today=$(date +%Y-%m-%d)
		if ! grep -q "^## \\[$version\\]" CHANGELOG.md 2>/dev/null; then
			first_version_line=$(grep -n "^## \\[" CHANGELOG.md | head -1 | cut -d: -f1 || true)
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
	echo "═══ reettier release $version (all targets) ═══"
	echo "  (Force: resuming release for $version, $new_commits commits since $latest_tag)"
	do_bump=true

	if [ -f CHANGELOG.md ]; then
		today=$(date +%Y-%m-%d)
		if ! grep -q "^## \\[$version\\]" CHANGELOG.md 2>/dev/null; then
			first_version_line=$(grep -n "^## \\[" CHANGELOG.md | head -1 | cut -d: -f1 || true)
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
	echo "═══ reettier release $version (all targets) ═══"
	echo "  (No new commits since $latest_tag. Rebuilding and re-uploading binaries.)"
	do_bump=false
fi

# ──────────────────────────────────────────────
# Build (all targets, cross-compiled from this Mac)
# ──────────────────────────────────────────────

built_assets=()
for entry in "${targets[@]}"; do
	target="${entry%%:*}"
	rest="${entry#*:}"
	binary_name="${rest%%:*}"
	builder="${rest##*:}"
	echo ""
	echo "→ Building $binary_name ($target via $builder)..."
	rustup target add "$target" 2>/dev/null || true

	case "$builder" in
		cargo)    cargo build --release --target "$target" ;;
		zigbuild) cargo zigbuild --release --target "$target" ;;
		xwin)     cargo xwin build --release --target "$target" ;;
	esac

	# Windows targets produce APP.exe; macOS/Linux produce APP.
	if [ "$builder" = "xwin" ]; then
		cp "./target/$target/release/$APP.exe" "./$binary_name"
	else
		cp "./target/$target/release/$APP" "./$binary_name"
	fi
	file "./$binary_name"
	built_assets+=("./$binary_name#$binary_name")
done

# ──────────────────────────────────────────────
# Commit version bump
# ──────────────────────────────────────────────

if [ "$do_bump" = true ]; then
	echo ""
	echo "→ Committing version bump..."
	git add Cargo.toml Cargo.lock; [ -f CHANGELOG.md ] && git add CHANGELOG.md || true
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
# Cleanup copied binaries from project root
# ──────────────────────────────────────────────

echo ""
echo "→ Cleaning up..."
for entry in "${targets[@]}"; do
	rest="${entry#*:}"
	binary_name="${rest%%:*}"
	rm -f "./$binary_name"
	echo "  Removed ./$binary_name"
done

# ──────────────────────────────────────────────
# Done
# ──────────────────────────────────────────────

echo ""
echo "✅ Done! Released ${#targets[@]} binaries → $tag"
echo "   View at: https://github.com/$(git remote get-url origin | sed -E 's|.*github.com[/:]||; s|\.git$||')/releases/tag/$tag"
