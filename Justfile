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

test:
	@cargo test --workspace

doc:
	@cargo doc --workspace --no-deps

doc-open:
	@cargo doc --workspace --no-deps --open

run-daemon *args:
	@cargo run -p runloopd -- {{args}}

run-cli *args:
	@cargo run -p rlp -- {{args}}

run-monitor *args:
	@cargo run -p agtop -- {{args}}

all: fmt clippy test
