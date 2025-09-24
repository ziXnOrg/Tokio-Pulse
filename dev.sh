#!/bin/bash
# Development helper script for Tokio-Pulse

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Project root
PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

function print_header() {
    echo -e "${GREEN}==== $1 ====${NC}"
}

function print_error() {
    echo -e "${RED}Error: $1${NC}"
}

function print_warning() {
    echo -e "${YELLOW}Warning: $1${NC}"
}

function build_all() {
    print_header "Building entire workspace"
    cargo build --workspace --all-features
}

function test_all() {
    print_header "Running all tests"
    cargo test --workspace --all-features
}

function test_pulse() {
    print_header "Testing tokio-pulse only"
    cargo test --package tokio-pulse --all-features
}

function bench() {
    print_header "Running benchmarks"
    cargo bench --package tokio-pulse
}

function check_quality() {
    print_header "Running quality checks"

    echo "Running clippy..."
    cargo clippy --workspace --all-features -- -D warnings || print_warning "Clippy found issues"

    echo "Checking formatting..."
    cargo fmt --all -- --check || print_warning "Code needs formatting"

    echo "Running tests..."
    cargo test --workspace --all-features
}

function clean() {
    print_header "Cleaning build artifacts"
    cargo clean
}

function docs() {
    print_header "Building documentation"
    cargo doc --workspace --all-features --no-deps --open
}

function tokio_diff() {
    print_header "Showing changes in tokio fork"
    cd "$PROJECT_ROOT/tokio-fork"
    git diff master..tokio-pulse-integration
    cd "$PROJECT_ROOT"
}

function help() {
    echo "Tokio-Pulse Development Script"
    echo ""
    echo "Usage: ./dev.sh [command]"
    echo ""
    echo "Commands:"
    echo "  build     - Build entire workspace"
    echo "  test      - Run all tests"
    echo "  test-pulse - Test tokio-pulse only"
    echo "  bench     - Run benchmarks"
    echo "  check     - Run quality checks (clippy, fmt, tests)"
    echo "  clean     - Clean build artifacts"
    echo "  docs      - Build and open documentation"
    echo "  diff      - Show changes in tokio fork"
    echo "  help      - Show this help message"
}

# Main script logic
case "${1:-help}" in
    build)
        build_all
        ;;
    test)
        test_all
        ;;
    test-pulse)
        test_pulse
        ;;
    bench)
        bench
        ;;
    check)
        check_quality
        ;;
    clean)
        clean
        ;;
    docs)
        docs
        ;;
    diff)
        tokio_diff
        ;;
    help|*)
        help
        ;;
esac