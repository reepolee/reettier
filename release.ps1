# Release script for Windows.
# Builds the native binary and publishes it as a GitHub Release.
# Version is auto-bumped (patch) only when the tag for the current version doesn't exist yet.
#
# Usage: .\release.ps1 [-Draft] [-Minor]
#   -Draft  Create the release as a draft (default: published)
#   -Minor  Bump the minor version instead of the patch version (default: patch)
#
# Branch mode: on any branch other than the default (main/master), this instead
# builds a local dev binary named "reettier-<branch>.exe" (installed to ~\bin) and
# skips all versioning/tagging/publishing — so you can test & compare a branch
# build side-by-side with the released "reettier". No gh auth required.
# Pass -ReleaseBranch to override this and force a real publish from a branch.
#
# Prerequisites:
#   - gh CLI (https://cli.github.com) — authenticated via `gh auth login`
#   - git
#
# Workflow (run on each machine after pushing code):
#   1. macOS (first): bash release.sh     -> bumps version, creates tag + release, uploads
#   2. Linux:          bash release.sh     -> builds, uploads to existing release
#   3. Windows:        .\release.ps1      -> builds, uploads to existing release

param(
    [switch]$Draft,
    [switch]$Minor,
    [switch]$ReleaseBranch
)

$ErrorActionPreference = "Stop"
$AppName = "reettier"

# ──────────────────────────────────────────────
# Branch mode: on any branch other than the default (main/master),
# build a suffixed dev binary for local test & compare instead of releasing.
# ──────────────────────────────────────────────

$branch = (git rev-parse --abbrev-ref HEAD 2>$null)
$defaultBranch = (git symbolic-ref --short refs/remotes/origin/HEAD 2>$null)
if ($defaultBranch) { $defaultBranch = $defaultBranch -replace '^origin/', '' } else { $defaultBranch = 'main' }

# -ReleaseBranch forces a real publish from a non-default branch (escape hatch).
if (-not $ReleaseBranch -and $branch -and $branch -ne $defaultBranch -and $branch -ne 'master') {
    # Sanitize branch name for use in a filename (feat/foo -> feat-foo).
    $suffix = $branch -replace '[^A-Za-z0-9._-]', '-'
    $devBinary = "$AppName-$suffix.exe"

    Write-Host "═══ reettier dev build ($branch) ═══"
    Write-Host "  (Not on '$defaultBranch' — building '$devBinary' locally, no release.)"
    Write-Host "`n→ Building native release binary..."
    cargo build --release
    if ($LASTEXITCODE -ne 0) { Write-Error "Build failed"; exit 1 }

    $InstallDir = Join-Path $HOME "bin"
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    Copy-Item ".\target\release\$AppName.exe" (Join-Path $InstallDir $devBinary) -Force
    Write-Host "`n✅ Installed $(Join-Path $InstallDir $devBinary)"

    $UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if (($UserPath -split ";") -notcontains $InstallDir) {
        Write-Host "  Note: $InstallDir is not on your PATH — run it directly or add it."
    }
    Write-Host "  Compare:  reettier <file>   vs   $devBinary <file>"
    exit 0
}

# ──────────────────────────────────────────────
# Validate prerequisites
# ──────────────────────────────────────────────

if (-not (Get-Command "gh" -ErrorAction SilentlyContinue)) {
    Write-Error "gh CLI not found. Install it from https://cli.github.com/"
    exit 1
}

$authStatus = gh auth status 2>&1
if ($LASTEXITCODE -ne 0) {
    Write-Error "gh CLI is not authenticated. Run: gh auth login"
    exit 1
}

# ──────────────────────────────────────────────
# Read current version from Cargo.toml
# ──────────────────────────────────────────────

$cargoContent = Get-Content "Cargo.toml" -Raw
$versionMatch = [regex]::Match($cargoContent, 'version = "(\d+\.\d+\.\d+)"')
if (-not $versionMatch.Success) {
    Write-Error "Could not find version in Cargo.toml"
    exit 1
}

$version = $versionMatch.Groups[1].Value

# Detect native arch for local install
$arch = $env:PROCESSOR_ARCHITECTURE
$nativeBinary = switch ($arch) {
    'AMD64' { "$AppName-windows-x64.exe" }
    'ARM64' { "$AppName-windows-arm64.exe" }
    default { Write-Error "Unsupported architecture: $arch"; exit 1 }
}

$targets = @(
    @{ Target = "x86_64-pc-windows-msvc";   BinaryName = "$AppName-windows-x64.exe" }
    @{ Target = "aarch64-pc-windows-msvc";  BinaryName = "$AppName-windows-arm64.exe" }
)

