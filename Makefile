.PHONY: build test clippy clean

build:
	cargo build --release

test:
	cargo test

clippy:
	cargo clippy

clean:
	cargo clean
