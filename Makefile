DIST_DIR := dist
SERVER_BIN := serve
CLI_BIN := serve-cli
PREFIX ?= /usr/local
BIN_DIR := $(PREFIX)/bin
VERSION := $(shell sed -n 's/^version[[:space:]]*=[[:space:]]*"\(.*\)"/\1/p' Cargo.toml | head -n 1)
TARGETS ?= $(shell rustup target list --installed)
HOST_TRIPLE ?= $(shell rustc -vV | sed -n 's/^host: //p')
TARGET ?= $(HOST_TRIPLE)
BIN_EXT :=
ifneq (,$(findstring windows,$(TARGET)))
	BIN_EXT := .exe
endif
ANDROID_API ?= 21
NDK_HOME ?= $(shell \
	if [ -n "$$ANDROID_NDK_HOME" ]; then echo $$ANDROID_NDK_HOME; \
	elif [ -n "$$ANDROID_NDK_ROOT" ]; then echo $$ANDROID_NDK_ROOT; \
	elif [ -d "$$HOME/Android/Sdk/ndk" ]; then \
		ls -d $$HOME/Android/Sdk/ndk/* 2>/dev/null | sort -V | tail -n 1; \
	fi)
NDK_HOST_TAG ?= linux-x86_64
CARGO_TARGET_FLAG :=
DIST_TARGET_DIR := $(DIST_DIR)
TARGET_RELEASE_DIR := target/release
ifneq ($(TARGET),$(HOST_TRIPLE))
	CARGO_TARGET_FLAG := --target $(TARGET)
	DIST_TARGET_DIR := $(DIST_DIR)/$(TARGET)
	TARGET_RELEASE_DIR := target/$(TARGET)/release
endif

.PHONY: all build server cli clean dist release compress install build-targets compress-targets

all: build

build: dist server cli

dist:
	mkdir -p $(DIST_TARGET_DIR)

build-targets: dist
	@set -e; \
	skipped=0; \
	for target in $(TARGETS); do \
		target_underscore=$$(printf '%s' "$$target" | tr '-' '_'); \
		target_upper=$$(printf '%s' "$$target_underscore" | tr '[:lower:]' '[:upper:]'); \
		alt_target=$$(printf '%s' "$$target" | sed 's/-unknown-/-/'); \
		cc_var=""; \
		linker_var=""; \
		if [ "$$target" != "$(HOST_TRIPLE)" ]; then \
			eval "cc_var=\$${CC_$${target_underscore}}"; \
			eval "linker_var=\$${CARGO_TARGET_$${target_upper}_LINKER}"; \
			if [ -z "$$cc_var" ] && [ -z "$$linker_var" ]; then \
				case "$$target" in \
					aarch64-linux-android) clang_prefix=aarch64-linux-android ;; \
					armv7-linux-androideabi) clang_prefix=armv7a-linux-androideabi ;; \
					i686-linux-android) clang_prefix=i686-linux-android ;; \
					x86_64-linux-android) clang_prefix=x86_64-linux-android ;; \
					*) clang_prefix="";; \
				esac; \
				if [ -n "$$clang_prefix" ] && [ -n "$(NDK_HOME)" ]; then \
					ndk_prebuilt="$(NDK_HOME)/toolchains/llvm/prebuilt/$(NDK_HOST_TAG)"; \
					candidate="$$ndk_prebuilt/bin/$${clang_prefix}$(ANDROID_API)-clang"; \
					if [ -x "$$candidate" ]; then \
						cc_var="$$candidate"; \
						linker_var="$$candidate"; \
					fi; \
				fi; \
			fi; \
			if [ -z "$$cc_var" ] && [ -z "$$linker_var" ]; then \
				case "$$target" in \
					x86_64-pc-windows-gnu) mingw_prefix=x86_64-w64-mingw32 ;; \
					i686-pc-windows-gnu) mingw_prefix=i686-w64-mingw32 ;; \
					*) mingw_prefix="";; \
				esac; \
				if [ -n "$$mingw_prefix" ]; then \
					if command -v "$$mingw_prefix-gcc" >/dev/null 2>&1; then \
						cc_var="$$(command -v $$mingw_prefix-gcc)"; \
						linker_var="$$cc_var"; \
					elif command -v "$$mingw_prefix-clang" >/dev/null 2>&1; then \
						cc_var="$$(command -v $$mingw_prefix-clang)"; \
						linker_var="$$cc_var"; \
					fi; \
				fi; \
			fi; \
			if [ -z "$$cc_var" ] && [ -z "$$linker_var" ] \
				&& ! command -v "$$target-gcc" >/dev/null 2>&1 \
				&& ! command -v "$$target-clang" >/dev/null 2>&1 \
				&& ! command -v "$$alt_target-gcc" >/dev/null 2>&1 \
				&& ! command -v "$$alt_target-clang" >/dev/null 2>&1; then \
				echo "!! Skipping $$target (no toolchain found; set CC_$${target_underscore} or CARGO_TARGET_$${target_upper}_LINKER)"; \
				skipped=$$((skipped+1)); \
				continue; \
			fi; \
		fi; \
		echo "==> Building for $$target"; \
		mkdir -p $(DIST_DIR)/$$target; \
		bin_ext=""; \
		case "$$target" in \
			*-pc-windows-*) bin_ext=".exe" ;; \
			*) bin_ext="" ;; \
		esac; \
		cc_arg=""; \
		linker_arg=""; \
		if [ -n "$$cc_var" ]; then \
			cc_arg="CC_$${target_underscore}=$$cc_var"; \
		fi; \
		if [ -n "$$linker_var" ]; then \
			linker_arg="CARGO_TARGET_$${target_upper}_LINKER=$$linker_var"; \
		fi; \
		env $$cc_arg $$linker_arg cargo build --package serve --release --quiet --target $$target; \
		install -m 755 target/$$target/release/$(SERVER_BIN)$${bin_ext} $(DIST_DIR)/$$target/$(SERVER_BIN)$${bin_ext}; \
		env $$cc_arg $$linker_arg cargo build --package serve-cli --release --quiet --target $$target; \
		install -m 755 target/$$target/release/$(CLI_BIN)$${bin_ext} $(DIST_DIR)/$$target/$(CLI_BIN)$${bin_ext}; \
	done; \
	if [ $$skipped -ne 0 ]; then \
		echo "Finished with $$skipped skipped target(s)."; \
	fi

server: dist
	cargo build --package serve --release --quiet $(CARGO_TARGET_FLAG)
	install -m 755 $(TARGET_RELEASE_DIR)/$(SERVER_BIN)$(BIN_EXT) $(DIST_TARGET_DIR)/$(SERVER_BIN)$(BIN_EXT)

cli: dist
	cargo build --package serve-cli --release --quiet $(CARGO_TARGET_FLAG)
	install -m 755 $(TARGET_RELEASE_DIR)/$(CLI_BIN)$(BIN_EXT) $(DIST_TARGET_DIR)/$(CLI_BIN)$(BIN_EXT)

compress: build
	@command -v upx >/dev/null 2>&1 || { echo "upx not found in PATH"; exit 1; }
	@command echo "Compressing $(SERVER_BIN), $(CLI_BIN)"
	upx --best --lzma -q --no-progress $(DIST_TARGET_DIR)/$(SERVER_BIN)$(BIN_EXT) > /dev/null
	upx --best --lzma -q --no-progress $(DIST_TARGET_DIR)/$(CLI_BIN)$(BIN_EXT) > /dev/null

compress-targets: build-targets
	@command -v upx >/dev/null 2>&1 || { echo "upx not found in PATH"; exit 1; }
	@set -e; \
	for target in $(TARGETS); do \
		bin_ext=""; \
		case "$$target" in \
			*-pc-windows-*) bin_ext=".exe" ;; \
			*) bin_ext="" ;; \
		esac; \
		if [ -f "$(DIST_DIR)/$$target/$(SERVER_BIN)$${bin_ext}" ]; then \
			echo "==> Compressing $$target"; \
			upx --best --lzma -q --no-progress $(DIST_DIR)/$$target/$(SERVER_BIN)$${bin_ext} > /dev/null; \
			upx --best --lzma -q --no-progress $(DIST_DIR)/$$target/$(CLI_BIN)$${bin_ext} > /dev/null; \
		fi; \
	done

release: compress-targets
	@set -e; \
	for target in $(TARGETS); do \
		bin_ext=""; \
		case "$$target" in \
			*-pc-windows-*) bin_ext=".exe" ;; \
			*) bin_ext="" ;; \
		esac; \
		if [ -f "$(DIST_DIR)/$$target/$(SERVER_BIN)$${bin_ext}" ]; then \
			echo "==> Packaging $$target"; \
			tar -C $(DIST_DIR)/$$target \
				--transform='s,^\./,,' \
				--transform='s,^,serve-$(VERSION)/,' \
				-cf serve-$(VERSION)_$${target}_rel.tar .; \
		fi; \
	done

install: compress
	install -m 755 $(DIST_TARGET_DIR)/$(SERVER_BIN)$(BIN_EXT) $(BIN_DIR)/$(SERVER_BIN)$(BIN_EXT)
	install -m 755 $(DIST_TARGET_DIR)/$(CLI_BIN)$(BIN_EXT) $(BIN_DIR)/$(CLI_BIN)$(BIN_EXT)

clean:
	rm -rf $(DIST_DIR)
	cargo clean