# ──────────────────────────────────────────────
# Detect code changes since last release
# ──────────────────────────────────────────────

git fetch --tags 2>$null | Out-Null

$latestTag = git describe --tags --abbrev=0 --match 'v*' 2>$null
$newCommits = 0

if ($latestTag) {
    # Verify local version matches the latest tag before proceeding.
    # If you run the release script without pulling first, versions will diverge.
    $tagVersion = $latestTag.TrimStart('v')
    if ($tagVersion -ne $version) {
        if ([version]$tagVersion -gt [version]$version) {
            # Tag is ahead of Cargo.toml → secondary machine, use tag version
            Write-Host "  (Note: latest tag is $tagVersion, Cargo.toml has $version — using tag version)"
            $version = $tagVersion
        } else {
            # Cargo.toml is ahead of the tag → manually bumped without a release
            Write-Error "Cargo.toml version ($version) is ahead of latest tag ($tagVersion). Did you forget to create a tag?"
            exit 1
        }
    }

    $newCommits = [int](git rev-list HEAD "^$latestTag" --count 2>$null)
} else {
    # No prior tag → this is the first release ever
    $newCommits = 1
}

$tag = "v$version"
$doBump = $false

if ($newCommits -gt 0) {
    # Code has changed since last release → bump version
    $parts = $version -split '\.'
    if ($Minor) {
        $newVersion = "$($parts[0]).$([int]$parts[1] + 1).0"
        $bumpType = "minor"
    } else {
        $newVersion = "$($parts[0]).$($parts[1]).$([int]$parts[2] + 1)"
        $bumpType = "patch"
    }

    Write-Host "═══ reettier release $newVersion for Windows ═══"
    Write-Host "  (Bumping $bumpType from $version -> $newVersion, $newCommits commits since $latestTag)"

    $cargoContent = Get-Content "Cargo.toml" -Raw
    $cargoContent = $cargoContent -replace 'version = "\d+\.\d+\.\d+"', "version = `"$newVersion`""
    Set-Content "Cargo.toml" -Value $cargoContent

    $version = $newVersion
    $tag = "v$version"
    $doBump = $true

    # Update CHANGELOG.md with a new version heading
    if (Test-Path "CHANGELOG.md") {
        $today = Get-Date -Format "yyyy-MM-dd"
        $lines = Get-Content "CHANGELOG.md"
        $hasEntry = $false
        foreach ($line in $lines) {
            if ($line -match "^## \[$version\]") {
                $hasEntry = $true
                break
            }
        }
        if (-not $hasEntry) {
            $insertLine = 0
            for ($i = 0; $i -lt $lines.Count; $i++) {
                if ($lines[$i] -match "^## \[") {
                    $insertLine = $i
                    break
                }
            }
            if ($insertLine -gt 0) {
                $newContent = @()
                for ($i = 0; $i -lt $insertLine; $i++) {
                    $newContent += $lines[$i]
                }
                $newContent += ""
                $newContent += "## [$version] - $today"
                $newContent += ""
                for ($i = $insertLine; $i -lt $lines.Count; $i++) {
                    $newContent += $lines[$i]
                }
                Set-Content "CHANGELOG.md" -Value $newContent
                Write-Host "  Updated CHANGELOG.md with version $version"
            }
        }
    }
} else {
    # No code changes → just upload the binary
    Write-Host "═══ reettier release $version for Windows ═══"
    Write-Host "  (No new commits since $latestTag. Uploading binary only.)"
    $doBump = $false
}

# ──────────────────────────────────────────────
# Build (all targets for Windows)
# ──────────────────────────────────────────────

$builtAssets = @()
foreach ($t in $targets) {
    Write-Host "`n→ Building $($t.BinaryName) ($($t.Target))..."
    rustup target add $t.Target 2>$null | Out-Null
    cargo build --release --target $t.Target
    if ($LASTEXITCODE -ne 0) {
        if ($t.Target -eq "aarch64-pc-windows-msvc") {
            Write-Host "  WARNING: ARM64 build failed — skipping."
            Write-Host "  To enable ARM64 builds, install the MSVC ARM64 toolchain:"
            Write-Host "    Visual Studio Installer → Modify → Individual Components"
            Write-Host "    → 'MSVC v143 - VS 2022 C++ ARM64 build tools'"
            continue
        }
        Write-Error "Build failed for $($t.Target)"; exit 1
    }
    Copy-Item ".\target\$($t.Target)\release\$AppName.exe" ".\$($t.BinaryName)"
    $builtAssets += ".\$($t.BinaryName)#$($t.BinaryName)"
}

