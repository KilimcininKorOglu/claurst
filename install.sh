#!/usr/bin/env bash
# Claurst installer for Linux, macOS, and Windows (Git Bash / MSYS / Cygwin).
#
# Usage (one-liner):
#   curl -fsSL https://github.com/KilimcininKorOglu/claurst/releases/latest/download/install.sh | bash
#
# Or download and run locally:
#   curl -fsSL -O https://github.com/KilimcininKorOglu/claurst/releases/latest/download/install.sh
#   chmod +x install.sh
#   ./install.sh

set -euo pipefail

APP=claurst
REPO=KilimcininKorOglu/claurst

# ANSI colours
MUTED='\033[0;2m'
RED='\033[0;31m'
GREEN='\033[0;32m'
ORANGE='\033[38;5;214m'
NC='\033[0m'

usage() {
    cat <<EOF
Claurst installer

Usage: install.sh [options]

Options:
    -h, --help              Display this help message
    -v, --version <version> Install a specific version (e.g., 0.1.0 or v0.1.0)
    -b, --binary <path>     Install from a local binary instead of downloading
        --token <token>     GitHub token for API and downloads
                            (or set GITHUB_TOKEN / GH_TOKEN)
        --no-modify-path    Don't modify shell config files (.zshrc, .bashrc, etc.)
        --install-dir <dir> Override install location
                            (default: ${XDG_BIN_HOME:-~/.local/bin}; on Windows:
                            %LOCALAPPDATA%\\Programs\\claurst)

Examples:
    curl -fsSL https://github.com/KilimcininKorOglu/claurst/releases/latest/download/install.sh | bash
    ./install.sh --version 0.1.0
    ./install.sh --binary /path/to/claurst
    GITHUB_TOKEN=ghp_... ./install.sh
EOF
}

print_message() {
    local level=$1
    local message=$2
    local color=""
    case "$level" in
        info)    color="$NC" ;;
        success) color="$GREEN" ;;
        warning) color="$ORANGE" ;;
        error)   color="$RED" ;;
        *)       color="$NC" ;;
    esac
    printf "${color}%b${NC}\n" "$message"
}

# print_message renders through printf %b, which turns a backslash escape into
# the character it names. A Windows path is mostly backslashes, so double them
# before one reaches a message: C:\tmp would otherwise print as C:<tab>mp.
escape_backslashes() {
    echo "${1//\\/\\\\}"
}

requested_version=${VERSION:-}
no_modify_path=false
binary_path=""
install_dir_override=""
# --token wins; otherwise GITHUB_TOKEN, then GH_TOKEN (the gh CLI's variable).
github_token="${GITHUB_TOKEN:-${GH_TOKEN:-}}"

while [[ $# -gt 0 ]]; do
    case "$1" in
        -h|--help)
            usage
            exit 0
            ;;
        -v|--version)
            if [[ -n "${2:-}" ]]; then
                requested_version="$2"
                shift 2
            else
                print_message error "Error: --version requires a version argument"
                exit 1
            fi
            ;;
        -b|--binary)
            if [[ -n "${2:-}" ]]; then
                binary_path="$2"
                shift 2
            else
                print_message error "Error: --binary requires a path argument"
                exit 1
            fi
            ;;
        --token)
            if [[ -n "${2:-}" ]]; then
                github_token="$2"
                shift 2
            else
                print_message error "Error: --token requires a token argument"
                exit 1
            fi
            ;;
        --no-modify-path)
            no_modify_path=true
            shift
            ;;
        --install-dir)
            if [[ -n "${2:-}" ]]; then
                install_dir_override="$2"
                shift 2
            else
                print_message error "Error: --install-dir requires a path argument"
                exit 1
            fi
            ;;
        *)
            print_message warning "Warning: Unknown option '$1'"
            shift
            ;;
    esac
done

# ----- GitHub requests -----
# curl reads the Authorization header from a config on stdin rather than from
# an argument, because a command line is readable by every user on the machine
# through /proc/<pid>/cmdline. curl itself drops the header when GitHub
# redirects an asset download to its storage host, so the token only ever
# reaches GitHub.
github_curl() {
    if [[ -n "$github_token" ]]; then
        printf 'header = "Authorization: Bearer %s"\n' "$github_token" | curl -K - "$@"
    else
        curl "$@"
    fi
}

# True under Git Bash / MSYS / Cygwin on Windows.
is_windows_shell() {
    case "$(uname -s 2>/dev/null || echo unknown)" in
        MINGW*|MSYS*|CYGWIN*) return 0 ;;
        *) return 1 ;;
    esac
}

