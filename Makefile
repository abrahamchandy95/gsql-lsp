TREE_SITTER ?= tree-sitter
GSQL_LSP_BIN ?= target/debug/gsql-lsp

.PHONY: all build release install generate grammar-test queries test lint editor-test neovim-test packaging-test docs-examples builtin-docs clean

all: build

build:
	cargo build

release:
	cargo build --release

install:
	cargo install --path crates/gsql-lsp

# Regenerate tree-sitter-gsql/src from grammar.js (no Node.js needed).
generate:
	cd tree-sitter-gsql && $(TREE_SITTER) generate --js-runtime native

grammar-test:
	cd tree-sitter-gsql && $(TREE_SITTER) test

# Regenerate the editor-specific query files from tree-sitter-gsql/queries.
queries:
	python3 scripts/sync_queries.py

test: grammar-test
	cargo test --workspace
	python3 scripts/sync_queries.py --check

lint:
	cargo fmt --all -- --check
	cargo clippy -p gsql-lsp --all-targets -- -D warnings

editor-test: neovim-test
	python3 editors/vscode/test/tokenize.py --test
	vim -N -u NONE -es -S editors/vim/test/syntax.vim </dev/null

neovim-test: build
	GSQL_LSP_BIN=$(GSQL_LSP_BIN) nvim --headless --clean -u NONE -l editors/neovim/test/e2e.lua
	GSQL_LSP_BIN=$(GSQL_LSP_BIN) nvim --headless --clean -u NONE -l editors/neovim/test/capabilities.lua
	GSQL_LSP_BIN=$(GSQL_LSP_BIN) nvim --headless --clean -u NONE -l editors/neovim/test/install.lua
	nvim --headless --clean -u NONE -l editors/neovim/test/layout.lua   # LAZY_NVIM=<lazy.nvim dir> adds a real lazy.nvim run

packaging-test: build
	GSQL_LSP_BIN=$(GSQL_LSP_BIN) sh scripts/test_install.sh
	sh scripts/test_homebrew.sh
	cargo package --workspace --allow-dirty

docs-examples:
	python3 scripts/docs_examples.py

builtin-docs:
	python3 scripts/sync_builtin_docs.py

clean:
	cargo clean
	rm -rf editors/neovim/parser