# ──────────────────────────────────────────────
# Commit version bump (first machine only)
# ──────────────────────────────────────────────

if ($doBump) {
    Write-Host "`n→ Committing version bump..."
    git add Cargo.toml CHANGELOG.md
    if ($LASTEXITCODE -ne 0) { Write-Error "git add failed"; exit 1 }
    git commit -m "Bump version to $version"
    if ($LASTEXITCODE -ne 0) { Write-Error "git commit failed"; exit 1 }
    Write-Host "  Committed: Bump version to $version"
}

# ──────────────────────────────────────────────
# Create and push git tag
# ──────────────────────────────────────────────

Write-Host "`n→ Tagging $tag..."

$tagLocal = git rev-parse $tag 2>$null
if ($LASTEXITCODE -eq 0) {
    Write-Host "  Tag $tag already exists locally."
} else {
    git tag $tag
    Write-Host "  Created tag $tag locally."
}

# Push tag and (if bumped) the version bump commit together
if ($doBump) {
    Write-Host "  Pushing version bump commit..."
    git push origin HEAD
}

Write-Host "  Pushing tag $tag to origin..."
git push origin $tag

# ──────────────────────────────────────────────
# Create or upload to GitHub Release
# ──────────────────────────────────────────────

Write-Host "`n→ Publishing release $tag..."

$releaseExists = gh release view $tag 2>$null
if ($LASTEXITCODE -eq 0) {
    Write-Host "  Release $tag already exists. Uploading assets..."
    gh release upload $tag @builtAssets --clobber
} else {
    Write-Host "  Creating release $tag..."
    $notesFile = [System.IO.Path]::GetTempFileName()
    if (Test-Path "CHANGELOG.md") {
        $inSection = $false
        $notes = @()
        Get-Content "CHANGELOG.md" | ForEach-Object {
            if ($_ -match "^## \[$version\]") {
                $inSection = $true
            } elseif ($_ -match "^## \[" -and $inSection) {
                $inSection = $false
            } elseif ($inSection) {
                $notes += $_
            }
        }
        if ($notes.Count -gt 0) {
            Set-Content $notesFile -Value $notes
        } else {
            Set-Content $notesFile -Value "Release $tag"
        }
    } else {
        Set-Content $notesFile -Value "Release $tag"
    }

    $releaseArgs = @(
        "release", "create", $tag
    ) + $builtAssets + @(
        "--title", $tag,
        "--notes-file", $notesFile
    )
    if ($Draft) {
        $releaseArgs += "--draft"
        Write-Host "  (Draft mode)"
    }
    gh @releaseArgs
    Remove-Item $notesFile -Force
}

# ──────────────────────────────────────────────
# Install locally (to PATH)
# ──────────────────────────────────────────────

Write-Host "`n→ Installing locally ($nativeBinary)..."
$InstallDir = Join-Path $HOME "bin"
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
Copy-Item ".\$nativeBinary" (Join-Path $InstallDir "$AppName.exe") -Force

$UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
$Paths = $UserPath -split ";"
if ($Paths -notcontains $InstallDir) {
    $NewPath = if ([string]::IsNullOrWhiteSpace($UserPath)) {
        $InstallDir
    } else {
        "$UserPath;$InstallDir"
    }
    [Environment]::SetEnvironmentVariable("Path", $NewPath, "User")
    Write-Host "  Added $InstallDir to user PATH"
    Write-Host "  Restart terminal to use $AppName"
}

Write-Host "  Installed to $(Join-Path $InstallDir "$AppName.exe")"

# Remove stale cargo-installed binary if present (avoids version conflicts)
$cargoBin = Join-Path $HOME ".cargo\bin\$AppName.exe"
if (Test-Path $cargoBin) {
    Remove-Item $cargoBin -Force
    Write-Host "  Removed stale $cargoBin"
}

# ──────────────────────────────────────────────
# Cleanup copied binary from project root
# ──────────────────────────────────────────────

Write-Host "`n→ Cleaning up..."
foreach ($t in $targets) {
    if (Test-Path ".\$($t.BinaryName)") {
        Remove-Item ".\$($t.BinaryName)" -Force
        Write-Host "  Removed .\$($t.BinaryName)"
    }
}

# ──────────────────────────────────────────────
# Done
# ──────────────────────────────────────────────

$remoteUrl = git remote get-url origin
$repoPath = $remoteUrl -replace '.*github.com[/:]', '' -replace '\.git$', ''
Write-Host "`n✅ Done! Released $($targets.Count) binaries → $tag"
Write-Host "   View at: https://github.com/$repoPath/releases/tag/$tag"
