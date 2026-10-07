" Vim syntax file
" Language:    TigerGraph GSQL
" Maintainer:  gsql-lsp contributors
" URL:         https://github.com/gsql-lsp/gsql-lsp

if exists('b:current_syntax')
  finish
endif

let s:cpo_save = &cpo
set cpo&vim

" GSQL keywords are case-insensitive.
syntax case ignore

syntax keyword gsqlConditional if then else end case when
" The stubs of the built-ins (the reference file): only as whole declaration lines,
" since `type`, `object` and `method` are common names.
syntax match gsqlKeyword /\c^\s*\(builtin\s\+\(function\|object\|type\|keyword\|constant\)\|method\|mutator\)\>/
syntax keyword gsqlRepeat while foreach do break continue
syntax keyword gsqlException try raise exception
syntax keyword gsqlStatement return returns print log
syntax keyword gsqlOperatorWord and or not in is like escape between union intersect minus
syntax keyword gsqlKeyword select from where accum having order by limit offset sample target pinned
syntax keyword gsqlKeyword per group into as asc desc distinct insert values delete update set
syntax keyword gsqlKeyword create replace drop alter add query function interpret install run show use
syntax keyword gsqlKeyword graph global distributed template opencypher syntax api for to
syntax keyword gsqlKeyword directed undirected primary_id primary key default with discriminator attribute index pair
syntax keyword gsqlKeyword loading schema_change job define filename header input_line_filter load temp_table using option
syntax keyword gsqlKeyword data_source package grant revoke role privilege user secret token tag description
syntax keyword gsqlKeyword ls begin abort resume export import clear store version help quit exit cascade all on
syntax keyword gsqlKeyword interval range leading trailing both static empty numeric typedef tuple compress
syntax keyword gsqlKeyword year month day hour minute second to_csv
syntax keyword gsqlKeyword nullable admin virtual tags overwrite password schema vector workload queue proxy
syntax match gsqlKeyword "\<post[-_]accum\>"

syntax keyword gsqlType int uint float double bool string datetime vertex edge jsonobject jsonarray list bag map file
syntax match gsqlAccumulatorType "\<\%(sum\|max\|min\|avg\|or\|and\|bitwiseor\|bitwiseand\|deviation\|deviationp\|list\|set\|bag\|map\|heap\|groupby\|array\)accum\>"

syntax keyword gsqlBoolean true false
syntax keyword gsqlConstant null any
syntax match gsqlConstant "\<gsql_\%(u\)\=int_\%(max\|min\)\>"

syntax match gsqlGlobalAccumulator "@@\h\w*"
syntax match gsqlLocalAccumulator "\%(@\)\@<!@\h\w*'\="
syntax match gsqlColumn "\$\%(\d\+\|\"[^\"]*\"\%(:\"[^\"]*\"\)*\|\h[[:alnum:]_.]*\)"
" A command option, not a negative value (`x = -y`, `RETURN -y`).
syntax match gsqlOption "\%(^\|\s\)\zs\%([=+*/%<>(,!|&^-]\s*\)\@<!\%(\<\%(return\|print\|then\|else\|when\|and\|or\|not\|in\|limit\|by\)\s\+\)\@<!-\a\w*"

syntax match gsqlNumber "\<\d\+\%(\.\d\+\)\=\%([eE][+-]\=\d\+\)\=\>"
syntax match gsqlNumber "\%(\w\)\@<!\.\d\+\%([eE][+-]\=\d\+\)\=\>"

syntax match gsqlEscape contained "\\."
syntax region gsqlString start=+"+ skip=+\\.+ end=+"+ contains=gsqlEscape

syntax match gsqlFunction "\<\h\w*\ze\s*("
" Attributes named like keywords (`t.order`): a match that starts at the dot
" takes priority over the keyword.
syntax match gsqlProperty "\.\h\w*\>\%(\s*(\)\@!"

syntax keyword gsqlTodo contained TODO FIXME XXX NOTE
syntax match gsqlComment "//.*$" contains=gsqlTodo,@Spell
syntax match gsqlComment "#.*$" contains=gsqlTodo,@Spell
syntax region gsqlComment start="/\*" end="\*/" contains=gsqlTodo,@Spell

highlight default link gsqlConditional Conditional
highlight default link gsqlRepeat Repeat
highlight default link gsqlException Exception
highlight default link gsqlStatement Statement
highlight default link gsqlOperatorWord Operator
highlight default link gsqlKeyword Keyword
highlight default link gsqlType Type
highlight default link gsqlAccumulatorType Type
highlight default link gsqlBoolean Boolean
highlight default link gsqlConstant Constant
highlight default link gsqlGlobalAccumulator Identifier
highlight default link gsqlLocalAccumulator Identifier
highlight default link gsqlColumn Special
highlight default link gsqlOption Special
highlight default link gsqlNumber Number
highlight default link gsqlEscape SpecialChar
highlight default link gsqlString String
highlight default link gsqlFunction Function
highlight default link gsqlTodo Todo
highlight default link gsqlComment Comment

let b:current_syntax = 'gsql'

let &cpo = s:cpo_save
unlet s:cpo_save
