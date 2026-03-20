set shell := ["bash", "-euo", "pipefail", "-c"]

ci:
    cargo fmt --all --check
    cargo check --all-features --locked
    cargo clippy --all-targets --all-features --locked -- -D warnings
    cargo test --all-features --locked

patch:
    cargo release patch --no-publish --execute
