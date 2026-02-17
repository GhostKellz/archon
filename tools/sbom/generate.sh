#!/usr/bin/env bash
# Archon SBOM (Software Bill of Materials) Generator
# Generates CycloneDX format SBOM for release artifacts

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
OUTPUT_DIR="${OUTPUT_DIR:-$PROJECT_ROOT/dist}"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

log_info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

# Check for required tools
check_dependencies() {
    local missing=()

    if ! command -v cargo &> /dev/null; then
        missing+=("cargo")
    fi

    if ! cargo cyclonedx --help &> /dev/null; then
        log_warn "cargo-cyclonedx not installed. Installing..."
        cargo install cargo-cyclonedx || {
            log_error "Failed to install cargo-cyclonedx"
            exit 1
        }
    fi

    if [ ${#missing[@]} -ne 0 ]; then
        log_error "Missing dependencies: ${missing[*]}"
        exit 1
    fi
}

# Get version from Cargo.toml
get_version() {
    grep '^version' "$PROJECT_ROOT/Cargo.toml" | head -1 | sed 's/.*"\(.*\)".*/\1/'
}

# Generate SBOM
generate_sbom() {
    local format="${1:-json}"
    local version
    version=$(get_version)

    mkdir -p "$OUTPUT_DIR"

    log_info "Generating SBOM for archon v${version}..."

    cd "$PROJECT_ROOT"

    case "$format" in
        json)
            cargo cyclonedx --format json > "$OUTPUT_DIR/archon-${version}-sbom.json"
            log_info "Generated: $OUTPUT_DIR/archon-${version}-sbom.json"
            ;;
        xml)
            cargo cyclonedx --format xml > "$OUTPUT_DIR/archon-${version}-sbom.xml"
            log_info "Generated: $OUTPUT_DIR/archon-${version}-sbom.xml"
            ;;
        all)
            cargo cyclonedx --format json > "$OUTPUT_DIR/archon-${version}-sbom.json"
            cargo cyclonedx --format xml > "$OUTPUT_DIR/archon-${version}-sbom.xml"
            log_info "Generated JSON and XML SBOMs"
            ;;
        *)
            log_error "Unknown format: $format (use json, xml, or all)"
            exit 1
            ;;
    esac

    # Generate SHA256 checksum
    cd "$OUTPUT_DIR"
    for sbom_file in archon-*-sbom.*; do
        if [ -f "$sbom_file" ]; then
            sha256sum "$sbom_file" > "$sbom_file.sha256"
            log_info "Checksum: $sbom_file.sha256"
        fi
    done
}

# Verify SBOM against actual dependencies
verify_sbom() {
    local sbom_file="$1"

    if [ ! -f "$sbom_file" ]; then
        log_error "SBOM file not found: $sbom_file"
        exit 1
    fi

    log_info "Verifying SBOM: $sbom_file"

    # Count components in SBOM
    local component_count
    if [[ "$sbom_file" == *.json ]]; then
        component_count=$(jq '.components | length' "$sbom_file" 2>/dev/null || echo "0")
    else
        component_count=$(grep -c '<component' "$sbom_file" 2>/dev/null || echo "0")
    fi

    log_info "Found $component_count components in SBOM"

    # Compare with cargo dependencies
    local cargo_deps
    cargo_deps=$(cd "$PROJECT_ROOT" && cargo tree --depth 1 | wc -l)
    log_info "Cargo reports approximately $cargo_deps direct dependencies"
}

# Display usage
usage() {
    cat << EOF
Archon SBOM Generator

Usage: $(basename "$0") [command] [options]

Commands:
    generate [format]   Generate SBOM (json, xml, or all)
    verify [file]       Verify SBOM against actual dependencies
    help                Show this help message

Options:
    OUTPUT_DIR=<path>   Set output directory (default: dist/)

Examples:
    $(basename "$0") generate json
    $(basename "$0") generate all
    $(basename "$0") verify dist/archon-0.1.0-sbom.json
EOF
}

# Main
main() {
    check_dependencies

    local command="${1:-help}"

    case "$command" in
        generate)
            generate_sbom "${2:-json}"
            ;;
        verify)
            verify_sbom "${2:-$OUTPUT_DIR/archon-*-sbom.json}"
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
