# MikMik installer for Windows (PowerShell).
#
# Usage (one-liner):
#   irm https://github.com/KilimcininKorOglu/mikmik/releases/latest/download/install.ps1 | iex
#
# Or download and run locally:
#   Invoke-WebRequest https://github.com/KilimcininKorOglu/mikmik/releases/latest/download/install.ps1 -OutFile install.ps1
#   .\install.ps1

[CmdletBinding()]
param(
    [string]$Version = "",
    [string]$Binary = "",
    [string]$InstallDir = "",
    [string]$Token = "",
    [switch]$NoModifyPath,
    [switch]$Help
)

$ErrorActionPreference = 'Stop'

$App = 'mikmik'
$Repo = 'KilimcininKorOglu/mikmik'

function Write-Info($msg)    { Write-Host $msg }
function Write-Success($msg) { Write-Host $msg -ForegroundColor Green }
function Write-Warn($msg)    { Write-Host $msg -ForegroundColor Yellow }
function Write-Err($msg)     { Write-Host $msg -ForegroundColor Red }
function Write-Muted($msg)   { Write-Host $msg -ForegroundColor DarkGray }

function Show-Usage {
@"
MikMik installer (Windows)

Usage: install.ps1 [options]

Options:
    -Help                   Show this help
    -Version <version>      Install a specific version (e.g., 0.1.0)
    -Binary <path>          Install from a local binary instead of downloading
    -InstallDir <path>      Override install location (default: %LOCALAPPDATA%\Programs\mikmik)
    -Token <token>          GitHub token for API and downloads
                            (or set GITHUB_TOKEN / GH_TOKEN)
    -NoModifyPath           Don't add the install dir to user PATH

Examples:
    irm https://github.com/KilimcininKorOglu/mikmik/releases/latest/download/install.ps1 | iex
    .\install.ps1 -Version 0.1.0
    .\install.ps1 -Binary C:\path\to\mikmik.exe
    `$env:GITHUB_TOKEN = 'ghp_...'; .\install.ps1
"@
}

if ($Help) { Show-Usage; exit 0 }

# ----- GitHub requests -----
# -Token wins; otherwise GITHUB_TOKEN, then GH_TOKEN (the gh CLI's variable).
if ([string]::IsNullOrEmpty($Token)) {
    if (-not [string]::IsNullOrEmpty($env:GITHUB_TOKEN)) {
        $Token = $env:GITHUB_TOKEN
    } elseif (-not [string]::IsNullOrEmpty($env:GH_TOKEN)) {
        $Token = $env:GH_TOKEN
    }
}

# Hosts the token may be sent to. A release download redirects to a storage
# host, and Windows PowerShell carries custom headers across a redirect, so the
# token would land somewhere it does not belong unless the header is decided
# per hop.
$script:GitHubHosts = @('github.com', 'api.github.com', 'www.github.com')

function Get-GitHubHeaders($uri) {
    $headers = @{
        'User-Agent' = 'mikmik-installer'
        'Accept'     = 'application/vnd.github+json'
    }
    if ([string]::IsNullOrEmpty($script:Token)) { return $headers }
    try {
        $host_name = ([Uri]$uri).Host
    } catch {
        return $headers
    }
    if ($script:GitHubHosts -contains $host_name.ToLower()) {
        $headers['Authorization'] = "Bearer $($script:Token)"
    }
    return $headers
}

# Download $Uri to $OutFile, following redirects by hand so each hop gets the
# headers its own host is allowed to see.
function Invoke-GitHubDownload($Uri, $OutFile) {
    $target = $Uri
    for ($hop = 0; $hop -lt 5; $hop++) {
        $headers = Get-GitHubHeaders $target
        $redirect = $null
        try {
            $oldPref = $ProgressPreference
            $ProgressPreference = 'SilentlyContinue'
            try {
                $resp = Invoke-WebRequest -UseBasicParsing -Uri $target -Headers $headers `
                    -MaximumRedirection 0 -OutFile $OutFile -ErrorAction Stop
            } finally {
                $ProgressPreference = $oldPref
            }
            # PowerShell 7 hands back a 3xx here instead of throwing.
            if ($null -ne $resp -and [int]$resp.StatusCode -ge 300 -and [int]$resp.StatusCode -lt 400) {
                $redirect = Get-RedirectLocation $resp
            } else {
                return
            }
        } catch {
            $response = $_.Exception.Response
            if ($null -eq $response) { throw }
            $status = [int]$response.StatusCode
            if ($status -lt 300 -or $status -ge 400) { throw }
            $redirect = Get-RedirectLocation $response
            if ([string]::IsNullOrEmpty($redirect)) { throw }
        }

        if ([string]::IsNullOrEmpty($redirect)) { return }
        $target = $redirect
    }
    throw "Too many redirects while downloading $Uri"
}

