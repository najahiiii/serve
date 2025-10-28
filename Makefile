DIST_DIR := dist
GO_BIN := serve-go
RUST_BIN := serve-rs
CLI_BIN := serve-cli
PREFIX ?= /usr/local
BIN_DIR := $(PREFIX)/bin
VERSION := $(shell sed -n 's/^version[[:space:]]*=[[:space:]]*"\(.*\)"/\1/p' serve-rs/Cargo.toml | head -n 1)

.PHONY: all build go rust clean dist go-debug release-tar

all: build

build: dist go rust cli

dist:
	mkdir -p $(DIST_DIR)

go: dist
	cd serve-go && GOOS=$(GOOS) GOARCH=$(GOARCH) go build -ldflags "-s -w" -o ../$(DIST_DIR)/$(GO_BIN)

go-debug: dist
	cd serve-go && GOOS=$(GOOS) GOARCH=$(GOARCH) go build -gcflags="all=-N -l" -o ../$(DIST_DIR)/$(GO_BIN)

rust: dist
	cargo build --manifest-path serve-rs/Cargo.toml --release
	install -m 755 serve-rs/target/release/$(RUST_BIN) $(DIST_DIR)/$(RUST_BIN)

cli: dist
	cargo build --manifest-path serve-cli/Cargo.toml --release
	install -m 755 serve-cli/target/release/$(CLI_BIN) $(DIST_DIR)/$(CLI_BIN)

compress: build
	@command -v upx >/dev/null 2>&1 || { echo "upx not found in PATH"; exit 1; }
	@command echo "Compressing $(GO_BIN), $(RUST_BIN), $(CLI_BIN)"
	upx --best --lzma -q --no-progress $(DIST_DIR)/$(GO_BIN) > /dev/null
	upx --best --lzma -q --no-progress $(DIST_DIR)/$(RUST_BIN) > /dev/null
	upx --best --lzma -q --no-progress $(DIST_DIR)/$(CLI_BIN) > /dev/null

release-tar: compress
	@rm -f serve-$(VERSION)_rel.tar
	tar -C $(DIST_DIR) --transform='s,^,serve-$(VERSION)/,' -cf serve-$(VERSION)_rel.tar .

install: build
	install -m 755 $(DIST_DIR)/$(GO_BIN) $(BIN_DIR)/$(GO_BIN)
	install -m 755 $(DIST_DIR)/$(RUST_BIN) $(BIN_DIR)/$(RUST_BIN)
	install -m 755 $(DIST_DIR)/$(CLI_BIN) $(BIN_DIR)/$(CLI_BIN)

clean:
	rm -rf $(DIST_DIR)
	cargo clean --manifest-path serve-rs/Cargo.toml
	cargo clean --manifest-path serve-cli/Cargo.toml
	cd serve-go && go clean
