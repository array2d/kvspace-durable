.PHONY: build test clippy clean

PREFIX ?= ~/.local

build:
	cargo build --release --bin kvspace
	install -d $(PREFIX)/bin
	install target/release/kvspace $(PREFIX)/bin/kvspace

test: build
	python3 tutorial/test.py

clippy:
	cargo clippy

clean:
	cargo clean