# The Location header is a string on Windows PowerShell and a Uri collection on
# PowerShell 7, so both shapes are read.
function Get-RedirectLocation($response) {
    try {
        $value = $response.Headers.Location
        if ($value) { return [string]$value }
    } catch { }
    try {
        $value = $response.Headers['Location']
        if ($value) { return [string]$value }
    } catch { }
    return $null
}

# ----- Detect architecture -----
function Get-Arch {
    $procArch = $env:PROCESSOR_ARCHITECTURE
    if ($null -eq $procArch) { $procArch = '' }
    switch ($procArch.ToLower()) {
        'amd64'   { return 'x86_64' }
        'x86'     { return 'x86_64' }   # rare; ship 64-bit anyway
        'arm64'   {
            Write-Warn "ARM64 Windows is not currently supported in releases. Falling back to x86_64."
            return 'x86_64'
        }
        default   {
            Write-Warn "Unknown architecture '$procArch'. Defaulting to x86_64."
            return 'x86_64'
        }
    }
}

# ----- Resolve install directory -----
# Binary location is independent of the mikmik data dir. Default to the
# per-user programs location (%LOCALAPPDATA%\Programs\mikmik), falling back to
# the user profile when LOCALAPPDATA is unavailable.
if ([string]::IsNullOrEmpty($InstallDir)) {
    if (-not [string]::IsNullOrEmpty($env:LOCALAPPDATA)) {
        $InstallDir = Join-Path $env:LOCALAPPDATA "Programs\mikmik"
    } else {
        $InstallDir = Join-Path $env:USERPROFILE ".local\bin"
    }
}
if (-not (Test-Path $InstallDir)) {
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
}

# ----- Resolve version (latest if not provided) -----
function Resolve-Version {
    if (-not [string]::IsNullOrEmpty($script:Version)) {
        return ($script:Version -replace '^v', '')
    }
    $apiUrl = "https://api.github.com/repos/$Repo/releases/latest"
    try {
        $resp = Invoke-RestMethod -UseBasicParsing -Uri $apiUrl -Headers (Get-GitHubHeaders $apiUrl)
        $tag = $resp.tag_name
        if ([string]::IsNullOrEmpty($tag)) { throw "no tag_name in response" }
        return ($tag -replace '^v', '')
    } catch {
        Write-Err "Failed to fetch the latest version from the GitHub API: $_"
        Write-Info "Check that $Repo has a published release:"
        Write-Info "  https://github.com/$Repo/releases"
        if ([string]::IsNullOrEmpty($script:Token)) {
            Write-Info "Unauthenticated requests are rate limited. Set GITHUB_TOKEN, GH_TOKEN, or pass -Token."
        } else {
            Write-Info "If the token was refused, check that it is valid and can read this repository."
        }
        Write-Info "You can also install a known version directly: -Version 0.1.0"
        exit 1
    }
}