# Turn a Windows path (C:\foo or C:/foo) into the Unix form this shell uses.
to_unix_path() {
    local win_path="$1"
    if command -v cygpath >/dev/null 2>&1; then
        cygpath -u "$win_path"
    else
        # MSYS accepts a mixed path once the separators are forward slashes.
        echo "${win_path//\\//}"
    fi
}

# Windows installs land where install.ps1 puts them, so the two installers
# agree on one location and one PATH entry.
default_install_dir() {
    if is_windows_shell; then
        if [[ -n "${LOCALAPPDATA:-}" ]]; then
            echo "$(to_unix_path "$LOCALAPPDATA")/Programs/claurst"
        elif [[ -n "${USERPROFILE:-}" ]]; then
            echo "$(to_unix_path "$USERPROFILE")/.local/bin"
        else
            echo "$HOME/.local/bin"
        fi
    else
        echo "${XDG_BIN_HOME:-$HOME/.local/bin}"
    fi
}

INSTALL_DIR="${install_dir_override:-$(default_install_dir)}"
mkdir -p "$INSTALL_DIR"

# ----- Detect platform & arch -----
detect_target() {
    local raw_os arch
    raw_os=$(uname -s)
    case "$raw_os" in
        Darwin*)              os="macos" ;;
        Linux*)               os="linux" ;;
        MINGW*|MSYS*|CYGWIN*) os="windows" ;;
        *)
            print_message error "Unsupported OS: $raw_os"
            exit 1
            ;;
    esac

    arch=$(uname -m)
    case "$arch" in
        x86_64|amd64) arch="x86_64" ;;
        aarch64|arm64) arch="aarch64" ;;
        *)
            print_message error "Unsupported architecture: $arch"
            exit 1
            ;;
    esac

    # Apple Silicon under Rosetta: prefer the native arm64 binary.
    if [ "$os" = "macos" ] && [ "$arch" = "x86_64" ]; then
        rosetta_flag=$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)
        if [ "$rosetta_flag" = "1" ]; then
            arch="aarch64"
        fi
    fi

    # Releases carry one Windows build, same as install.ps1 assumes.
    if [ "$os" = "windows" ] && [ "$arch" != "x86_64" ]; then
        print_message warning "Windows ${arch} is not currently supported in releases. Falling back to x86_64."
        arch="x86_64"
    fi

    target="${os}-${arch}"
    if [ "$os" = "windows" ]; then
        archive_ext="zip"
        binary_name="${APP}.exe"
    else
        archive_ext="tar.gz"
        binary_name="$APP"
    fi
}

# ----- Pre-flight: required tools -----
check_required_tools() {
    local missing=()
    # A local --binary install downloads and extracts nothing.
    if [[ -z "$binary_path" ]]; then
        command -v curl >/dev/null 2>&1 || missing+=("curl")
        if is_windows_shell; then
            # Git for Windows ships bsdtar, which opens a zip, but not unzip.
            if ! command -v unzip >/dev/null 2>&1 && ! command -v tar >/dev/null 2>&1; then
                missing+=("unzip or tar")
            fi
        else
            command -v tar >/dev/null 2>&1 || missing+=("tar")
        fi
    fi
    if [ "${#missing[@]}" -gt 0 ]; then
        print_message error "Missing required tools: ${missing[*]}"
        print_message info "Please install them and try again."
        exit 1
    fi
}

resolve_version() {
    if [[ -n "$requested_version" ]]; then
        # Strip leading 'v' if present
        requested_version="${requested_version#v}"
        specific_version="$requested_version"
    else
        # Fetch latest version from GitHub API.  Use sed instead of jq for
        # portability.  The call is guarded rather than assigned directly,
        # because `set -e` kills the script on a failed assignment and the
        # explanation below would never print.
        local api_response=""
        if ! api_response=$(github_curl -fsSL \
            -H "User-Agent: claurst-installer" \
            -H "Accept: application/vnd.github+json" \
            "https://api.github.com/repos/${REPO}/releases/latest"); then
            api_response=""
        fi

        specific_version=$(printf '%s' "$api_response" \
            | sed -n 's/.*"tag_name": *"v\{0,1\}\([^"]*\)".*/\1/p' \
            | head -n 1)
        if [[ -z "$specific_version" ]]; then
            print_message error "Failed to fetch the latest version from the GitHub API."
            print_message info "Check that ${REPO} has a published release:"
            print_message info "  https://github.com/${REPO}/releases"
            if [[ -z "$github_token" ]]; then
                print_message info "Unauthenticated requests are rate limited. Set GITHUB_TOKEN, GH_TOKEN, or pass --token."
            else
                print_message info "If the token was refused, check that it is valid and can read this repository."
            fi
            print_message info "You can also install a known version directly: --version 0.1.0"
            exit 1
        fi
    fi
}

