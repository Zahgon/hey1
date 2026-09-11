# Mirrors the Go repository's Makefile. Cross builds use `cross` when it is
# available (it supplies the linkers/sysroots that `cargo build --target`
# needs); otherwise install the targets with rustup and provide a linker.
binary = hey

BUILDER ?= $(shell command -v cross >/dev/null 2>&1 && echo cross || echo cargo)

release:
	$(BUILDER) build --release --target x86_64-pc-windows-gnu
	mkdir -p ./bin && cp target/x86_64-pc-windows-gnu/release/$(binary).exe ./bin/$(binary)_windows_amd64
	$(BUILDER) build --release --target x86_64-unknown-linux-gnu
	mkdir -p ./bin && cp target/x86_64-unknown-linux-gnu/release/$(binary) ./bin/$(binary)_linux_amd64
	$(BUILDER) build --release --target x86_64-apple-darwin
	mkdir -p ./bin && cp target/x86_64-apple-darwin/release/$(binary) ./bin/$(binary)_darwin_amd64

upload:
	gsutil -m cp -r ./bin/* gs://hey-releases/

release-upload: release upload

build:
	cargo build --release

test:
	cargo test

lint:
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings

.PHONY: release upload release-upload build test lint
