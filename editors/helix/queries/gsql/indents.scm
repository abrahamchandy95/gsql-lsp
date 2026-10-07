; Helix indentation queries (hand-written: Helix uses @indent/@outdent),
; matching `gsql-lsp format`: blocks, bracketed lists and the continuation
; lines of statements are indented one level; closing brackets, END, the
; ELSE of an IF, EXCEPTION and SELECT clauses line up with the line that
; opened them. A WHEN (or the ELSE of a CASE) is one level in, and its
; statements two.

[
  (query_body)
  (opencypher_body)
  (loading_job_body)
  (schema_change_body)
  (if_statement)
  (case_statement)
  (while_statement)
  (foreach_statement)
  (try_statement)
  (when_clause)
  (exception_handler)
  (vertex_attribute_list)
  (edge_attribute_list)
  (parameter_list)
  (argument_list)
  (value_list)
  (insert_columns)
  (column_list)
  (vertex_set_literal)
  (list_literal)
  (property_map)
  (subscript_expression)
  (parenthesized_expression)
  (case_expression)
  (select_statement)
  (update_statement)
  (delete_statement)
  (insert_statement)
  (variable_declaration)
  (accumulator_declaration)
  (vertex_set_declaration)
  (assignment_statement)
  (expression_statement)
  (print_statement)
  (return_statement)
  (raise_statement)
  (load_statement)
] @indent

(case_statement
  (else_clause) @indent)

(try_statement
  (else_clause) @indent)

; Only the brackets of the nodes above close an indentation.
[
  (query_body
    "}" @outdent)
  (opencypher_body
    "}" @outdent)
  (loading_job_body
    "}" @outdent)
  (schema_change_body
    "}" @outdent)
  (vertex_set_literal
    "}" @outdent)
  (property_map
    "}" @outdent)
  (vertex_attribute_list
    ")" @outdent)
  (edge_attribute_list
    ")" @outdent)
  (parameter_list
    ")" @outdent)
  (argument_list
    ")" @outdent)
  (value_list
    ")" @outdent)
  (insert_columns
    ")" @outdent)
  (column_list
    ")" @outdent)
  (parenthesized_expression
    ")" @outdent)
  (list_literal
    "]" @outdent)
  (subscript_expression
    "]" @outdent)
]

(if_statement
  "END" @outdent)

(if_statement
  (else_if_clause
    [
      "ELSE"
      "ELSE IF"
    ] @outdent))

(if_statement
  (else_clause
    "ELSE" @outdent))

(case_statement
  "END" @outdent)

(case_expression
  "END" @outdent)

(while_statement
  "END" @outdent)

(foreach_statement
  "END" @outdent)

(try_statement
  [
    "EXCEPTION"
    "END"
  ] @outdent)

; SELECT clauses line up with the statement (their keyword: @outdent covers
; every line of the captured node).
(from_clause
  "FROM" @outdent)

(sample_clause
  "SAMPLE" @outdent)

(where_clause
  "WHERE" @outdent)

(accum_clause
  "ACCUM" @outdent)

(post_accum_clause
  "POST-ACCUM" @outdent)

(group_by_clause
  "GROUP" @outdent)

(having_clause
  "HAVING" @outdent)

(order_by_clause
  "ORDER" @outdent)

(limit_clause
  "LIMIT" @outdent)
