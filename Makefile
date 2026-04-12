.PHONY: build test test-unit test-smoke lint fmt

TARGET_DIR ?= ./target/debug

build:
	cargo build --workspace

test-unit:
	cargo test --workspace

test-smoke: build
	TARGET_DIR=$(TARGET_DIR) ./tests/run.sh

test: test-unit test-smoke

lint:
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets --all-features -- -D warnings

fmt:
	cargo fmt --all
