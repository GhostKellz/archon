#!/usr/bin/env bash
# Archon Release Signing Script
# Signs release artifacts with GPG and optionally cosign

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
DIST_DIR="${DIST_DIR:-$PROJECT_ROOT/dist}"

# Default GPG key ID (set via environment or .env)
GPG_KEY_ID="${GPG_KEY_ID:-}"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

log_info() { echo -e "${GREEN}[INFO]${NC} $1"; }
log_warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
log_error() { echo -e "${RED}[ERROR]${NC} $1"; }

# Check for required tools
check_dependencies() {
    if ! command -v gpg &> /dev/null; then
        log_error "gpg not found. Please install GnuPG."
        exit 1
    fi

    if ! command -v sha256sum &> /dev/null; then
        log_error "sha256sum not found."
        exit 1
    fi
}

# Get version
get_version() {
    grep '^version' "$PROJECT_ROOT/Cargo.toml" | head -1 | sed 's/.*"\(.*\)".*/\1/'
}

# Generate checksums for all artifacts
generate_checksums() {
    log_info "Generating SHA256 checksums..."

    cd "$DIST_DIR"

    # Find all distributable files (excluding existing checksums and signatures)
    find . -maxdepth 1 -type f \
        ! -name '*.sha256' \
        ! -name '*.sig' \
        ! -name '*.asc' \
        ! -name 'SHA256SUMS*' \
        -print0 | while IFS= read -r -d '' file; do
        local basename
        basename=$(basename "$file")
        sha256sum "$basename" > "$basename.sha256"
        log_info "  Created: $basename.sha256"
    done

    # Create combined checksums file
    sha256sum ./*.tar.* ./*.zip 2>/dev/null > SHA256SUMS || true
    log_info "Created: SHA256SUMS"
}

# Sign file with GPG
gpg_sign_file() {
    local file="$1"
    local key_id="${2:-$GPG_KEY_ID}"

    if [ -z "$key_id" ]; then
        log_error "GPG_KEY_ID not set. Use: GPG_KEY_ID=<keyid> $0 sign"
        exit 1
    fi

    log_info "Signing: $file"

    # Create detached signature
    gpg --armor --detach-sign --local-user "$key_id" "$file"
    log_info "  Created: $file.asc"
}

# Sign all release artifacts
sign_artifacts() {
    local key_id="${1:-$GPG_KEY_ID}"

    if [ -z "$key_id" ]; then
        log_error "GPG_KEY_ID not set."
        log_info "Usage: GPG_KEY_ID=<keyid> $0 sign"
        log_info ""
        log_info "Available GPG keys:"
        gpg --list-secret-keys --keyid-format SHORT 2>/dev/null || true
        exit 1
    fi

    log_info "Signing artifacts with key: $key_id"

    cd "$DIST_DIR"

    # Sign main artifacts
    for file in *.tar.* *.zip SHA256SUMS; do
        if [ -f "$file" ]; then
            gpg_sign_file "$file" "$key_id"
        fi
    done

    # Sign SBOM files
    for file in *-sbom.json *-sbom.xml; do
        if [ -f "$file" ]; then
            gpg_sign_file "$file" "$key_id"
        fi
    done

    log_info "Signing complete!"
}

# Verify signatures
verify_signatures() {
    log_info "Verifying GPG signatures..."

    cd "$DIST_DIR"

    local failed=0
    for sig_file in *.asc; do
        if [ -f "$sig_file" ]; then
            local file="${sig_file%.asc}"
            if gpg --verify "$sig_file" "$file" 2>/dev/null; then
                log_info "  OK: $file"
            else
                log_error "  FAILED: $file"
                failed=$((failed + 1))
            fi
        fi
    done

    if [ $failed -gt 0 ]; then
        log_error "$failed signature(s) failed verification"
        exit 1
    fi

    log_info "All signatures verified!"
}

# Cosign for container images (optional)
cosign_sign() {
    local image="$1"

    if ! command -v cosign &> /dev/null; then
        log_warn "cosign not installed. Skipping container signing."
        return
    fi

    log_info "Signing container image: $image"
    cosign sign "$image"
}

# Display usage
usage() {
    cat << EOF
Archon Release Signing Script

Usage: $(basename "$0") [command] [options]

Commands:
    checksums           Generate SHA256 checksums for all artifacts
    sign [key_id]       Sign artifacts with GPG (uses GPG_KEY_ID env var)
    verify              Verify GPG signatures
    cosign <image>      Sign container image with cosign
    all [key_id]        Run checksums + sign
    help                Show this help message

Environment Variables:
    GPG_KEY_ID          GPG key ID to use for signing
    DIST_DIR            Directory containing artifacts (default: dist/)

Examples:
    $(basename "$0") checksums
    GPG_KEY_ID=ABCD1234 $(basename "$0") sign
    $(basename "$0") verify
    $(basename "$0") cosign ghcr.io/ghostkellz/archon:latest
EOF
}

main() {
    check_dependencies

    local command="${1:-help}"

    case "$command" in
        checksums)
            generate_checksums
            ;;
        sign)
            sign_artifacts "${2:-}"
            ;;
        verify)
            verify_signatures
            ;;
        cosign)
            cosign_sign "${2:-}"
            ;;
        all)
            generate_checksums
            sign_artifacts "${2:-}"
            ;;
        help|--help|-h)
            usage
            ;;
        *)
            log_error "Unknown command: $command"
            usage
            exit 1
            ;;
    esac
}

main "$@"
