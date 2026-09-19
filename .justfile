set shell := ["bash", "-euo", "pipefail", "-c"]

patch:
  cargo release patch --no-publish --execute

publish:
  cargo publish

ci:
  cargo fmt --all --check
  cargo check --all-targets --all-features
  cargo rustc --lib --all-features -- -D missing-docs
  cargo clippy --all-targets --all-features -- -D warnings
  cargo test --doc --all-features
  cargo nextest run --all-targets --all-features
  cargo nextest run --all-targets --no-default-features
  cargo clippy --all-targets --no-default-features -- -D warnings
  cargo test --doc --no-default-features
  cargo doc --no-deps --all-features
  cargo publish --dry-run --allow-dirty

# Requires an installed nightly toolchain, as does docs.rs.
docsrs:
  RUSTDOCFLAGS='--cfg docsrs' cargo +nightly doc --all-features --no-deps

# Requires pwsh on PATH, or SHELLCOMP_PWSH pointing to its executable.
test-powershell:
  cargo test --all-features generated_profile_loads_literal_paths_without_changing_user_variables -- --ignored