# ----- Already-installed check -----
function Check-Existing($desiredVersion) {
    $existing = Get-Command mikmik -ErrorAction SilentlyContinue
    if ($null -eq $existing) { return }
    try {
        $vline = (& mikmik --version) 2>&1 | Select-Object -First 1
        $installed = ($vline -split '\s+')[-1]
    } catch {
        $installed = 'unknown'
    }
    if ($installed -eq $desiredVersion) {
        Write-Muted "Version $desiredVersion already installed at $($existing.Source)"
        Write-Muted "Use -Version to install a different one."
        exit 0
    }
    Write-Muted "Found existing mikmik at $($existing.Source) (v$installed) - upgrading to v$desiredVersion"
}

# ----- Download & extract -----
function Download-And-Install($desiredVersion, $arch) {
    $archive = "mikmik-windows-$arch.zip"
    $url = "https://github.com/$Repo/releases/download/v$desiredVersion/$archive"
    $tmpRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("mikmik-install-" + [System.Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmpRoot -Force | Out-Null

    $zipPath = Join-Path $tmpRoot $archive
    $extractDir = Join-Path $tmpRoot "extract"
    New-Item -ItemType Directory -Path $extractDir -Force | Out-Null

    Write-Info "Installing mikmik v$desiredVersion (windows-$arch)"
    Write-Muted "Downloading $url"
    if (-not [string]::IsNullOrEmpty($script:Token)) {
        Write-Muted "Using GitHub authentication."
    }
    try {
        Invoke-GitHubDownload $url $zipPath
    } catch {
        Write-Err "Download failed: $_"
        Write-Info ("Check that release v$desiredVersion exists for windows-" + $arch + ":")
        Write-Info "  https://github.com/$Repo/releases/tag/v$desiredVersion"
        if ([string]::IsNullOrEmpty($script:Token)) {
            Write-Info "A private release needs GITHUB_TOKEN, GH_TOKEN, or -Token."
        }
        Remove-Item -Recurse -Force $tmpRoot -ErrorAction SilentlyContinue
        exit 1
    }

    # ----- Verify checksum (supply-chain integrity) -----
    # Fetch SHA256SUMS from the same release and verify the archive before we
    # extract and run it.  Older releases may not ship SHA256SUMS: warn and
    # continue so existing installs keep working.  If it IS present and the
    # hash does NOT match, abort hard.
    $sumsUrl = "https://github.com/$Repo/releases/download/v$desiredVersion/SHA256SUMS"
    $sumsPath = Join-Path $tmpRoot 'SHA256SUMS'
    $haveSums = $false
    try {
        Invoke-GitHubDownload $sumsUrl $sumsPath
        $haveSums = $true
    } catch {
        Write-Warn "Could not fetch SHA256SUMS for v$desiredVersion - skipping checksum verification."
    }

    if ($haveSums) {
        # SHA256SUMS lines look like "<hash>  <filename>" (two spaces). Split on
        # whitespace into at most 2 parts and match on the bare archive name.
        $expected = $null
        foreach ($line in (Get-Content $sumsPath)) {
            $parts = $line.Trim() -split '\s+', 2
            if ($parts.Count -eq 2 -and $parts[1].Trim().TrimStart('*') -eq $archive) {
                $expected = $parts[0].Trim()
                break
            }
        }
        if ([string]::IsNullOrEmpty($expected)) {
            Write-Warn "No checksum listed for $archive in SHA256SUMS - skipping verification."
        } else {
            # Get-FileHash returns uppercase hex; sha256sum emits lowercase, so
            # compare case-insensitively.
            $actual = (Get-FileHash -Algorithm SHA256 -Path $zipPath).Hash
            if ($actual -ieq $expected) {
                Write-Muted "Checksum verified."
            } else {
                Write-Err "Checksum verification FAILED for $archive"
                Write-Info "  expected: $expected"
                Write-Info "  actual:   $actual"
                Write-Info "The download may be corrupted or tampered with. Aborting."
                Remove-Item -Recurse -Force $tmpRoot -ErrorAction SilentlyContinue
                exit 1
            }
        }
    }

    Write-Muted "Extracting..."
    try {
        Expand-Archive -Path $zipPath -DestinationPath $extractDir -Force
    } catch {
        Write-Err "Extract failed: $_"
        Remove-Item -Recurse -Force $tmpRoot -ErrorAction SilentlyContinue
        exit 1
    }

    $extractedExe = Join-Path $extractDir 'mikmik.exe'
    if (-not (Test-Path $extractedExe)) {
        Write-Err "Archive did not contain expected binary 'mikmik.exe'"
        Get-ChildItem -Recurse $extractDir | Format-Table FullName
        Remove-Item -Recurse -Force $tmpRoot -ErrorAction SilentlyContinue
        exit 1
    }

    Install-Binary $extractedExe
    Remove-Item -Recurse -Force $tmpRoot -ErrorAction SilentlyContinue
}

function Install-FromBinary {
    if (-not (Test-Path $script:Binary)) {
        Write-Err "Binary not found at $script:Binary"
        exit 1
    }
    Write-Info "Installing mikmik from $script:Binary"
    Install-Binary $script:Binary
}

function Install-Binary($source) {
    $target = Join-Path $InstallDir 'mikmik.exe'
    # Declared outside the block below so the cleanup after the copy can see it;
    # scoped inside, the renamed binary was left behind by every upgrade.
    $stale = "$target.old"

    # The currently running mikmik.exe (if any) holds an exclusive file lock on
    # Windows.  Swap by renaming the old one aside, then drop it once the new
    # binary is in place: the lock follows the open handle, not the name.
    if (Test-Path $target) {
        if (Test-Path $stale) { Remove-Item -Force $stale -ErrorAction SilentlyContinue }
        try { Move-Item -Force $target $stale } catch { }
    }

    Copy-Item -Force $source $target
    if (Test-Path $stale) {
        Remove-Item -Force $stale -ErrorAction SilentlyContinue
    }
    Write-Success "Installed: $target"
}

# ----- PATH modification -----
function Add-ToUserPath {
    if ($NoModifyPath) { return }

    $current = [Environment]::GetEnvironmentVariable('Path', 'User')
    if ($null -eq $current) { $current = '' }

    # Already on PATH?
    $paths = $current -split ';' | Where-Object { $_ -ne '' }
    foreach ($p in $paths) {
        if ($p.TrimEnd('\') -ieq $InstallDir.TrimEnd('\')) {
            Write-Muted "Install dir already on user PATH: $InstallDir"
            return
        }
    }

    if ([string]::IsNullOrEmpty($current)) {
        $newPath = $InstallDir
    } else {
        $newPath = $InstallDir + ';' + $current
    }
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')

    # Make it visible in this session too so mikmik --version works immediately.
    $env:Path = $InstallDir + ';' + $env:Path

    Write-Success ("Added " + $InstallDir + " to user PATH")
    Write-Muted "Open a new terminal for the change to take effect everywhere."
}

# ----- GitHub Actions hint -----
function GithubPathHint {
    if ($env:GITHUB_ACTIONS -eq 'true' -and -not [string]::IsNullOrEmpty($env:GITHUB_PATH)) {
        Add-Content -Path $env:GITHUB_PATH -Value $InstallDir
        Write-Info "Added $InstallDir to `$GITHUB_PATH"
    }
}

# ----- Main flow -----
if (-not [string]::IsNullOrEmpty($Binary)) {
    Install-FromBinary
} else {
    $arch = Get-Arch
    $desiredVersion = Resolve-Version
    Check-Existing $desiredVersion
    Download-And-Install $desiredVersion $arch
}

Add-ToUserPath
GithubPathHint

Write-Host ""
Write-Success "mikmik is installed!"
Write-Host ""
Write-Muted  "Quickstart:"
Write-Muted  "  # Set an API key"
Write-Host   "  `$env:ANTHROPIC_API_KEY = 'sk-ant-...'"
Write-Host   ""
Write-Muted  "  # Open a new terminal, then:"
Write-Success "  mikmik             "
Write-Muted  "  # or"
Write-Success "  mikmik -p `"...`"      "
Write-Host   ""
Write-Muted  "Docs: https://github.com/$Repo"
