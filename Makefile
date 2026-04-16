.PHONY: test build clippy check-all

test:
	cargo test

build:
	cargo build --release && cp target/release/qedgen bin/qedgen

clippy:
	cargo clippy -- -D warnings

check-all: clippy test
