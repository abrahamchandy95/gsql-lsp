(query_definition
  name: (_) @name) @definition.function

(opencypher_query_definition
  name: (identifier) @name) @definition.function

(loading_job_definition
  name: (identifier) @name) @definition.function

(schema_change_job_definition
  name: (identifier) @name) @definition.function

(vertex_definition
  name: (identifier) @name) @definition.class

(edge_definition
  name: (identifier) @name) @definition.class

(typedef_statement
  name: (identifier) @name) @definition.class

(virtual_edge_declaration
  name: (identifier) @name) @definition.class

(graph_definition
  name: (identifier) @name) @definition.module

(call_expression
  function: (identifier) @name) @reference.call

(call_expression
  function: (member_expression
    property: (identifier) @name)) @reference.call

(run_query_statement
  query: (_) @name) @reference.call

(install_query_statement
  query: (_) @name) @reference.call
