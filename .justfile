set shell := ["bash", "-euo", "pipefail", "-c"]

patch:
    cargo release patch --no-publish --execute

publish:
    cargo publish

ci:
    cargo fmt --all --check
    cargo check --all-features --locked
    cargo clippy --all-targets --all-features --locked -- -D warnings
    cargo test --all-features --locked