# ----- Already-installed check -----
check_existing_install() {
    if command -v "$APP" >/dev/null 2>&1; then
        existing_path=$(command -v "$APP")
        installed_version=$("$APP" --version 2>/dev/null | awk '{print $NF}' || echo "unknown")
        if [[ "$installed_version" == "$specific_version" ]]; then
            print_message info "${MUTED}Version ${NC}$specific_version${MUTED} already installed at ${NC}$existing_path"
            print_message info "${MUTED}Use --version to install a different version, or pass a different one to upgrade.${NC}"
            exit 0
        fi
        print_message info "${MUTED}Found existing claurst at ${NC}$existing_path${MUTED} (v$installed_version) - upgrading to v$specific_version${NC}"
    fi
}

# ----- Download & extract -----
download_and_install() {
    local archive="${APP}-${target}.${archive_ext}"
    local url="https://github.com/${REPO}/releases/download/v${specific_version}/${archive}"
    local tmp_dir
    # -t is GNU-style; Git Bash and BSD layouts need the explicit template.
    tmp_dir=$(mktemp -d -t claurst-install-XXXXXX 2>/dev/null \
        || mktemp -d "${TMPDIR:-/tmp}/claurst-install.XXXXXX")
    trap "rm -rf '$tmp_dir'" EXIT

    print_message info "${MUTED}Installing ${NC}${APP} ${MUTED}v${NC}${specific_version} ${MUTED}(${target})${NC}"
    print_message info "${MUTED}Downloading ${NC}${url}"
    if [[ -n "$github_token" ]]; then
        print_message info "${MUTED}Using GitHub authentication.${NC}"
    fi

    if ! github_curl -fL --progress-bar \
        -H "User-Agent: claurst-installer" \
        -o "$tmp_dir/$archive" "$url"; then
        print_message error "Download failed."
        print_message info "Check that release v${specific_version} exists for ${target}:"
        print_message info "  https://github.com/${REPO}/releases/tag/v${specific_version}"
        if [[ -z "$github_token" ]]; then
            print_message info "A private release needs GITHUB_TOKEN, GH_TOKEN, or --token."
        fi
        exit 1
    fi

    # ----- Verify checksum (supply-chain integrity) -----
    # Fetch SHA256SUMS from the same release and verify the archive before we
    # extract and run it.  Older releases may not ship SHA256SUMS — in that
    # case we warn and continue so existing installs keep working.  But if the
    # file IS present and the hash does NOT match, we abort hard.
    local sums_url="https://github.com/${REPO}/releases/download/v${specific_version}/SHA256SUMS"
    if github_curl -fsSL -H "User-Agent: claurst-installer" \
        -o "$tmp_dir/SHA256SUMS" "$sums_url" 2>/dev/null; then
        # sha256sum emits "<hash>  <filename>" (two spaces); awk collapses the
        # whitespace so $1=hash, $2=bare filename.  Match on the bare archive
        # name (not the full temp path).
        local expected actual
        expected=$(awk -v f="$archive" '$2 == f {print $1}' "$tmp_dir/SHA256SUMS" | head -n 1)
        if [[ -z "$expected" ]]; then
            print_message warning "Warning: no checksum listed for ${archive} in SHA256SUMS - skipping verification."
        else
            if command -v sha256sum >/dev/null 2>&1; then
                actual=$(sha256sum "$tmp_dir/$archive" | awk '{print $1}')
            elif command -v shasum >/dev/null 2>&1; then
                actual=$(shasum -a 256 "$tmp_dir/$archive" | awk '{print $1}')
            else
                actual=""
            fi
            if [[ -z "$actual" ]]; then
                print_message warning "Warning: no sha256sum/shasum tool available - skipping checksum verification."
            elif [[ "$actual" != "$expected" ]]; then
                print_message error "Checksum verification FAILED for ${archive}"
                print_message info "  expected: $expected"
                print_message info "  actual:   $actual"
                print_message info "The download may be corrupted or tampered with. Aborting."
                exit 1
            else
                print_message info "${MUTED}Checksum verified.${NC}"
            fi
        fi
    else
        print_message warning "Warning: could not fetch SHA256SUMS for v${specific_version} - skipping checksum verification."
    fi

    print_message info "${MUTED}Extracting...${NC}"
    if [[ "$archive_ext" == "zip" ]]; then
        # Git for Windows ships no unzip, so fall back to bsdtar, which reads
        # a zip as happily as a tarball.
        if command -v unzip >/dev/null 2>&1; then
            if ! unzip -o -q "$tmp_dir/$archive" -d "$tmp_dir"; then
                print_message error "Extract failed."
                exit 1
            fi
        elif ! tar -xf "$tmp_dir/$archive" -C "$tmp_dir"; then
            print_message error "Extract failed."
            exit 1
        fi
    else
        tar -xzf "$tmp_dir/$archive" -C "$tmp_dir"
    fi

    if [[ ! -f "$tmp_dir/$binary_name" ]]; then
        print_message error "Archive did not contain expected binary '$binary_name'"
        print_message info "Contents:"
        ls -la "$tmp_dir"
        exit 1
    fi

    install_binary "$tmp_dir/$binary_name"
}

