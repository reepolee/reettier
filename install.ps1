# Install reettier from the latest GitHub Release.
# Usage: irm https://raw.githubusercontent.com/reepolee/reettier/main/install.ps1 | iex

$AppName = "reettier"
$Owner = "reepolee"
$Repo = "reettier"
$InstallDir = if ($env:INSTALL_DIR) { $env:INSTALL_DIR } else { Join-Path $HOME "bin" }

# ──────────────────────────────────────────────
# Detect platform
# ──────────────────────────────────────────────

$arch = $env:PROCESSOR_ARCHITECTURE
switch ($arch) {
    'AMD64' { $assetName = "$AppName-windows-x64.exe" }
    'ARM64' { $assetName = "$AppName-windows-arm64.exe" }
    default { Write-Error "Unsupported architecture: $arch"; exit 1 }
}

# ──────────────────────────────────────────────
# Download
# ──────────────────────────────────────────────

$downloadUrl = "https://github.com/$Owner/$Repo/releases/latest/download/$assetName"
$tmpPath = Join-Path $env:TEMP $assetName

Write-Host "→ Downloading $assetName ..."
try {
    Invoke-WebRequest -Uri $downloadUrl -OutFile $tmpPath -UseBasicParsing
} catch {
    Write-Error "Download failed: $_"
    exit 1
}

# ──────────────────────────────────────────────
# Install
# ──────────────────────────────────────────────

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
$target = Join-Path $InstallDir "$AppName.exe"
Copy-Item $tmpPath $target -Force
Remove-Item $tmpPath -Force

Write-Host "  Installed to $target"

# ──────────────────────────────────────────────
# PATH check
# ──────────────────────────────────────────────

$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
$paths = $userPath -split ";"

if ($paths -notcontains $InstallDir) {
    $newPath = if ([string]::IsNullOrWhiteSpace($userPath)) {
        $InstallDir
    } else {
        "$userPath;$InstallDir"
    }

    [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
    Write-Host "  Added $InstallDir to user PATH"
    Write-Host ""
    Write-Host "Restart your terminal to use $AppName"
}

Write-Host ""
Write-Host "✅ reettier installed!"
& $target --version
