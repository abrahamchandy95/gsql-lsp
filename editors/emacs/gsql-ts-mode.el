;;; gsql-ts-mode.el --- Tree-sitter major mode for TigerGraph GSQL  -*- lexical-binding: t; -*-

;; Author: Abraham Chandy
;; Version: 0.1.0
;; Package-Requires: ((emacs "29.1"))
;; Keywords: languages, databases
;; URL: https://github.com/abrahamchandy95/gsql-lsp

;;; Commentary:

;; A major mode for TigerGraph GSQL built on the tree-sitter-gsql grammar,
;; with font-lock, indentation, imenu and defun navigation.  It registers the
;; gsql-lsp language server with Eglot.
;;
;; Install the grammar once with M-x treesit-install-language-grammar RET gsql.

;;; Code:

(require 'treesit)
(eval-when-compile (require 'rx))

(declare-function treesit-parser-create "treesit.c")
(declare-function treesit-node-child-by-field-name "treesit.c")
(declare-function treesit-node-text "treesit.c")
(declare-function treesit-node-type "treesit.c")

(defgroup gsql nil
  "TigerGraph GSQL."
  :group 'languages
  :prefix "gsql-")

(defcustom gsql-ts-mode-indent-offset 4
  "Number of spaces for each indentation step in `gsql-ts-mode'.
The GSQL Style Guide indents the body of a block by 4 spaces."
  :type 'integer
  :safe 'integerp
  :group 'gsql)

(add-to-list 'treesit-language-source-alist
             '(gsql "https://github.com/abrahamchandy95/gsql-lsp" "main" "tree-sitter-gsql/src"))

(defvar gsql-ts-mode--syntax-table
  (let ((table (make-syntax-table)))
    (modify-syntax-entry ?_ "_" table)
    (modify-syntax-entry ?@ "_" table)
    (modify-syntax-entry ?\" "\"" table)
    (modify-syntax-entry ?\\ "\\" table)
    (modify-syntax-entry ?/ ". 124b" table)
    (modify-syntax-entry ?* ". 23" table)
    (modify-syntax-entry ?# "< b" table)
    (modify-syntax-entry ?\n "> b" table)
    (modify-syntax-entry ?' "." table)
    table)
  "Syntax table for `gsql-ts-mode'.")

;; BEGIN GENERATED KEYWORDS (scripts/sync_queries.py)
(defvar gsql-ts-mode--control-keywords
  '(
    "BREAK" "CASE" "CONTINUE" "DO" "ELSE" "ELSE IF" "END" "EXCEPTION"
    "FOREACH" "IF" "RAISE" "RETURN" "RETURNS" "THEN" "TRY" "WHEN"
    "WHILE")
  "GSQL control-flow keywords (tree-sitter node names).")

(defvar gsql-ts-mode--operator-keywords
  '(
    "AND" "BETWEEN" "ESCAPE" "IN" "INTERSECT" "IS" "LIKE" "MINUS" "NOT"
    "OR" "UNION")
  "GSQL operator keywords (tree-sitter node names).")

(defvar gsql-ts-mode--keywords
  '(
    "ABORT" "ACCUM" "ADD" "ADMIN" "ALL" "ALTER" "API" "AS" "ASC"
    "ATTRIBUTE" "BAG" "BEGIN" "BOOL" "BOTH" "BUILTIN" "BY" "CASCADE"
    "CLEAR" "COMPRESS" "CONSTANT" "CREATE" "DATA_SOURCE" "DATETIME"
    "DAY" "DEFAULT" "DEFINE" "DELETE" "DESC" "DESCRIPTION" "DIRECTED"
    "DISCRIMINATOR" "DISTINCT" "DISTRIBUTED" "DOUBLE" "DROP" "EDGE"
    "EMPTY" "EXIT" "EXPORT" "FILE" "FILENAME" "FIXED_BINARY" "FLOAT"
    "FOR" "FROM" "FUNCTION" "GET" "GLOBAL" "GRANT" "GRANTS" "GRAPH"
    "GROUP" "HAVING" "HEADER" "HELP" "HOUR" "IMPORT" "INDEX"
    "INPUT_LINE_FILTER" "INSERT" "INSTALL" "INT" "INTERPRET" "INTERVAL"
    "INTO" "JOB" "JSONARRAY" "JSONOBJECT" "KEY" "KEYWORD" "LEADING"
    "LIMIT" "LIST" "LOAD" "LOADING" "LS" "MAP" "METHOD" "MINUTE" "MONTH"
    "MUTATOR" "NULLABLE" "NUMERIC" "OBJECT" "OF" "OFFSET" "ON"
    "OPENCYPHER" "OPTION" "ORDER" "OVERWRITE" "PACKAGE" "PAIR"
    "PASSWORD" "PER" "PINNED" "POLICY" "POST-ACCUM" "PRIMARY"
    "PRIMARY_ID" "PRINT" "PRIVILEGE" "PROXY" "PUT" "QUERY" "QUERY_PARAM"
    "QUEUE" "QUIT" "RANGE" "REPLACE" "RESUME" "REVOKE" "ROLE" "ROW"
    "RUN" "SAMPLE" "SCHEMA" "SCHEMA_CHANGE" "SECOND" "SECRET" "SELECT"
    "SET" "SHOW" "STATIC" "STORE" "STRING" "SYNTAX" "TAG" "TAGS"
    "TARGET" "TEMPLATE" "TEMP_TABLE" "TO" "TOKEN" "TO_CSV" "TRAILING"
    "TUPLE" "TYPE" "TYPEDEF" "UINT" "UNDIRECTED" "UPDATE" "USE" "USER"
    "USING" "VALUES" "VECTOR" "VERSION" "VERTEX" "VIRTUAL" "WHERE"
    "WITH" "WORKLOAD" "YEAR")
  "Other GSQL keywords (tree-sitter node names).")
;; END GENERATED KEYWORDS

(defvar gsql-ts-mode--font-lock-settings
  ;; Each query needs its own :language.  A face applies only where no
  ;; earlier rule put one, so types come before keywords: `VERTEX' and `SET'
  ;; are types in `VERTEX<Person>' and `SET<INT>', keywords elsewhere.
  (treesit-font-lock-rules
   :language 'gsql
   :feature 'comment
   '((comment) @font-lock-comment-face)

   :language 'gsql
   :feature 'string
   '((string) @font-lock-string-face)

   :language 'gsql
   :feature 'string
   :override t
   '((escape_sequence) @font-lock-escape-face)

   :language 'gsql
   :feature 'type
   '((primitive_type) @font-lock-type-face
     (accumulator_kind) @font-lock-type-face
     (type_identifier) @font-lock-type-face
     (vertex_type "VERTEX" @font-lock-type-face)
     (vertex_type type: (identifier) @font-lock-type-face)
     (edge_type "EDGE" @font-lock-type-face)
     (edge_type type: (identifier) @font-lock-type-face)
     (collection_type kind: _ @font-lock-type-face)
     (vertex_pattern type: (identifier) @font-lock-type-face)
     (edge_atom name: (identifier) @font-lock-type-face)
     (node_pattern type: (identifier) @font-lock-type-face)
     (relationship_detail type: (identifier) @font-lock-type-face)
     (edge_pair from: (identifier) @font-lock-type-face)
     (edge_pair to: (identifier) @font-lock-type-face)
     (type_wildcard type: (identifier) @font-lock-type-face)
     (load_destination target: (identifier) @font-lock-type-face)
     (insert_statement target: (identifier) @font-lock-type-face))

   :language 'gsql
   :feature 'keyword
   `([,@gsql-ts-mode--keywords] @font-lock-keyword-face
     [,@gsql-ts-mode--control-keywords] @font-lock-keyword-face
     [,@gsql-ts-mode--operator-keywords] @font-lock-operator-face)

   :language 'gsql
   :feature 'definition
   '((vertex_definition name: (identifier) @font-lock-type-face)
     (edge_definition name: (identifier) @font-lock-type-face)
     (typedef_statement name: (identifier) @font-lock-type-face)
     (virtual_edge_declaration name: (identifier) @font-lock-type-face)
     (graph_definition name: (identifier) @font-lock-constant-face)
     (query_definition name: (_) @font-lock-function-name-face)
     (opencypher_query_definition name: (_) @font-lock-function-name-face)
     (loading_job_definition name: (identifier) @font-lock-function-name-face)
     (schema_change_job_definition name: (identifier) @font-lock-function-name-face)
     (parameter name: (identifier) @font-lock-variable-name-face)
     (variable_declarator name: (identifier) @font-lock-variable-name-face)
     (attribute_definition name: (identifier) @font-lock-property-name-face)
     (primary_id_definition name: (identifier) @font-lock-property-name-face))

   :language 'gsql
   :feature 'variable
   '((global_accumulator) @font-lock-variable-use-face
     (local_accumulator) @font-lock-property-use-face
     (column_reference) @font-lock-builtin-face
     (for_graph_clause graph: (identifier) @font-lock-constant-face)
     (use_statement graph: (identifier) @font-lock-constant-face))

   :language 'gsql
   :feature 'function
   '((call_expression function: (identifier) @font-lock-function-call-face)
     (call_expression function: (member_expression property: (identifier) @font-lock-function-call-face)))

   :language 'gsql
   :feature 'property
   '((member_expression property: (identifier) @font-lock-property-use-face))

   :language 'gsql
   :feature 'constant
   '((boolean) @font-lock-constant-face
     (null) @font-lock-constant-face
     (any) @font-lock-constant-face
     (wildcard) @font-lock-constant-face)

   :language 'gsql
   :feature 'number
   '((integer) @font-lock-number-face
     (float) @font-lock-number-face)

   :language 'gsql
   :feature 'operator
   '(["=" "+=" "==" "!=" "<>" "<=" ">=" "+" "-" "*" "/" "%" "&" "|" "<<" ">>" "->" "<-" "'"]
     @font-lock-operator-face)

   :language 'gsql
   :feature 'bracket
   '(["(" ")" "[" "]" "{" "}"] @font-lock-bracket-face)

   :language 'gsql
   :feature 'delimiter
   '(["," ";" "." ":" ".."] @font-lock-delimiter-face))
  "Tree-sitter font-lock settings for `gsql-ts-mode'.")

(defvar gsql-ts-mode--indent-rules
  ;; `node-is' matches a regexp, ignoring case by default: anchor exact types
  ;; (or "EXCEPTION" would also match `exception_declaration').
  `((gsql
     ((node-is "\\`}\\'") parent-bol 0)
     ((node-is "\\`)\\'") parent-bol 0)
     ((node-is "\\`]\\'") parent-bol 0)
     ((node-is "\\`END\\'") parent-bol 0)
     ((node-is "\\`else_if_clause\\'") parent-bol 0)
     ((node-is "\\`else_clause\\'") parent-bol 0)
     ((node-is "\\`EXCEPTION\\'") parent-bol 0)
     ((parent-is "query_body") parent-bol gsql-ts-mode-indent-offset)
     ((parent-is "opencypher_body") parent-bol gsql-ts-mode-indent-offset)
     ((parent-is "loading_job_body") parent-bol gsql-ts-mode-indent-offset)
     ((parent-is "schema_change_body") parent-bol gsql-ts-mode-indent-offset)
     ((parent-is "block") parent-bol 0)
     ((parent-is "if_statement") parent-bol gsql-ts-mode-indent-offset)
     ((parent-is "else_if_clause") parent-bol gsql-ts-mode-indent-offset)
     ((parent-is "else_clause") parent-bol gsql-ts-mode-indent-offset)
     ((parent-is "while_statement") parent-bol gsql-ts-mode-indent-offset)
     ((parent-is "foreach_statement") parent-bol gsql-ts-mode-indent-offset)
     ((parent-is "case_statement") parent-bol gsql-ts-mode-indent-offset)
     ((parent-is "when_clause") parent-bol gsql-ts-mode-indent-offset)
     ((parent-is "try_statement") parent-bol gsql-ts-mode-indent-offset)
     ((parent-is "exception_handler") parent-bol gsql-ts-mode-indent-offset)
     ((parent-is "select_statement") parent-bol gsql-ts-mode-indent-offset)
     ((parent-is "accum_clause") parent-bol gsql-ts-mode-indent-offset)
     ((parent-is "post_accum_clause") parent-bol gsql-ts-mode-indent-offset)
     ((parent-is "vertex_attribute_list") parent-bol gsql-ts-mode-indent-offset)
     ((parent-is "edge_attribute_list") parent-bol gsql-ts-mode-indent-offset)
     ((parent-is "parameter_list") parent-bol gsql-ts-mode-indent-offset)
     ((parent-is "argument_list") parent-bol gsql-ts-mode-indent-offset)
     ((parent-is "source_file") column-0 0)
     (no-node parent-bol 0)))
  "Tree-sitter indentation rules for `gsql-ts-mode'.")

(defun gsql-ts-mode--defun-name (node)
  "Return the name of the definition NODE, or nil."
  (when-let ((name (treesit-node-child-by-field-name node "name")))
    (treesit-node-text name t)))

(defconst gsql-ts-mode--definitions
  (rx bos (or "query_definition" "opencypher_query_definition" "vertex_definition"
              "edge_definition" "graph_definition" "loading_job_definition"
              "schema_change_job_definition" "typedef_statement")
      eos)
  "Tree-sitter node types that define named GSQL objects.")

;;;###autoload
(define-derived-mode gsql-ts-mode prog-mode "GSQL"
  "Major mode for TigerGraph GSQL, powered by tree-sitter."
  :group 'gsql
  :syntax-table gsql-ts-mode--syntax-table
  (setq-local comment-start "// ")
  (setq-local comment-end "")
  (setq-local comment-start-skip (rx (or "//" "#" "/*") (* (syntax whitespace))))
  ;; The GSQL Style Guide: spaces instead of tabs.
  (setq-local indent-tabs-mode nil)
  (when (treesit-ready-p 'gsql)
    (treesit-parser-create 'gsql)
    (setq-local treesit-font-lock-settings gsql-ts-mode--font-lock-settings)
    (setq-local treesit-font-lock-feature-list
                '((comment definition)
                  (keyword string type)
                  (constant number variable function)
                  (bracket delimiter operator property)))
    (setq-local treesit-simple-indent-rules gsql-ts-mode--indent-rules)
    (setq-local treesit-defun-type-regexp gsql-ts-mode--definitions)
    (setq-local treesit-defun-name-function #'gsql-ts-mode--defun-name)
    (setq-local treesit-simple-imenu-settings
                '(("Query" "\\`\\(?:opencypher_\\)?query_definition\\'" nil nil)
                  ("Vertex" "\\`vertex_definition\\'" nil nil)
                  ("Edge" "\\`edge_definition\\'" nil nil)
                  ("Graph" "\\`graph_definition\\'" nil nil)
                  ("Job" "\\`\\(?:loading\\|schema_change\\)_job_definition\\'" nil nil)
                  ("Tuple" "\\`typedef_statement\\'" nil nil)))
    (treesit-major-mode-setup)))

;;;###autoload
(add-to-list 'auto-mode-alist '("\\.gsql?\\'" . gsql-ts-mode))

(with-eval-after-load 'eglot
  (defvar eglot-server-programs)
  (add-to-list 'eglot-server-programs '(gsql-ts-mode . ("gsql-lsp"))))

(provide 'gsql-ts-mode)

;;; gsql-ts-mode.el ends here
