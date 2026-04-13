#!/usr/bin/env bash
# Archon Deterministic Build Script
# Builds reproducible artifacts with fixed timestamps and environments

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
OUTPUT_DIR="${OUTPUT_DIR:-$PROJECT_ROOT/dist}"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

log_info() { echo -e "${GREEN}[INFO]${NC} $1"; }
log_warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
log_error() { echo -e "${RED}[ERROR]${NC} $1"; }

# Get SOURCE_DATE_EPOCH from git
get_source_date_epoch() {
    cd "$PROJECT_ROOT"
    git log -1 --format=%ct 2>/dev/null || date +%s
}

# Get version
get_version() {
    grep '^version' "$PROJECT_ROOT/Cargo.toml" | head -1 | sed 's/.*"\(.*\)".*/\1/'
}

# Set deterministic environment
setup_environment() {
    # Set SOURCE_DATE_EPOCH for reproducible builds
    export SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(get_source_date_epoch)}"
    log_info "SOURCE_DATE_EPOCH: $SOURCE_DATE_EPOCH ($(date -d @"$SOURCE_DATE_EPOCH" 2>/dev/null || date -r "$SOURCE_DATE_EPOCH"))"

    # Disable incremental compilation for reproducibility
    export CARGO_INCREMENTAL=0

    # Embed bitcode for determinism
    export RUSTFLAGS="${RUSTFLAGS:-} -C embed-bitcode=yes"

    # Use consistent locale
    export LC_ALL=C
    export LANG=C

    # Timezone
    export TZ=UTC
}

# Build release binary
build_release() {
    log_info "Building release binary..."

    cd "$PROJECT_ROOT"

    # Clean previous build
    cargo clean --release 2>/dev/null || true

    # Build with locked dependencies
    cargo build --release --locked

    log_info "Build complete!"
}

# Package artifacts
package_artifacts() {
    local version
    version=$(get_version)
    local target="${TARGET:-$(rustc -vV | grep 'host:' | cut -d' ' -f2)}"

    mkdir -p "$OUTPUT_DIR"

    log_info "Packaging artifacts for $target..."

    cd "$PROJECT_ROOT"

    # Create archive name
    local archive_name="archon-${version}-${target}"

    # Create staging directory
    local staging_dir="$OUTPUT_DIR/staging/$archive_name"
    rm -rf "$staging_dir"
    mkdir -p "$staging_dir"

    # Copy binaries
    local binaries=("archon" "archon-host" "ghostdns" "archon-settings")
    for bin in "${binaries[@]}"; do
        if [ -f "target/release/$bin" ]; then
            cp "target/release/$bin" "$staging_dir/"
            log_info "  Copied: $bin"
        fi
    done

    # Copy documentation
    cp README.md "$staging_dir/" 2>/dev/null || true
    cp LICENSE* "$staging_dir/" 2>/dev/null || true

    # Create tarball with deterministic timestamps
    cd "$OUTPUT_DIR/staging"
    tar --sort=name \
        --mtime="@$SOURCE_DATE_EPOCH" \
        --owner=0 --group=0 --numeric-owner \
        -czf "$OUTPUT_DIR/$archive_name.tar.gz" \
        "$archive_name"

    log_info "Created: $OUTPUT_DIR/$archive_name.tar.gz"

    # Generate checksum
    cd "$OUTPUT_DIR"
    sha256sum "$archive_name.tar.gz" > "$archive_name.tar.gz.sha256"

    # Cleanup staging
    rm -rf "$OUTPUT_DIR/staging"

    # Generate build metadata
    generate_build_metadata "$version" "$target"
}

# Generate build metadata
generate_build_metadata() {
    local version="$1"
    local target="$2"

    log_info "Generating build metadata..."

    local metadata_file="$OUTPUT_DIR/archon-${version}-build_meta.json"

    cat > "$metadata_file" << EOF
{
  "version": "$version",
  "target": "$target",
  "source_date_epoch": $SOURCE_DATE_EPOCH,
  "build_date": "$(date -u -d @"$SOURCE_DATE_EPOCH" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -r "$SOURCE_DATE_EPOCH" +%Y-%m-%dT%H:%M:%SZ)",
  "git_commit": "$(git rev-parse HEAD 2>/dev/null || echo 'unknown')",
  "git_branch": "$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo 'unknown')",
  "rustc_version": "$(rustc --version)",
  "cargo_version": "$(cargo --version)",
  "profile": "release",
  "features": [],
  "cargo_incremental": "$CARGO_INCREMENTAL",
  "rustflags": "$RUSTFLAGS"
}
EOF

    log_info "Created: $metadata_file"
}

# Verify build reproducibility
verify_build() {
    log_info "Verifying build reproducibility..."

    local first_hash second_hash

    # First build
    build_release
    first_hash=$(sha256sum "$PROJECT_ROOT/target/release/archon" | cut -d' ' -f1)
    log_info "First build hash: $first_hash"

    # Clean and rebuild
    cargo clean --release
    build_release
    second_hash=$(sha256sum "$PROJECT_ROOT/target/release/archon" | cut -d' ' -f1)
    log_info "Second build hash: $second_hash"

    if [ "$first_hash" = "$second_hash" ]; then
        log_info "Build is reproducible!"
        return 0
    else
        log_warn "Build is NOT reproducible. Hashes differ."
        return 1
    fi
}

# Display usage
usage() {
    cat << EOF
Archon Deterministic Build Script

Usage: $(basename "$0") [command] [options]

Commands:
    build               Build release binary with deterministic settings
    package             Build and package artifacts
    verify              Verify build reproducibility
    metadata            Generate build metadata only
    help                Show this help message

Environment Variables:
    SOURCE_DATE_EPOCH   Unix timestamp for build (default: git commit time)
    OUTPUT_DIR          Output directory (default: dist/)
    TARGET              Target triple (default: host)
    RUSTFLAGS           Additional Rust flags

Examples:
    $(basename "$0") build
    $(basename "$0") package
    SOURCE_DATE_EPOCH=1700000000 $(basename "$0") build
EOF
}

main() {
    setup_environment

    local command="${1:-help}"

    case "$command" in
        build)
            build_release
            ;;
        package)
            build_release
            package_artifacts
            ;;
        verify)
            verify_build
            ;;
        metadata)
            generate_build_metadata "$(get_version)" "$(rustc -vV | grep 'host:' | cut -d' ' -f2)"
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
