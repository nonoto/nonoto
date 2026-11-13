# Windows向けのコアのビルドに使うターゲット。開発コンテナにMinGWを入れている。
WINDOWS_TARGET := x86_64-pc-windows-gnu

.PHONY: fmt
fmt:
	cargo fmt --all

.PHONY: fmt-check
fmt-check:
	cargo fmt --all --check

.PHONY: lint
lint:
	cargo clippy --workspace --all-targets --locked -- -D warnings

.PHONY: test
test:
	cargo test --workspace --locked

.PHONY: build-core-windows
build-core-windows:
	cargo build -p nonoto-core --target $(WINDOWS_TARGET) --locked

# 開発コンテナの仮想ディスプレイでデスクトップアプリを起動し、画面を tmp/screenshot-desktop.png に保存する。
.PHONY: screenshot-desktop
screenshot-desktop:
	scripts/screenshot-desktop.sh

# 数千行のMarkdownのページを1件作る。行数は LINES で変えられる (例: make seed-large-page LINES=10000)。
LINES ?= 5000

.PHONY: seed-large-page
seed-large-page:
	scripts/seed-large-page.sh $(LINES)