install_from_binary() {
    if [[ ! -f "$binary_path" ]]; then
        print_message error "Binary not found at $binary_path"
        exit 1
    fi
    print_message info "${MUTED}Installing ${NC}${APP} ${MUTED}from ${NC}$binary_path"
    install_binary "$binary_path"
}

install_binary() {
    local source="$1"
    # detect_target sets binary_name; a local --binary install skips it.
    local dest_name="${binary_name:-$APP}"
    if is_windows_shell && [[ "$dest_name" != *.exe ]]; then
        dest_name="${dest_name}.exe"
    fi
    local target_path="${INSTALL_DIR}/${dest_name}"
    local stale="${target_path}.old"

    # Windows keeps an exclusive lock on a running .exe, so the old binary is
    # renamed aside first: the lock follows the open handle, not the name.
    # Removing it afterwards is what install.ps1 does too.
    if is_windows_shell && [[ -f "$target_path" ]]; then
        rm -f "$stale" 2>/dev/null || true
        mv -f "$target_path" "$stale" 2>/dev/null || true
    fi

    cp "$source" "$target_path"
    chmod 755 "$target_path"

    if [[ -f "$stale" ]]; then
        rm -f "$stale" 2>/dev/null || true
    fi

    # On macOS, strip the quarantine attribute so Gatekeeper doesn't block the unsigned binary.
    if [[ "$(uname -s)" == "Darwin" ]]; then
        xattr -dr com.apple.quarantine "$target_path" 2>/dev/null || true
    fi

    print_message success "Installed: $target_path"
}

