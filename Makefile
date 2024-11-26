DIST_DIR := dist
GO_BIN := serve-go
RUST_BIN := serve-rs
PREFIX ?= /usr/local
BIN_DIR := $(PREFIX)/bin

.PHONY: all build go rust clean dist

all: build

build: dist go rust

dist:
	mkdir -p $(DIST_DIR)

go: dist
	cd serve-go && GOOS=$(GOOS) GOARCH=$(GOARCH) go build -o ../$(DIST_DIR)/$(GO_BIN)

rust: dist
	cargo build --manifest-path serve-rs/Cargo.toml --release
	install -m 755 serve-rs/target/release/$(RUST_BIN) $(DIST_DIR)/$(RUST_BIN)

install: build
	install -m 755 $(DIST_DIR)/$(GO_BIN) $(BIN_DIR)/$(GO_BIN)
	install -m 755 $(DIST_DIR)/$(RUST_BIN) $(BIN_DIR)/$(RUST_BIN)

clean:
	rm -rf $(DIST_DIR)
	cargo clean --manifest-path serve-rs/Cargo.toml
	cd serve-go && go clean
