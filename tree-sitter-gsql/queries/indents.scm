; Indentation, matching `gsql-lsp format`: blocks, bracketed lists and the
; continuation lines of statements are indented one level; closing brackets,
; END, the ELSE of an IF, EXCEPTION and SELECT clauses line up with the line
; that opened them. A WHEN (or the ELSE of a CASE) is one level in, and its
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
] @indent.begin

(case_statement
  (else_clause) @indent.begin)

(try_statement
  (else_clause) @indent.begin)

; Only the brackets of the nodes above close an indentation.
[
  (query_body
    "}" @indent.branch @indent.end)
  (opencypher_body
    "}" @indent.branch @indent.end)
  (loading_job_body
    "}" @indent.branch @indent.end)
  (schema_change_body
    "}" @indent.branch @indent.end)
  (vertex_set_literal
    "}" @indent.branch @indent.end)
  (property_map
    "}" @indent.branch @indent.end)
  (vertex_attribute_list
    ")" @indent.branch @indent.end)
  (edge_attribute_list
    ")" @indent.branch @indent.end)
  (parameter_list
    ")" @indent.branch @indent.end)
  (argument_list
    ")" @indent.branch @indent.end)
  (value_list
    ")" @indent.branch @indent.end)
  (insert_columns
    ")" @indent.branch @indent.end)
  (column_list
    ")" @indent.branch @indent.end)
  (parenthesized_expression
    ")" @indent.branch @indent.end)
  (list_literal
    "]" @indent.branch @indent.end)
  (subscript_expression
    "]" @indent.branch @indent.end)
]

(if_statement
  "END" @indent.branch @indent.end)

(if_statement
  (else_if_clause
    [
      "ELSE"
      "ELSE IF"
    ] @indent.branch))

(if_statement
  (else_clause
    "ELSE" @indent.branch))

(case_statement
  "END" @indent.branch @indent.end)

(case_expression
  "END" @indent.branch @indent.end)

(while_statement
  "END" @indent.branch @indent.end)

(foreach_statement
  "END" @indent.branch @indent.end)

(try_statement
  [
    "EXCEPTION"
    "END"
  ] @indent.branch)

(try_statement
  "END" @indent.end)

(select_statement
  [
    (from_clause)
    (sample_clause)
    (where_clause)
    (accum_clause)
    (post_accum_clause)
    (group_by_clause)
    (having_clause)
    (order_by_clause)
    (limit_clause)
  ] @indent.branch)

(comment) @indent.auto

(string) @indent.ignore
