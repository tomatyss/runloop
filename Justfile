# Workspace automation for Runloop OS.

default := "all"

fmt:
	@cargo fmt --all

fmt-check:
	@cargo fmt --all -- --check

clippy:
	@cargo clippy --workspace -- -D warnings

check:
	@cargo check --workspace

build:
	@cargo build --workspace

build-release:
	@cargo build --workspace --release

deb:
	@packaging/systemd/build-deb.sh

test:
	@cargo test --workspace

pre-commit:
	@scripts/pre-commit.sh

markdownfmt:
	@npx prettier --write '**/*.md'

markdownlint: markdownfmt
	@npx markdownlint-cli2 '**/*.md'

doc:
	@cargo doc --workspace --no-deps

doc-open:
	@cargo doc --workspace --no-deps --open

docs-book:
	@mdbook build docs

docs-serve:
	@mdbook serve docs -n 127.0.0.1 -p 3000

run-daemon *args:
	@cargo run -p runloopd -- {{args}}

run-cli *args:
	@cargo run -p rlp -- {{args}}

run-monitor *args:
	@cargo run -p agtop -- {{args}}

all: fmt clippy test
