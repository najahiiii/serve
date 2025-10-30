DIST_DIR := dist
SERVER_BIN := serve
CLI_BIN := serve-cli
PREFIX ?= /usr/local
BIN_DIR := $(PREFIX)/bin
VERSION := $(shell sed -n 's/^version[[:space:]]*=[[:space:]]*"\(.*\)"/\1/p' Cargo.toml | head -n 1)

.PHONY: all build server cli clean dist release compress install

all: build

build: dist server cli

dist:
	mkdir -p $(DIST_DIR)

server: dist
	cargo build --package serve --release
	install -m 755 target/release/$(SERVER_BIN) $(DIST_DIR)/$(SERVER_BIN)

cli: dist
	cargo build --package serve-cli --release
	install -m 755 target/release/$(CLI_BIN) $(DIST_DIR)/$(CLI_BIN)

compress: build
	@command -v upx >/dev/null 2>&1 || { echo "upx not found in PATH"; exit 1; }
	@command echo "Compressing $(SERVER_BIN), $(CLI_BIN)"
	upx --best --lzma -q --no-progress $(DIST_DIR)/$(SERVER_BIN) > /dev/null
	upx --best --lzma -q --no-progress $(DIST_DIR)/$(CLI_BIN) > /dev/null

release: compress
	@rm -f serve-$(VERSION)_rel.tar
	tar -C $(DIST_DIR) --transform='s,^,serve-$(VERSION)/,' -cf serve-$(VERSION)_rel.tar .

install: compress
	install -m 755 $(DIST_DIR)/$(SERVER_BIN) $(BIN_DIR)/$(SERVER_BIN)
	install -m 755 $(DIST_DIR)/$(CLI_BIN) $(BIN_DIR)/$(CLI_BIN)

clean:
	rm -rf $(DIST_DIR)
	cargo clean