# ----- PATH modification -----
# On Windows the shell config is the wrong place: PowerShell and cmd never read
# it. install.ps1 writes the user-scoped Path variable, so this does the same
# through powershell.exe and the two installers stay interchangeable.
add_to_windows_user_path() {
    local win_dir
    if command -v cygpath >/dev/null 2>&1; then
        win_dir=$(cygpath -w "$INSTALL_DIR")
    elif [[ "$INSTALL_DIR" =~ ^/([a-zA-Z])/(.*) ]]; then
        win_dir="${BASH_REMATCH[1]^}:\\${BASH_REMATCH[2]//\//\\}"
    else
        win_dir="${INSTALL_DIR//\//\\}"
    fi

    if ! command -v powershell.exe >/dev/null 2>&1; then
        print_message warning "powershell.exe not found. Add this to your PATH manually: $(escape_backslashes "$win_dir")"
        return
    fi

    # The directory travels in the environment rather than in the script text,
    # so a path holding a quote or a backslash cannot break the command.
    local ps_script
    ps_script=$(cat <<'PSEOF'
$ErrorActionPreference = 'Stop'
$installDir = $env:CLAURST_INSTALL_DIR_WIN
$current = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($null -eq $current) { $current = '' }
foreach ($entry in ($current -split ';' | Where-Object { $_ -ne '' })) {
    if ($entry.TrimEnd('\') -ieq $installDir.TrimEnd('\')) {
        Write-Output 'already'
        exit 0
    }
}
if ([string]::IsNullOrEmpty($current)) {
    $newPath = $installDir
} else {
    $newPath = $installDir + ';' + $current
}
[Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
Write-Output 'added'
PSEOF
)

    local result
    if ! result=$(CLAURST_INSTALL_DIR_WIN="$win_dir" \
        powershell.exe -NoProfile -NonInteractive -Command "$ps_script" 2>/dev/null); then
        print_message warning "Could not update the Windows user PATH. Add it manually: $(escape_backslashes "$win_dir")"
        return
    fi

    # Usable in this shell right away; a new terminal picks up the stored value.
    export PATH="$INSTALL_DIR:$PATH"

    result=$(echo "$result" | tr -d '\r' | tail -n 1)
    if [[ "$result" == "already" ]]; then
        print_message info "${MUTED}Install dir already on user PATH: ${NC}$(escape_backslashes "$win_dir")"
    else
        print_message success "Added $(escape_backslashes "$win_dir") to user PATH"
        print_message info "${MUTED}Open a new terminal for the change to take effect everywhere.${NC}"
    fi
}

add_to_unix_path() {
    if [[ ":$PATH:" == *":$INSTALL_DIR:"* ]]; then
        return  # already on PATH for this session
    fi

    XDG_CONFIG_HOME=${XDG_CONFIG_HOME:-$HOME/.config}
    local current_shell config_files=""
    current_shell=$(basename "${SHELL:-/bin/bash}")

    case "$current_shell" in
        fish)
            config_files="$HOME/.config/fish/config.fish"
            ;;
        zsh)
            config_files="${ZDOTDIR:-$HOME}/.zshrc ${ZDOTDIR:-$HOME}/.zshenv $XDG_CONFIG_HOME/zsh/.zshrc"
            ;;
        bash)
            config_files="$HOME/.bashrc $HOME/.bash_profile $HOME/.profile"
            ;;
        ash|sh)
            config_files="$HOME/.profile /etc/profile"
            ;;
        *)
            config_files="$HOME/.bashrc $HOME/.bash_profile $HOME/.profile"
            ;;
    esac

    local config_file=""
    for file in $config_files; do
        if [[ -f "$file" ]]; then
            config_file="$file"
            break
        fi
    done

    # Fall back to creating ~/.profile if nothing exists.
    if [[ -z "$config_file" ]]; then
        config_file="$HOME/.profile"
        touch "$config_file"
    fi

    local path_line
    if [[ "$current_shell" == "fish" ]]; then
        path_line="fish_add_path $INSTALL_DIR"
    else
        path_line="export PATH=\"$INSTALL_DIR:\$PATH\""
    fi

    if grep -Fq "$INSTALL_DIR" "$config_file" 2>/dev/null; then
        print_message info "${MUTED}PATH entry already in ${NC}$config_file"
        return
    fi

    {
        echo ""
        echo "# claurst"
        echo "$path_line"
    } >> "$config_file"
    print_message success "Added $INSTALL_DIR to PATH in $config_file"
    print_message info "${MUTED}Restart your shell or run:${NC} source $config_file"
}

add_to_path_if_needed() {
    if [[ "$no_modify_path" == "true" ]]; then
        return
    fi

    if is_windows_shell; then
        add_to_windows_user_path
    else
        add_to_unix_path
    fi
}

# ----- GitHub Actions environment hint -----
github_path_hint() {
    if [ -n "${GITHUB_ACTIONS-}" ] && [ "${GITHUB_ACTIONS}" = "true" ] && [ -n "${GITHUB_PATH-}" ]; then
        echo "$INSTALL_DIR" >> "$GITHUB_PATH"
        print_message info "Added $INSTALL_DIR to \$GITHUB_PATH"
    fi
}

# ----- Main flow -----
main() {
    check_required_tools

    if [[ -n "$binary_path" ]]; then
        # Local binary install - skip detection & version resolution.
        specific_version="local"
        install_from_binary
    else
        detect_target
        resolve_version
        check_existing_install
        download_and_install
    fi

    add_to_path_if_needed
    github_path_hint

    # Goodbye banner
    echo ""
    print_message success "claurst is installed!"
    echo ""
    echo -e "${MUTED}Quickstart:${NC}"
    echo -e "  ${MUTED}# Set an API key${NC}"
    echo -e "  export ANTHROPIC_API_KEY=sk-ant-..."
    echo -e ""
    echo -e "  ${MUTED}# Open a new terminal, then:${NC}"
    echo -e "  ${GREEN}claurst${NC}              ${MUTED}# Interactive TUI${NC}"
    echo -e "  ${GREEN}claurst -p \"...\"${NC}       ${MUTED}# Headless one-shot${NC}"
    echo ""
    echo -e "${MUTED}Docs: ${NC}https://github.com/${REPO}"
}

main "$@"
