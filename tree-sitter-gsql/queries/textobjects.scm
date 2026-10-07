; Textobjects for nvim-treesitter-textobjects.

(query_definition) @function.outer

(query_definition
  body: (query_body) @function.inner)

(opencypher_query_definition) @function.outer

(loading_job_definition) @function.outer

(loading_job_definition
  body: (loading_job_body) @function.inner)

(schema_change_job_definition) @function.outer

(schema_change_job_definition
  body: (schema_change_body) @function.inner)

(vertex_definition) @class.outer

(vertex_definition
  attributes: (vertex_attribute_list) @class.inner)

(edge_definition) @class.outer

(edge_definition
  attributes: (edge_attribute_list) @class.inner)

; A parameter or argument with the comma before it, or after it for the
; first (captures of the same name in a match form one range).
(parameter_list
  "," @parameter.outer
  .
  (parameter) @parameter.inner @parameter.outer)

(parameter_list
  .
  (parameter) @parameter.inner @parameter.outer
  .
  ","? @parameter.outer)

(argument_list
  "," @parameter.outer
  .
  (_) @parameter.inner @parameter.outer)

(argument_list
  .
  (_) @parameter.inner @parameter.outer
  .
  ","? @parameter.outer)

(call_expression) @call.outer

(call_expression
  arguments: (argument_list) @call.inner)

(if_statement) @conditional.outer

(if_statement
  consequence: (block) @conditional.inner)

(case_statement) @conditional.outer

(while_statement) @loop.outer

(while_statement
  body: (block) @loop.inner)

(foreach_statement) @loop.outer

(foreach_statement
  body: (block) @loop.inner)

(select_statement) @block.outer

(return_statement) @return.outer

(return_statement
  value: (_) @return.inner)

(comment) @comment.outer

[
  (typedef_statement)
  (virtual_edge_declaration)
  (accumulator_declaration)
  (variable_declaration)
  (file_declaration)
  (exception_declaration)
  (vertex_set_declaration)
  (assignment_statement)
  (expression_statement)
  (print_statement)
  (insert_statement)
  (delete_statement)
  (update_statement)
  (raise_statement)
  (load_statement)
  (define_filename_statement)
] @statement.outer
