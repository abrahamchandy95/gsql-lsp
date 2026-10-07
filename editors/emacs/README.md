# GSQL for Emacs

`gsql-ts-mode.el` is a tree-sitter major mode (Emacs 29.1 or later, built with
tree-sitter support) with font-lock, indentation, imenu and defun navigation. It
registers `gsql-lsp` with Eglot (built in since Emacs 29).

## 1. The server

```sh
sh scripts/install.sh                     # release binary (from a checkout; see the script header for options)
cargo install --path crates/gsql-lsp      # or build from a checkout
```

or download `gsql-lsp-<target>.tar.gz` from the GitHub releases page and put the
binary on PATH (Emacs reads `exec-path`; GUI Emacs on macOS may need
`exec-path-from-shell`).

## 2. The mode

```elisp
(add-to-list 'load-path "/path/to/gsql-lsp/editors/emacs")
(require 'gsql-ts-mode)
(add-hook 'gsql-ts-mode-hook #'eglot-ensure)
```

## 3. The grammar

Needs a C compiler. `gsql-ts-mode.el` adds this entry to
`treesit-language-source-alist` (the repository URL appears there only; the
generated `src/parser.c` is committed, so the tree-sitter CLI is not needed):

```elisp
(gsql "https://github.com/gsql-lsp/gsql-lsp" "main" "tree-sitter-gsql/src")
```

Run `M-x treesit-install-language-grammar RET gsql RET` once. From a local
checkout, use a path instead:

```elisp
(setf (alist-get 'gsql treesit-language-source-alist)
      '("/path/to/gsql-lsp" nil "tree-sitter-gsql/src"))
```

Open a `.gsql` file; `M-x eglot` starts the server if `eglot-ensure` is not used.

Status: not run (Emacs is not installed on the development machine).
