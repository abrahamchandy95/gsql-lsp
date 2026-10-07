/**
 * @file TigerGraph GSQL grammar for tree-sitter
 * @author gsql-lsp contributors
 * @license MIT
 *
 * Covers the GSQL shell/DDL commands (schema, graphs, jobs, users), loading
 * jobs, schema change jobs and the query language (accumulators, SELECT
 * blocks in classic, pattern-matching and openCypher-style syntax, control
 * flow, DML and output statements).
 *
 * GSQL keywords are case-insensitive, so every keyword is matched with a
 * case-insensitive regex and exposed as an anonymous node named after its
 * upper-case spelling (e.g. `select` produces a `"SELECT"` node).
 */

/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

const PREC = {
  OR: 1,
  AND: 2,
  NOT: 3,
  COMPARE: 4,
  UNION: 5,
  INTERSECT: 6,
  BIT_OR: 7,
  BIT_XOR: 8,
  BIT_AND: 9,
  SHIFT: 10,
  ADD: 11,
  MUL: 12,
  UNARY: 13,
  POSTFIX: 14,
};

/**
 * A case-insensitive keyword, exposed as an anonymous node named `WORD`.
 * @param {string} word
 */
function kw(word) {
  return alias(new RegExp(word.toLowerCase(), 'i'), word.toUpperCase());
}

/** @param {RuleOrLiteral} rule */
function commaSep1(rule) {
  return seq(rule, repeat(seq(',', rule)));
}

/** @param {RuleOrLiteral} rule */
function commaSep(rule) {
  return optional(commaSep1(rule));
}

/** @param {RuleOrLiteral} rule */
function pipeSep1(rule) {
  return seq(rule, repeat(seq('|', rule)));
}

/**
 * The parenthesised-or-bare version argument of `API` and `SYNTAX`.
 * @param {GrammarSymbols<string>} $
 */
function versionArgument($) {
  const version = field('version', choice($.string, $.identifier));
  return choice(seq('(', version, ')'), version);
}

/**
 * IF/ELSE IF/ELSE/END, parameterised by the statement list used inside the
 * branches (semicolon-terminated in query bodies, comma-separated in ACCUM).
 * @param {GrammarSymbols<string>} $
 * @param {RuleOrLiteral} block
 * @param {RuleOrLiteral} elseIf
 * @param {RuleOrLiteral} elseClause
 */
function ifStatement($, block, elseIf, elseClause) {
  const head = [
    kw('IF'),
    field('condition', $._expression),
    kw('THEN'),
    optional(field('consequence', block)),
  ];
  return choice(
    seq(...head, optional(field('alternative', elseClause)), kw('END')),
    // GSQL also reads `ELSE IF` as `ELSE` followed by a nested IF, which has
    // its own END: `IF a THEN .. ELSE IF b THEN .. ELSE .. END END`. The
    // clause reading keeps long ELSE IF ladders cheap to parse; the ENDs of
    // nested IFs are taken after it, only when nothing else needs them (and
    // only after an ELSE IF: any other extra END is an error).
    seq(
      ...head,
      repeat1(field('alternative', elseIf)),
      optional(field('alternative', elseClause)),
      kw('END'),
      repeat(prec.dynamic(-1, seq(optional(';'), field('nested_end', kw('END'))))),
    ),
  );
}

/**
 * @param {GrammarSymbols<string>} $
 * @param {RuleOrLiteral} block
 */
function elseIfClause($, block) {
  // `ELSE IF` continues the same IF (one END), like GSQL itself reads it;
  // prefer that over an ELSE branch that starts with a nested IF.
  return prec.dynamic(1, seq(
    choice($._else_if, seq(kw('ELSE'), kw('IF'))),
    field('condition', $._expression),
    kw('THEN'),
    optional(field('consequence', block)),
  ));
}

/**
 * @param {GrammarSymbols<string>} $
 * @param {RuleOrLiteral} whenClause
 * @param {RuleOrLiteral} elseClause
 */
function caseStatement($, whenClause, elseClause) {
  return seq(
    kw('CASE'),
    optional(field('value', $._expression)),
    repeat1(whenClause),
    optional(elseClause),
    kw('END'),
  );
}

/**
 * @param {GrammarSymbols<string>} $
 * @param {RuleOrLiteral} block
 */
function whenClause($, block) {
  return seq(
    kw('WHEN'),
    field('condition', $._expression),
    kw('THEN'),
    optional(field('body', block)),
  );
}

/**
 * @param {GrammarSymbols<string>} $
 * @param {RuleOrLiteral} block
 */
function whileStatement($, block) {
  return seq(
    kw('WHILE'),
    field('condition', $._expression),
    optional(seq(kw('LIMIT'), field('limit', $._expression))),
    kw('DO'),
    optional(field('body', block)),
    kw('END'),
  );
}

/**
 * @param {GrammarSymbols<string>} $
 * @param {RuleOrLiteral} block
 */
function foreachStatement($, block) {
  return seq(
    kw('FOREACH'),
    field('variable', choice($.identifier, $.foreach_variables)),
    choice(kw('IN'), ':'),
    field('collection', $._expression),
    kw('DO'),
    optional(field('body', block)),
    kw('END'),
  );
}

export default grammar({
  name: 'gsql',

  extras: $ => [/\s/, $.comment],

  word: $ => $.identifier,

  supertypes: $ => [
    $._top_level_statement,
    $._statement,
    $._expression,
    $._primary_expression,
    $._type,
  ],

  conflicts: $ => [
    // `FROM A, TO B | FROM C, TO D` vs `FROM A, TO B|C`
    [$._edge_pair_to],
    // `S (Person) = ...` vs the call `S(Person)`
    [$._primary_expression, $.vertex_set_declaration],
    // `ELSE IF` vs an ELSE branch whose first statement is an IF, and an
    // ELSE branch that is just that IF
    [$.if_statement, $.else_if_clause],
    [$.dml_if_statement, $.dml_else_if_clause],
    // the END after an IF: its own extra END (nested IF) or an outer construct's
    [$.if_statement],
    [$.dml_if_statement],
    [$._statement, $._nested_if],
    [$._dml_statement, $._dml_nested_if],
    // `(k -> v, ...)`: a map literal, or a pair for a MapAccum or GroupByAccum
    [$._tuple_head, $.map_entry],
    // `HeapAccum<T>(k, score DESC)`: `k` is the capacity or a sort key
    [$._primary_expression, $.sort_key],
    // `POST-ACCUM (t) ...` vs a statement starting with `(t)`
    [$._primary_expression, $.post_accum_clause],
    // `RUN QUERY -queue q1 name()` vs `RUN QUERY -av name()`: an option
    // takes the following name as its value only if the command still parses
    [$.command_option],
    // `INSTALL QUERY list` vs `INSTALL QUERY` and a `LIST WORKLOAD QUEUE` line
    [$.install_query_statement],
    // `SHOW LOADING STATUS list` followed by a `LIST WORKLOAD QUEUE` line
    [$.show_statement],
    [$.shell_command],
    // `TUPLE<a list>` (type `a`, field `list`) vs `TUPLE<a LIST<INT>>` (field `a`)
    [$._type, $.tuple_field],
    [$._type, $._soft_name],
    // `FILE f ("x")` vs a declaration of type `file` (a tuple of that name)
    [$._soft_keyword, $.file_declaration],
    // `UPDATE s FROM ...` vs a declaration of type `update`
    [$._soft_keyword, $.update_statement],
  ],

  rules: {
    source_file: $ => repeat(choice($._top_level_statement, ';')),

    _top_level_statement: $ => choice(
      $.vertex_definition,
      $.edge_definition,
      $.graph_definition,
      $.typedef_statement,
      $.query_definition,
      $.opencypher_query_definition,
      $.interpret_query_statement,
      $.loading_job_definition,
      $.schema_change_job_definition,
      $.data_source_definition,
      $.package_definition,
      $.use_statement,
      $.install_query_statement,
      $.run_query_statement,
      $.run_job_statement,
      $.drop_statement,
      $.show_statement,
      $.security_statement,
      $.grant_statement,
      $.revoke_statement,
      $.data_source_grant_statement,
      $.description_statement,
      $.row_policy_statement,
      $.install_function_statement,
      $.shell_command,
      $.stub_declaration,
    ),

    // ------------------------------------------------------------------
    // Stubs: the declarations of the built-ins, in the reference file that
    // "go to definition" opens (the equivalent of Python's `.pyi` files).
    // ------------------------------------------------------------------

    stub_declaration: $ => seq(
      kw('BUILTIN'),
      choice(
        $.stub_function,
        $.stub_object,
        seq(kw('CONSTANT'), field('name', $.identifier), ';'),
        seq(choice(kw('TYPE'), kw('KEYWORD')), field('name', $.stub_word), ';'),
      ),
    ),

    stub_function: $ => seq(kw('FUNCTION'), field('name', $.identifier), $.stub_signature, ';'),

    // `BUILTIN OBJECT SumAccum<INT | STRING> { METHOD ...; }`: a type with methods.
    stub_object: $ => seq(
      kw('OBJECT'),
      field('name', $.identifier),
      optional(field('syntax', $.stub_syntax)),
      '{',
      repeat($.stub_method),
      '}',
    ),

    stub_method: $ => seq(
      choice(kw('METHOD'), kw('MUTATOR')),
      field('name', $.identifier),
      $.stub_signature,
      ';',
    ),

    stub_signature: $ => seq(
      '(',
      optional(field('parameters', $.stub_text)),
      ')',
      optional(seq('->', field('returns', $.stub_text))),
    ),

    // Free text: the parameters of a signature (one level of parentheses) or its result.
    stub_text: _ => token(prec(-1, /([^(){};\n]|\([^(){};\n]*\))+/)),

    stub_syntax: _ => token(prec(-1, /[^{};\n]+/)),

    // A keyword or type name as the reference lists it: `ELSE IF`, `POST-ACCUM`.
    stub_word: _ => token(/[A-Za-z_][A-Za-z0-9_-]*( [A-Za-z_][A-Za-z0-9_]*)*/),

    // ------------------------------------------------------------------
    // Schema definition (DDL)
    // ------------------------------------------------------------------

    vertex_definition: $ => seq(
      choice(kw('CREATE'), kw('ADD')),
      kw('VERTEX'),
      field('name', $.identifier),
      field('attributes', $.vertex_attribute_list),
      optional($.with_clause),
    ),

    vertex_attribute_list: $ => seq(
      '(',
      commaSep(choice(
        $.primary_id_definition,
        $.attribute_definition,
        $.primary_key_constraint,
      )),
      ')',
    ),

    primary_id_definition: $ => seq(
      kw('PRIMARY_ID'),
      field('name', $.identifier),
      field('type', $._attribute_type),
      optional($.default_value),
    ),

    attribute_definition: $ => seq(
      field('name', $.identifier),
      field('type', $._attribute_type),
      optional(field('primary_key', $.primary_key)),
      optional(kw('NULLABLE')),
      optional($.default_value),
    ),

    primary_key: _ => seq(kw('PRIMARY'), kw('KEY')),

    primary_key_constraint: $ => seq(
      kw('PRIMARY'),
      kw('KEY'),
      '(',
      commaSep1(field('attribute', $.identifier)),
      ')',
    ),

    default_value: $ => seq(kw('DEFAULT'), field('value', $._expression)),

    _attribute_type: $ => choice(
      $.primitive_type,
      $.collection_type,
      alias($.identifier, $.type_identifier),
    ),

    edge_definition: $ => seq(
      choice(kw('CREATE'), kw('ADD')),
      optional(field('direction', choice(kw('DIRECTED'), kw('UNDIRECTED')))),
      kw('EDGE'),
      field('name', $.identifier),
      field('attributes', $.edge_attribute_list),
      optional($.with_clause),
    ),

    edge_attribute_list: $ => seq(
      '(',
      pipeSep1($.edge_pair),
      repeat(seq(',', choice($.attribute_definition, $.discriminator))),
      ')',
    ),

    edge_pair: $ => seq(
      kw('FROM'),
      $._edge_pair_from,
      ',',
      kw('TO'),
      $._edge_pair_to,
    ),

    _edge_pair_from: $ => choice(
      field('from', alias('*', $.wildcard)),
      pipeSep1(field('from', $.identifier)),
    ),

    _edge_pair_to: $ => choice(
      field('to', alias('*', $.wildcard)),
      pipeSep1(field('to', $.identifier)),
    ),

    discriminator: $ => seq(
      kw('DISCRIMINATOR'),
      '(',
      commaSep1(choice($.attribute_definition, $.identifier)),
      ')',
    ),

    with_clause: $ => seq(kw('WITH'), commaSep1($.option_assignment)),

    option_assignment: $ => seq(
      field('key', $.identifier),
      '=',
      field('value', $._option_value),
    ),

    _option_value: $ => choice(
      $.string,
      $.integer,
      $.float,
      $.boolean,
      $.identifier,
    ),

    graph_definition: $ => seq(
      kw('CREATE'),
      kw('GRAPH'),
      field('name', $.identifier),
      choice(
        seq(
          '(',
          optional(choice(
            alias('*', $.wildcard),
            commaSep1(field('member', $.identifier)),
          )),
          ')',
        ),
        // Tag-based graph: `AS base (v1:tag1&tag2, e1)` or `AS base:tag`.
        seq(
          kw('AS'),
          field('base', $.identifier),
          choice(
            seq('(', commaSep1(seq(field('member', $.identifier), optional(seq(':', $.tag_expression)))), ')'),
            seq(':', $.tag_expression),
          ),
        ),
      ),
      optional(choice($.with_clause, seq(kw('WITH'), kw('ADMIN'), field('admin', $.identifier)))),
    ),

    tag_expression: $ => seq(field('tag', $.identifier), repeat(seq('&', field('tag', $.identifier)))),

    typedef_statement: $ => seq(
      kw('TYPEDEF'),
      choice(
        seq(kw('TUPLE'), '<', commaSep1($.tuple_field), '>'),
        // `TYPEDEF HeapAccum<Rec>(10, score DESC) Top_Heap`
        field('type', $.accumulator_type),
      ),
      field('name', $._name),
    ),

    tuple_field: $ => choice(
      seq(
        field('type', $._type),
        optional($.type_size),
        field('name', choice($.identifier, $._soft_name)),
      ),
      seq(
        field('name', choice($.identifier, $._soft_name)),
        field('type', choice(
          $.primitive_type,
          $.vertex_type,
          $.edge_type,
          $.collection_type,
        )),
        optional($.type_size),
      ),
    ),

    type_size: $ => seq('(', $.integer, ')'),

    // ------------------------------------------------------------------
    // Types
    // ------------------------------------------------------------------

    _type: $ => choice(
      $.primitive_type,
      $.vertex_type,
      $.edge_type,
      $.collection_type,
      $.accumulator_type,
      $.tuple_type,
      alias($.identifier, $.type_identifier),
      alias($._soft_keyword, $.type_identifier),
    ),

    primitive_type: $ => choice(
      kw('INT'),
      kw('UINT'),
      kw('FLOAT'),
      kw('DOUBLE'),
      kw('BOOL'),
      kw('DATETIME'),
      seq(kw('STRING'), optional(kw('COMPRESS'))),
      kw('JSONOBJECT'),
      kw('JSONARRAY'),
      // `FIXED_BINARY(8)`: a binary value of 8 bytes.
      seq(kw('FIXED_BINARY'), '(', $.integer, ')'),
    ),

    vertex_type: $ => seq(
      kw('VERTEX'),
      optional(seq('<', field('type', $.identifier), '>')),
    ),

    edge_type: $ => seq(
      kw('EDGE'),
      optional(seq('<', field('type', $.identifier), '>')),
    ),

    // Anonymous tuple: `TUPLE<INT, STRING>`.
    tuple_type: $ => seq(kw('TUPLE'), '<', commaSep1(choice($.tuple_field, $._type)), '>'),

    collection_type: $ => choice(
      seq(
        field('kind', choice(kw('LIST'), kw('SET'), kw('BAG'))),
        '<',
        field('element', $._type),
        '>',
      ),
      seq(
        field('kind', kw('MAP')),
        '<',
        field('key', $._type),
        ',',
        field('value', $._type),
        '>',
      ),
    ),

    accumulator_type: $ => prec.right(seq(
      field('kind', $.accumulator_kind),
      optional(seq(
        '<',
        // `BitwiseOrAccum<128>` takes a bit width.
        commaSep1(field('argument', choice($._type, $.group_by_field, $.integer))),
        '>',
      )),
      optional(field('heap', $.heap_options)),
    )),

    accumulator_kind: _ =>
      /(sum|max|min|avg|or|and|bitwiseor|bitwiseand|list|set|bag|map|heap|groupby|array|deviation|deviationp)accum/i,

    group_by_field: $ => seq(field('type', $._type), field('name', $.identifier)),

    heap_options: $ => seq(
      '(',
      choice(
        // The capacity may be left out; a leading name without ASC/DESC
        // followed by sort keys is read as the capacity.
        prec.dynamic(1, seq(field('capacity', $._expression), repeat1(seq(',', $.sort_key)))),
        commaSep1($.sort_key),
      ),
      ')',
    ),

    sort_key: $ => seq(
      field('name', $.identifier),
      optional(field('order', choice(kw('ASC'), kw('DESC')))),
    ),

    // ------------------------------------------------------------------
    // Queries
    // ------------------------------------------------------------------

    query_definition: $ => seq(
      kw('CREATE'),
      optional(seq(kw('OR'), kw('REPLACE'))),
      optional(field('modifier', choice(kw('DISTRIBUTED'), kw('TEMPLATE')))),
      choice(kw('QUERY'), kw('FUNCTION')),
      field('name', choice($.identifier, $.qualified_identifier)),
      field('parameters', $.parameter_list),
      repeat($._query_option),
      field('body', $.query_body),
    ),

    opencypher_query_definition: $ => seq(
      kw('CREATE'),
      optional(seq(kw('OR'), kw('REPLACE'))),
      optional(field('modifier', kw('DISTRIBUTED'))),
      kw('OPENCYPHER'),
      kw('QUERY'),
      field('name', $.identifier),
      field('parameters', $.parameter_list),
      repeat($._query_option),
      field('body', $.opencypher_body),
    ),

    interpret_query_statement: $ => seq(
      kw('INTERPRET'),
      choice(
        seq(
          kw('QUERY'),
          field('parameters', $.parameter_list),
          repeat($._query_option),
          field('body', $.query_body),
        ),
        seq(
          kw('QUERY'),
          repeat($.command_option),
          field('name', $.identifier),
          field('arguments', $.argument_list),
          repeat($.command_option),
        ),
        seq(
          kw('OPENCYPHER'),
          kw('QUERY'),
          field('parameters', $.parameter_list),
          repeat($._query_option),
          field('body', $.opencypher_body),
        ),
      ),
    ),

    _query_option: $ => choice(
      $.for_graph_clause,
      $.returns_clause,
      $.api_clause,
      $.syntax_clause,
    ),

    for_graph_clause: $ => seq(kw('FOR'), kw('GRAPH'), field('graph', $.identifier)),

    returns_clause: $ => seq(
      kw('RETURNS'),
      choice(seq('(', field('type', $._type), ')'), field('type', $._type)),
    ),

    api_clause: $ => seq(kw('API'), versionArgument($)),

    syntax_clause: $ => seq(kw('SYNTAX'), versionArgument($)),

    parameter_list: $ => seq('(', commaSep($.parameter), ')'),

    parameter: $ => seq(
      field('type', choice($._type, $.file_type)),
      field('name', $.identifier),
      optional(seq('=', field('default', $._expression))),
    ),

    file_type: _ => prec(1, kw('FILE')),

    query_body: $ => seq(
      '{',
      repeat(seq(choice($._statement, $.exception_declaration), ';')),
      '}',
    ),

    opencypher_body: $ => seq('{', optional($.cypher_text), '}'),

    // Raw openCypher text, kept balanced on braces so the body can be
    // handed to a Cypher parser through an injection. Strings, backtick
    // names and comments are single tokens, so a brace inside them does not
    // count; a quote that never closes on its line is plain text.
    cypher_text: $ => repeat1(choice(
      token(prec(1, /[^{}"'`\/]+/)),
      token(prec(1, choice(
        /"([^"\\\r\n]|\\[^\r\n])*"/,
        /'([^'\\\r\n]|\\[^\r\n])*'/,
        /`[^`]*`/,
        /\/\/[^\r\n]*/,
        /\/\*[^*]*\*+([^/*][^*]*\*+)*\//,
      ))),
      token(prec(0, /["'`\/]/)),
      seq('{', optional($.cypher_text), '}'),
    )),

    // ------------------------------------------------------------------
    // Statements
    // ------------------------------------------------------------------

    _statement: $ => choice(
      $.typedef_statement,
      $.virtual_edge_declaration,
      $.accumulator_declaration,
      $.variable_declaration,
      $.file_declaration,
      $.vertex_set_declaration,
      $.assignment_statement,
      $.select_statement,
      $.expression_statement,
      $.if_statement,
      $.case_statement,
      $.while_statement,
      $.foreach_statement,
      $.break_statement,
      $.continue_statement,
      $.return_statement,
      $.print_statement,
      $.insert_statement,
      $.delete_statement,
      $.update_statement,
      $.raise_statement,
      $.try_statement,
    ),

    block: $ => repeat1(seq($._statement, ';')),

    // Statements allowed inside ACCUM / POST-ACCUM / UPDATE ... SET, which
    // are comma-separated rather than semicolon-terminated.
    _dml_statement: $ => choice(
      alias($.dml_variable_declaration, $.variable_declaration),
      alias($.dml_assignment_statement, $.assignment_statement),
      $.expression_statement,
      alias($.dml_if_statement, $.if_statement),
      alias($.dml_case_statement, $.case_statement),
      alias($.dml_while_statement, $.while_statement),
      alias($.dml_foreach_statement, $.foreach_statement),
      $.break_statement,
      $.continue_statement,
      $.insert_statement,
      alias($.dml_delete_statement, $.delete_statement),
      $.raise_statement,
    ),

    dml_block: $ => commaSep1($._dml_statement),

    accumulator_declaration: $ => seq(
      optional(kw('STATIC')),
      // A TYPEDEF'd accumulator is named by its type identifier.
      field('type', choice($.accumulator_type, alias($.identifier, $.type_identifier))),
      // `SumAccum<INT> EDGE @weight` attaches the accumulator to edges.
      optional(kw('EDGE')),
      commaSep1($.accumulator_declarator),
    ),

    accumulator_declarator: $ => seq(
      field('name', choice($.global_accumulator, $.local_accumulator)),
      repeat($.array_dimension),
      optional(seq('=', field('value', $._expression))),
    ),

    array_dimension: $ => seq('[', optional($._expression), ']'),

    variable_declaration: $ => seq(
      field('type', $._type),
      commaSep1($.variable_declarator),
    ),

    // Inside ACCUM the comma separates statements, so only one declarator.
    dml_variable_declaration: $ => seq(field('type', $._type), $.variable_declarator),

    variable_declarator: $ => seq(
      field('name', $.identifier),
      optional(seq('=', field('value', $._expression))),
    ),

    file_declaration: $ => seq(
      kw('FILE'),
      field('name', $.identifier),
      '(',
      field('path', $._expression),
      // Octal permission code, e.g. `"764"`.
      optional(seq(',', field('permission', $._expression))),
      ')',
    ),

    virtual_edge_declaration: $ => seq(
      kw('CREATE'),
      optional(field('direction', choice(kw('DIRECTED'), kw('UNDIRECTED')))),
      kw('VIRTUAL'),
      kw('EDGE'),
      field('name', $.identifier),
      field('attributes', $.edge_attribute_list),
    ),

    exception_declaration: $ => seq(
      kw('EXCEPTION'),
      field('name', $.identifier),
      '(',
      field('code', $._expression),
      ')',
    ),

    vertex_set_declaration: $ => seq(
      field('name', $.identifier),
      '(',
      field('type', choice($.identifier, alias(kw('ANY'), $.any), $.wildcard)),
      ')',
      '=',
      field('value', $._assignment_value),
    ),

    assignment_statement: $ => seq(
      field('left', $._assignable),
      field('operator', choice('=', '+=')),
      field('right', $._assignment_value),
    ),

    dml_assignment_statement: $ => seq(
      field('left', $._assignable),
      field('operator', choice('=', '+=')),
      field('right', $._expression),
    ),

    _assignable: $ => choice(
      $.identifier,
      $._soft_name,
      $.global_accumulator,
      $.local_accumulator,
      $.member_expression,
      $.subscript_expression,
    ),

    _assignment_value: $ => choice($._expression, $.select_statement),

    expression_statement: $ => $.call_expression,

    if_statement: $ => ifStatement($, $.block, $.else_if_clause, $.else_clause),

    else_if_clause: $ => elseIfClause($, $.block),

    else_clause: $ => seq(kw('ELSE'), optional(field('body', choice(
      $.block,
      // An ELSE branch that is just a nested IF. Preferring it settles the
      // ties of `ELSE IF` (see else_if_clause): when the ENDs fit both ways,
      // an `ELSE IF` early in a block is the IF's own clause, and a nested IF
      // is the whole ELSE branch, not the start of a longer one.
      prec.dynamic(1, alias($._nested_if, $.block)),
    )))),

    _nested_if: $ => seq($.if_statement, ';'),

    dml_if_statement: $ => ifStatement(
      $,
      alias($.dml_block, $.block),
      alias($.dml_else_if_clause, $.else_if_clause),
      alias($.dml_else_clause, $.else_clause),
    ),

    dml_else_if_clause: $ => elseIfClause($, alias($.dml_block, $.block)),

    dml_else_clause: $ => seq(
      kw('ELSE'),
      optional(field('body', choice(
        alias($.dml_block, $.block),
        prec.dynamic(1, alias($._dml_nested_if, $.block)),
      ))),
    ),

    _dml_nested_if: $ => alias($.dml_if_statement, $.if_statement),

    case_statement: $ => caseStatement($, $.when_clause, $.else_clause),

    when_clause: $ => whenClause($, $.block),

    dml_case_statement: $ => caseStatement(
      $,
      alias($.dml_when_clause, $.when_clause),
      alias($.dml_else_clause, $.else_clause),
    ),

    dml_when_clause: $ => whenClause($, alias($.dml_block, $.block)),

    while_statement: $ => whileStatement($, $.block),

    dml_while_statement: $ => whileStatement($, alias($.dml_block, $.block)),

    foreach_statement: $ => foreachStatement($, $.block),

    dml_foreach_statement: $ => foreachStatement($, alias($.dml_block, $.block)),

    foreach_variables: $ => seq('(', commaSep1($.identifier), ')'),

    break_statement: _ => kw('BREAK'),

    continue_statement: _ => kw('CONTINUE'),

    return_statement: $ => seq(kw('RETURN'), optional(field('value', $._expression))),

    print_statement: $ => seq(
      kw('PRINT'),
      commaSep1(choice($._expression, $.aliased_expression)),
      // `WITH TAGS` (tag-based graphs, before 4.0) prints the tags too.
      optional(seq(kw('WITH'), choice(kw('VECTOR'), kw('TAGS')))),
      optional(seq(kw('WHERE'), field('filter', $._expression))),
      optional(seq(kw('TO_CSV'), field('file', $._expression))),
    ),

    aliased_expression: $ => seq(
      field('value', $._expression),
      kw('AS'),
      field('alias', $.identifier),
    ),

    insert_statement: $ => seq(
      kw('INSERT'),
      kw('INTO'),
      // `INSERT INTO EDGE type_param VALUES (...)` names the type through a parameter.
      optional(field('kind', choice(kw('VERTEX'), kw('EDGE')))),
      field('target', $.identifier),
      optional(field('columns', $.insert_columns)),
      kw('VALUES'),
      field('values', $.value_list),
    ),

    insert_columns: $ => seq(
      '(',
      commaSep1(choice(kw('PRIMARY_ID'), kw('FROM'), kw('TO'), $.discriminator, $.identifier)),
      ')',
    ),

    value_list: $ => seq('(', commaSep($._value), ')'),

    _value: $ => choice($.wildcard, $._expression, $.typed_value),

    // A value followed by the vertex type of an edge endpoint, e.g.
    // `VALUES ($0 Person, $1 $2, ...)` or `VALUES (s Person, t Person)`.
    typed_value: $ => seq(
      field('value', $._expression),
      field('type', choice($.identifier, $.column_reference)),
    ),

    delete_statement: $ => seq(
      kw('DELETE'),
      field('alias', $.identifier),
      $.from_clause,
      optional($.where_clause),
    ),

    dml_delete_statement: $ => seq(kw('DELETE'), '(', field('alias', $.identifier), ')'),

    update_statement: $ => seq(
      kw('UPDATE'),
      field('alias', $.identifier),
      $.from_clause,
      kw('SET'),
      field('body', alias($.dml_block, $.block)),
      optional($.where_clause),
    ),

    raise_statement: $ => seq(
      kw('RAISE'),
      field('exception', $.identifier),
      optional(field('arguments', $.argument_list)),
    ),

    try_statement: $ => seq(
      kw('TRY'),
      optional(field('body', $.block)),
      kw('EXCEPTION'),
      repeat1($.exception_handler),
      optional($.else_clause),
      kw('END'),
    ),

    exception_handler: $ => seq(
      kw('WHEN'),
      field('exception', $.identifier),
      kw('THEN'),
      optional(field('body', $.block)),
    ),

    // ------------------------------------------------------------------
    // SELECT
    // ------------------------------------------------------------------

    select_statement: $ => seq(
      kw('SELECT'),
      optional(kw('DISTINCT')),
      commaSep1(field('result', choice($._expression, $.aliased_expression))),
      optional($.into_clause),
      $.from_clause,
      optional($.sample_clause),
      optional($.where_clause),
      // PER may also scope POST-ACCUM clauses without an ACCUM.
      optional(choice($.accum_clause, $.per_clause)),
      repeat($.post_accum_clause),
      optional($.group_by_clause),
      optional($.having_clause),
      optional($.order_by_clause),
      optional($.limit_clause),
    ),

    into_clause: $ => seq(kw('INTO'), commaSep1(field('table', $.identifier))),

    from_clause: $ => seq(kw('FROM'), commaSep1(choice($.path_pattern, $.cypher_pattern))),

    // Classic (`Start:s -(Knows:e)-> Person:t`) and pattern-matching
    // (`Person:p -(Knows>)- Person -(<Likes:e)- Post:x`) syntax.
    path_pattern: $ => choice(
      seq($.vertex_pattern, optional($._edge_steps)),
      // The source vertex set may be left out: `FROM -(Likes>:e)- :msg`.
      $._edge_steps,
    ),

    // An intermediate vertex may be left out (`-(E1)- -(E2)- :x`), but only
    // when another step follows, so a vertex type at the end of a pattern
    // (such as `Order`) is never mistaken for a keyword.
    _edge_steps: $ => choice(
      seq($.edge_step, optional($._edge_steps)),
      seq(alias($._edge_hop, $.edge_step), $._edge_steps),
    ),

    vertex_pattern: $ => choice(
      seq(
        field('type', choice(
          $.identifier,
          $.global_accumulator,
          alias(kw('ANY'), $.any),
          $.wildcard,
          $.vertex_type_alternation,
        )),
        optional(seq(':', field('alias', $.identifier))),
      ),
      seq(':', field('alias', $.identifier)),
    ),

    vertex_type_alternation: $ => seq(
      '(',
      $._vertex_type_atom,
      repeat1(seq('|', $._vertex_type_atom)),
      ')',
    ),

    _vertex_type_atom: $ => choice(
      $.identifier,
      $.global_accumulator,
      alias(kw('ANY'), $.any),
      $.wildcard,
    ),

    edge_step: $ => seq($._edge_hop, $.vertex_pattern),

    _edge_hop: $ => seq(
      '-',
      '(',
      optional($.edge_pattern),
      ')',
      choice('-', '->'),
    ),

    edge_pattern: $ => choice(
      seq(
        field('type', choice(
          $.edge_atom,
          $.edge_alternation,
          $.edge_repetition,
          $.edge_group,
          $.edge_sequence,
        )),
        optional(seq(':', field('alias', $.identifier))),
      ),
      seq(':', field('alias', $.identifier)),
    ),

    edge_atom: $ => seq(
      optional(field('direction', '<')),
      field('name', choice(
        $.identifier,
        $.global_accumulator,
        alias(kw('ANY'), $.any),
        $.wildcard,
      )),
      optional(field('direction', '>')),
    ),

    edge_alternation: $ => seq(
      choice($.edge_atom, $.edge_group, $.edge_sequence),
      repeat1(seq('|', choice($.edge_atom, $.edge_group, $.edge_sequence))),
    ),

    // Multi-hop path: `-(Is_Located_In>.Is_Part_Of>)-`.
    edge_sequence: $ => seq(
      choice($.edge_atom, $.edge_group, $.edge_repetition),
      repeat1(seq('.', choice($.edge_atom, $.edge_group, $.edge_repetition))),
    ),

    edge_group: $ => seq(
      '(',
      choice($.edge_atom, $.edge_alternation, $.edge_repetition, $.edge_group, $.edge_sequence),
      ')',
    ),

    edge_repetition: $ => seq(
      choice($.edge_atom, $.edge_group),
      '*',
      optional($.repetition_bounds),
    ),

    repetition_bounds: $ => choice(
      seq(field('min', $.integer), '..', optional(field('max', $.integer))),
      seq('..', field('max', $.integer)),
      field('exact', $.integer),
    ),

    // openCypher-style (GSQL V3) syntax: `(s:Person) -[e:Knows]-> (t:Person)`.
    cypher_pattern: $ => seq(
      $.node_pattern,
      repeat(seq($.relationship_pattern, $.node_pattern)),
    ),

    node_pattern: $ => seq(
      '(',
      optional(field('alias', $.identifier)),
      optional(seq(':', pipeSep1(field('type', choice($.identifier, $.global_accumulator))))),
      optional(field('properties', $.property_map)),
      ')',
    ),

    relationship_pattern: $ => choice(
      seq('-', optional($.relationship_detail), '->'),
      seq('<-', optional($.relationship_detail), '-'),
      seq('-', optional($.relationship_detail), '-'),
    ),

    relationship_detail: $ => seq(
      '[',
      optional(field('alias', $.identifier)),
      optional(seq(
        ':',
        optional(pipeSep1(field('type', choice($.identifier, $.global_accumulator, $.wildcard)))),
      )),
      optional(seq('*', optional($.repetition_bounds))),
      optional(field('properties', $.property_map)),
      ']',
    ),

    // `{name: "Adam"}` constrains attribute values.
    property_map: $ => seq('{', commaSep($.pair), '}'),

    sample_clause: $ => seq(
      kw('SAMPLE'),
      field('size', choice($.integer, $.float, $.identifier)),
      optional('%'),
      choice(kw('EDGE'), kw('TARGET')),
      optional(kw('PINNED')),
      kw('WHEN'),
      field('condition', $._expression),
    ),

    where_clause: $ => seq(kw('WHERE'), field('condition', $._expression)),

    accum_clause: $ => seq(
      optional($.per_clause),
      kw('ACCUM'),
      commaSep1($._dml_statement),
    ),

    per_clause: $ => seq(kw('PER'), '(', commaSep1($.identifier), ')'),

    post_accum_clause: $ => seq(
      choice(
        alias(/post-accum/i, 'POST-ACCUM'),
        alias(/post_accum/i, 'POST-ACCUM'),
      ),
      // `POST-ACCUM (t)` runs once per distinct binding of `t`.
      optional(seq('(', field('alias', $.identifier), ')')),
      commaSep1($._dml_statement),
    ),

    group_by_clause: $ => seq(kw('GROUP'), kw('BY'), commaSep1($._expression)),

    having_clause: $ => seq(kw('HAVING'), field('condition', $._expression)),

    order_by_clause: $ => seq(kw('ORDER'), kw('BY'), commaSep1($.order_item)),

    order_item: $ => seq(
      field('value', $._expression),
      optional(field('order', choice(kw('ASC'), kw('DESC')))),
    ),

    limit_clause: $ => seq(
      kw('LIMIT'),
      choice(
        seq(field('count', $._expression), optional(seq(kw('OFFSET'), field('offset', $._expression)))),
        // `LIMIT j, k` skips j results and keeps k.
        seq(field('offset', $._expression), ',', field('count', $._expression)),
      ),
    ),

    // ------------------------------------------------------------------
    // Expressions
    // ------------------------------------------------------------------

    _expression: $ => choice(
      $._primary_expression,
      $.unary_expression,
      $.binary_expression,
      $.in_expression,
      $.between_expression,
      $.like_expression,
      $.is_expression,
      $.case_expression,
      $.interval_expression,
    ),

    _primary_expression: $ => choice(
      $.identifier,
      $._soft_name,
      $.global_accumulator,
      $.local_accumulator,
      $.column_reference,
      $.integer,
      $.float,
      $.string,
      $.boolean,
      $.null,
      $.parenthesized_expression,
      $.tuple,
      $.key_value_pair,
      $.map_literal,
      $.list_literal,
      $.vertex_set_literal,
      $.type_wildcard,
      $.member_expression,
      $.call_expression,
      $.subscript_expression,
      $.range_expression,
    ),

    unary_expression: $ => choice(
      // `~` inverts the bits of a bitwise accumulator.
      prec(PREC.UNARY, seq(field('operator', choice('-', '~')), field('operand', $._expression))),
      prec(PREC.NOT, seq(field('operator', kw('NOT')), field('operand', $._expression))),
    ),

    binary_expression: $ => {
      /** @type {[number, RuleOrLiteral][]} */
      const table = [
        [PREC.OR, kw('OR')],
        [PREC.AND, kw('AND')],
        // `=` and `<>` compare in SYNTAX V3 queries.
        [PREC.COMPARE, choice('==', '!=', '<>', '=', '<', '<=', '>', '>=')],
        [PREC.UNION, choice(kw('UNION'), kw('MINUS'))],
        [PREC.INTERSECT, kw('INTERSECT')],
        [PREC.BIT_OR, '|'],
        [PREC.BIT_XOR, '^'],
        [PREC.BIT_AND, '&'],
        [PREC.SHIFT, choice('<<', '>>')],
        [PREC.ADD, choice('+', '-')],
        [PREC.MUL, choice('*', '/', '%')],
      ];
      return choice(...table.map(([precedence, operator]) => prec.left(precedence, seq(
        field('left', $._expression),
        field('operator', operator),
        field('right', $._expression),
      ))));
    },

    in_expression: $ => prec.left(PREC.COMPARE, seq(
      field('left', $._expression),
      optional(kw('NOT')),
      kw('IN'),
      field('right', $._expression),
    )),

    between_expression: $ => prec.left(PREC.COMPARE, seq(
      field('left', $._expression),
      optional(kw('NOT')),
      kw('BETWEEN'),
      field('low', $._expression),
      kw('AND'),
      field('high', $._expression),
    )),

    like_expression: $ => prec.left(PREC.COMPARE, seq(
      field('left', $._expression),
      optional(kw('NOT')),
      kw('LIKE'),
      field('pattern', $._expression),
      optional(seq(kw('ESCAPE'), field('escape', $._expression))),
    )),

    is_expression: $ => prec.left(PREC.COMPARE, seq(
      field('left', $._expression),
      kw('IS'),
      optional(kw('NOT')),
      field('right', choice($.null, kw('EMPTY'), kw('NUMERIC'))),
    )),

    case_expression: $ => seq(
      kw('CASE'),
      optional(field('value', $._expression)),
      repeat1(alias($.when_expression, $.when_clause)),
      optional(alias($.else_expression, $.else_clause)),
      kw('END'),
    ),

    when_expression: $ => seq(
      kw('WHEN'),
      field('condition', $._expression),
      kw('THEN'),
      field('result', $._expression),
    ),

    else_expression: $ => seq(kw('ELSE'), field('result', $._expression)),

    interval_expression: $ => seq(
      kw('INTERVAL'),
      field('value', $._expression),
      field('unit', choice(
        kw('YEAR'),
        kw('MONTH'),
        kw('DAY'),
        kw('HOUR'),
        kw('MINUTE'),
        kw('SECOND'),
      )),
    ),

    parenthesized_expression: $ => prec(1, seq('(', $._expression, ')')),

    // `(a, b, c)` -- set/bag literals, tuples of keys, vertex values.
    tuple: $ => seq($._tuple_head, ')'),

    // `(key -> value)` for MapAccum, `(k1, k2 -> a1, a2)` for GroupByAccum.
    key_value_pair: $ => seq($._tuple_head, '->', commaSep1($._expression), ')'),

    // `("a" -> 1, "b" -> 2)`: the value of a MAP attribute.
    map_literal: $ => seq('(', $.map_entry, repeat1(seq(',', $.map_entry)), ')'),

    map_entry: $ => seq(field('key', $._expression), '->', field('value', $._expression)),

    _tuple_head: $ => seq('(', commaSep1($._expression)),

    list_literal: $ => seq('[', commaSep($._expression), ']'),

    // Vertex-set seed: `{p}`, `{Person.*}`, `{ANY}`, `{@@set}`; also the
    // `{key: value}` options of functions such as vectorSearch.
    vertex_set_literal: $ => seq(
      '{',
      commaSep(choice($._expression, alias(kw('ANY'), $.any), $.wildcard, $.pair)),
      '}',
    ),

    // `{key: value}`; JSON arguments of RUN QUERY quote the key.
    pair: $ => seq(field('key', choice($.identifier, $.string)), ':', field('value', $._expression)),

    type_wildcard: $ => prec(PREC.POSTFIX, seq(
      field('type', $._primary_expression),
      '.',
      '*',
    )),

    member_expression: $ => prec(PREC.POSTFIX, seq(
      field('object', $._primary_expression),
      '.',
      field('property', choice($.identifier, $.local_accumulator)),
      optional(field('prime', "'")),
    )),

    call_expression: $ => prec(PREC.POSTFIX, seq(
      // `range(1, 10, 2)` is a function; `RANGE[1, 10]` a range expression.
      field('function', choice($._primary_expression, alias(kw('RANGE'), $.identifier))),
      field('arguments', $.argument_list),
    )),

    subscript_expression: $ => prec(PREC.POSTFIX, seq(
      field('object', $._primary_expression),
      '[',
      commaSep1(choice($._expression, $.aliased_expression)),
      ']',
    )),

    range_expression: $ => seq(
      kw('RANGE'),
      '[',
      field('start', $._expression),
      ',',
      field('end', $._expression),
      ']',
    ),

    argument_list: $ => seq(
      '(',
      optional(field('modifier', choice(
        kw('DISTINCT'),
        kw('LEADING'),
        kw('TRAILING'),
        kw('BOTH'),
      ))),
      // `_` passes a query its default value: `RUN QUERY q(21, _)`.
      commaSep(choice($._expression, alias('*', $.wildcard), $.wildcard, $.from_argument)),
      ')',
    ),

    // `trim(LEADING " " FROM s)`
    from_argument: $ => seq(optional($._expression), kw('FROM'), $._expression),

    qualified_identifier: $ => seq($.identifier, repeat1(seq('.', $.identifier))),

    // ------------------------------------------------------------------
    // Loading jobs
    // ------------------------------------------------------------------

    loading_job_definition: $ => seq(
      kw('CREATE'),
      optional(seq(kw('OR'), kw('REPLACE'))),
      kw('LOADING'),
      kw('JOB'),
      field('name', $._name),
      optional($.for_graph_clause),
      field('body', $.loading_job_body),
    ),

    loading_job_body: $ => seq(
      '{',
      repeat(seq(choice(
        $.define_filename_statement,
        $.define_header_statement,
        $.define_input_line_filter_statement,
        $.load_statement,
        $.loading_delete_statement,
      ), ';')),
      '}',
    ),

    define_filename_statement: $ => seq(
      kw('DEFINE'),
      kw('FILENAME'),
      field('name', $.identifier),
      optional(seq('=', field('path', choice($.string, $.identifier)))),
    ),

    define_header_statement: $ => seq(
      kw('DEFINE'),
      kw('HEADER'),
      field('name', $.identifier),
      '=',
      commaSep1($.string),
    ),

    define_input_line_filter_statement: $ => seq(
      kw('DEFINE'),
      kw('INPUT_LINE_FILTER'),
      field('name', $.identifier),
      '=',
      field('condition', $._expression),
    ),

    load_statement: $ => seq(
      kw('LOAD'),
      optional(choice(
        field('source', choice($.identifier, $.string)),
        seq(kw('TEMP_TABLE'), field('source', $.identifier)),
      )),
      commaSep1($.load_destination),
      optional($.tags_clause),
      optional($.using_clause),
    ),

    // Deprecated tag-based access control: `TAGS (public) BY OR`.
    tags_clause: $ => seq(
      kw('TAGS'),
      '(',
      commaSep1(field('tag', $.identifier)),
      ')',
      kw('BY'),
      optional(choice(kw('OR'), kw('OVERWRITE'))),
    ),

    load_destination: $ => seq(
      kw('TO'),
      choice(
        seq(field('kind', choice(kw('VERTEX'), kw('EDGE'))), field('target', $.identifier)),
        seq(
          field('kind', kw('TEMP_TABLE')),
          field('target', $.identifier),
          field('columns', $.column_list),
        ),
        seq(
          field('kind', kw('VECTOR')),
          kw('ATTRIBUTE'),
          field('attribute', $.identifier),
          kw('ON'),
          kw('VERTEX'),
          field('target', $.identifier),
        ),
      ),
      kw('VALUES'),
      field('values', $.value_list),
      optional($.where_clause),
      optional($.option_clause),
    ),

    column_list: $ => seq('(', commaSep1($.identifier), ')'),

    option_clause: $ => seq(kw('OPTION'), '(', commaSep($.option_assignment), ')'),

    using_clause: $ => seq(kw('USING'), commaSep1($.option_assignment)),

    loading_delete_statement: $ => seq(
      kw('DELETE'),
      choice(
        seq(
          field('kind', kw('VERTEX')),
          field('target', $.identifier),
          '(',
          kw('PRIMARY_ID'),
          $._value,
          ')',
        ),
        seq(
          field('kind', kw('EDGE')),
          field('target', choice($.identifier, alias('*', $.wildcard))),
          '(',
          kw('FROM'),
          $._value,
          // Without TO, every edge from the source vertex is deleted.
          optional(seq(',', kw('TO'), $._value)),
          repeat(seq(',', choice($.discriminator_values, $._value))),
          ')',
        ),
      ),
      optional(seq(kw('FROM'), field('source', choice($.identifier, $.string)))),
      optional($.where_clause),
    ),

    discriminator_values: $ => seq(kw('DISCRIMINATOR'), '(', commaSep1($._value), ')'),

    // ------------------------------------------------------------------
    // Schema change jobs
    // ------------------------------------------------------------------

    schema_change_job_definition: $ => seq(
      kw('CREATE'),
      optional(kw('GLOBAL')),
      kw('SCHEMA_CHANGE'),
      kw('JOB'),
      field('name', $._name),
      optional($.for_graph_clause),
      field('body', $.schema_change_body),
    ),

    schema_change_body: $ => seq(
      '{',
      repeat(seq(choice(
        $.vertex_definition,
        $.edge_definition,
        $.alter_type_statement,
        $.alter_graph_statement,
        $.add_to_graph_statement,
        $.drop_statement,
        $.tag_statement,
      ), ';')),
      '}',
    ),

    alter_type_statement: $ => seq(
      kw('ALTER'),
      field('kind', choice(kw('VERTEX'), kw('EDGE'))),
      field('name', $.identifier),
      choice(
        seq(
          kw('ADD'),
          choice(
            // `ALTER EDGE e ADD FROM (A) TO (B) ATTRIBUTE (...)` adds endpoint types.
            seq($._added_sources, optional($._added_targets), optional($._added_attributes)),
            seq($._added_targets, optional($._added_attributes)),
            $._added_attributes,
          ),
        ),
        seq(kw('DROP'), kw('ATTRIBUTE'), '(', commaSep1(field('attribute', $.identifier)), ')'),
        seq(
          kw('ADD'),
          kw('INDEX'),
          field('index', $.identifier),
          kw('ON'),
          '(',
          commaSep1(field('attribute', $.identifier)),
          ')',
        ),
        seq(kw('DROP'), kw('INDEX'), field('index', $.identifier)),
        seq(kw('ADD'), kw('PAIR'), '(', pipeSep1($.edge_pair), ')'),
        seq(kw('DROP'), kw('PAIR'), '(', pipeSep1($.edge_pair), ')'),
        seq(
          kw('ADD'),
          kw('VECTOR'),
          kw('ATTRIBUTE'),
          field('attribute', $.identifier),
          '(',
          commaSep($.option_assignment),
          ')',
        ),
        seq(kw('DROP'), kw('VECTOR'), kw('ATTRIBUTE'), field('attribute', $.identifier)),
        $.with_clause,
      ),
    ),

    _added_sources: $ => seq(kw('FROM'), '(', commaSep1(field('from', $.identifier)), ')'),

    _added_targets: $ => seq(kw('TO'), '(', commaSep1(field('to', $.identifier)), ')'),

    _added_attributes: $ => seq(kw('ATTRIBUTE'), '(', commaSep1($.attribute_definition), ')'),

    // Global schema change: `ADD VERTEX Person, Post TO GRAPH Social;`
    add_to_graph_statement: $ => seq(
      kw('ADD'),
      field('kind', choice(kw('VERTEX'), kw('EDGE'))),
      commaSep1(field('name', $.identifier)),
      kw('TO'),
      kw('GRAPH'),
      field('graph', $.identifier),
    ),

    alter_graph_statement: $ => seq(
      kw('ALTER'),
      kw('GRAPH'),
      field('name', $.identifier),
      choice(kw('ADD'), kw('DROP')),
      commaSep1(seq(
        optional(choice(kw('VERTEX'), kw('EDGE'))),
        field('member', $.identifier),
      )),
    ),

    tag_statement: $ => seq(
      kw('ADD'),
      kw('TAG'),
      field('name', $.identifier),
      optional(seq(kw('DESCRIPTION'), $.string)),
    ),

    // ------------------------------------------------------------------
    // Shell and administrative commands
    // ------------------------------------------------------------------

    data_source_definition: $ => seq(
      kw('CREATE'),
      kw('DATA_SOURCE'),
      optional(field('type', $.identifier)),
      field('name', $.identifier),
      optional(seq('=', field('config', choice($.string, $.identifier)))),
      optional($.for_graph_clause),
    ),

    package_definition: $ => seq(
      kw('CREATE'),
      kw('PACKAGE'),
      field('name', choice($.identifier, $.qualified_identifier)),
    ),

    use_statement: $ => prec.dynamic(1, seq(
      kw('USE'),
      choice(seq(kw('GRAPH'), field('graph', $.identifier)), kw('GLOBAL')),
    )),

    install_query_statement: $ => seq(
      kw('INSTALL'),
      kw('QUERY'),
      // Install options take no values; `INSTALL QUERY -OPTIMIZE` names no query.
      repeat(alias($._flag, $.command_option)),
      optional(choice(
        alias('*', $.wildcard),
        kw('ALL'),
        commaSep1(field('query', choice($.identifier, $._soft_name, $._command_name, $.qualified_identifier))),
      )),
    ),

    _flag: $ => $.option_flag,

    run_query_statement: $ => seq(
      kw('RUN'),
      kw('QUERY'),
      repeat($.command_option),
      field('query', choice($.identifier, $.qualified_identifier)),
      field('arguments', $.argument_list),
      // `-mode instruction`, `-gzip -o "out.gz"`
      repeat($.command_option),
    ),

    run_job_statement: $ => seq(
      kw('RUN'),
      optional(kw('GLOBAL')),
      choice(kw('LOADING'), kw('SCHEMA_CHANGE')),
      kw('JOB'),
      repeat($.command_option),
      field('job', $._name),
      repeat($.command_option),
      optional($.using_clause),
    ),

    // `-force`, `-n 1,100`, `-PROFILE BASIC`, `-o "out.gz"`
    command_option: $ => seq(
      $.option_flag,
      optional(field('value', choice(commaSep1(choice($.integer, $.line_end)), $.float, $.string, $.identifier))),
    ),

    // `-n 10,$`: from line 10 to the end of the file.
    line_end: _ => '$',

    option_flag: _ => /-[A-Za-z][A-Za-z0-9_]*/,

    drop_statement: $ => seq(
      kw('DROP'),
      choice(
        seq(
          field('kind', choice(kw('VERTEX'), kw('EDGE'), kw('TUPLE'))),
          commaSep1(field('name', $.identifier)),
          optional(seq(kw('FROM'), kw('GRAPH'), field('graph', $.identifier))),
        ),
        seq(field('kind', kw('GRAPH')), field('name', $.identifier), optional(kw('CASCADE'))),
        // `DROP FUNCTION lib1.func1`, `DROP FUNCTION lib1.*`
        seq(field('kind', kw('FUNCTION')), commaSep1(field('name', $.name_pattern))),
        seq(
          field('kind', choice(
            kw('QUERY'),
            kw('JOB'),
            kw('PACKAGE'),
            kw('DATA_SOURCE'),
          )),
          repeat($.command_option),
          choice(
            alias('*', $.wildcard),
            kw('ALL'),
            commaSep1(field('name', choice($.identifier, $._soft_name, $.qualified_identifier))),
          ),
        ),
        seq(
          field('kind', choice(kw('USER'), kw('ROLE'), kw('SECRET'), kw('GROUP'), kw('TAG'))),
          commaSep1(field('name', $.identifier)),
        ),
        kw('ALL'),
      ),
    ),

    show_statement: $ => prec.dynamic(1, seq(
      kw('SHOW'),
      choice(
        // `SHOW GRANTS TO ROLE r1, r2`
        seq(
          field('kind', kw('GRANTS')),
          choice(kw('TO'), kw('OF')),
          kw('ROLE'),
          commaSep1(field('name', $.identifier)),
        ),
        // `SHOW FUNCTION lib1.*`, `SHOW FUNCTION -r "regex"`
        seq(
          field('kind', kw('FUNCTION')),
          optional(alias($._flag, $.command_option)),
          optional(choice(alias('*', $.wildcard), $.string, commaSep1(field('name', $.name_pattern)))),
        ),
        seq(field('kind', choice(
        kw('VERTEX'),
        kw('EDGE'),
        kw('GRAPH'),
        kw('QUERY'),
        kw('JOB'),
        kw('PACKAGE'),
        kw('USER'),
        kw('ROLE'),
        kw('SECRET'),
        kw('TOKEN'),
        kw('DATA_SOURCE'),
        kw('TAG'),
        kw('PRIVILEGE'),
        kw('GROUP'),
        kw('SCHEMA'),
        kw('LOADING'),
        // `SHOW DEFAULT ROLES IN GRAPH g`, `SHOW PROXY USER u`, `SHOW WORKLOAD QUEUE q`
        kw('DEFAULT'),
        kw('PROXY'),
        kw('WORKLOAD'),
        // `SHOW ROW POLICY`
        kw('ROW'),
      )),
            repeat(choice($._command_argument, $._command_name))),
      ),
    )),

    // `GRANT DATA_SOURCE k1 TO GRAPH g1, g2`, `REVOKE DATA_SOURCE k1 FROM GRAPH g1`
    data_source_grant_statement: $ => seq(
      choice(
        seq(kw('GRANT'), kw('DATA_SOURCE'), field('name', $.identifier), kw('TO')),
        seq(kw('REVOKE'), kw('DATA_SOURCE'), field('name', $.identifier), kw('FROM')),
      ),
      kw('GRAPH'),
      commaSep1(field('graph', $.identifier)),
    ),

    // `UPDATE DESCRIPTION OF QUERY q "text"`, `SHOW DESCRIPTION OF QUERY_PARAM q.p`,
    // `DROP DESCRIPTION OF QUERY q1, q2 ON GRAPH g`
    description_statement: $ => prec.dynamic(1, seq(
      choice(kw('UPDATE'), kw('SHOW'), kw('DROP')),
      kw('DESCRIPTION'),
      kw('OF'),
      field('kind', choice(kw('QUERY'), kw('QUERY_PARAM'))),
      choice(alias('*', $.wildcard), commaSep1(field('name', $.name_pattern))),
      optional(seq(kw('ON'), kw('GRAPH'), field('graph', $.identifier))),
      optional(field('description', $.string)),
    )),

    // `ALTER VERTEX Person IN GLOBAL SET ROW POLICY lib.f ON (gender)`,
    // `ALTER VERTEX Person IN GRAPH g CLEAR ROW POLICY`
    row_policy_statement: $ => seq(
      kw('ALTER'),
      field('kind', choice(kw('VERTEX'), kw('EDGE'))),
      field('name', $.identifier),
      kw('IN'),
      choice(kw('GLOBAL'), seq(kw('GRAPH'), field('graph', $.identifier))),
      choice(
        seq(
          kw('SET'),
          kw('ROW'),
          kw('POLICY'),
          field('function', $.name_pattern),
          kw('ON'),
          '(',
          commaSep1(field('attribute', $.identifier)),
          ')',
        ),
        seq(kw('CLEAR'), kw('ROW'), kw('POLICY')),
      ),
    ),

    // `INSTALL FUNCTION lib1.func1`, `INSTALL FUNCTION ALL`
    install_function_statement: $ => seq(
      kw('INSTALL'),
      kw('FUNCTION'),
      repeat(alias($._flag, $.command_option)),
      optional(choice(
        alias('*', $.wildcard),
        kw('ALL'),
        commaSep1(field('function', $.name_pattern)),
      )),
    ),

    // A name or a dotted path that may end in a wildcard: `q1`, `lib1.func1`, `lib1.*`.
    name_pattern: $ => seq(
      $.identifier,
      repeat(seq('.', $.identifier)),
      optional(seq(optional('.'), alias('*', $.wildcard))),
    ),

    security_statement: $ => prec.right(seq(
      choice(kw('CREATE'), kw('ALTER')),
      field('kind', choice(
        kw('USER'),
        kw('ROLE'),
        kw('SECRET'),
        kw('GROUP'),
        kw('TOKEN'),
        kw('PASSWORD'),
      )),
      repeat($._command_argument),
    )),

    grant_statement: $ => prec.dynamic(1, seq(
      kw('GRANT'),
      choice($._privilege_spec, $._workload_queue),
      kw('TO'),
      $._grantees,
    )),

    revoke_statement: $ => seq(
      kw('REVOKE'),
      choice($._privilege_spec, $._workload_queue),
      kw('FROM'),
      $._grantees,
    ),

    _workload_queue: $ => seq(kw('WORKLOAD'), kw('QUEUE'), field('queue', $.identifier)),

    // `TO ROLE r1, r2`, `FROM USER u`, or just the names.
    _grantees: $ => seq(
      optional(choice(kw('ROLE'), kw('USER'), kw('GROUP'))),
      commaSep1(field('grantee', $.identifier)),
    ),

    _privilege_spec: $ => seq(
      // `GRANT DEFAULT READ ON ALL QUERIES IN GRAPH g`: the privileges of queries created later.
      optional(choice(kw('ROLE'), kw('PRIVILEGE'), kw('DEFAULT'))),
      commaSep1(field('privilege', choice(
        $.identifier,
        kw('CREATE'),
        kw('DROP'),
        kw('UPDATE'),
        kw('DELETE'),
        kw('INSERT'),
        kw('SELECT'),
        kw('ALL'),
      ))),
      optional(seq(
        kw('ON'),
        repeat1(choice(
          $.identifier,
          $.qualified_identifier,
          kw('GRAPH'),
          kw('GLOBAL'),
          kw('ALL'),
          kw('QUERY'),
          kw('IN'),
          kw('VERTEX'),
          kw('EDGE'),
          ',',
          alias('*', $.wildcard),
          // `ON EDGE Knows ATTRIBUTE to`: a name must follow, so the
          // built-in `from` and `to` attributes are not read as keywords.
          seq(kw('ATTRIBUTE'), field('attribute', $.identifier)),
        )),
      )),
    ),

    shell_command: $ => prec.dynamic(1, choice(
      kw('LS'),
      kw('BEGIN'),
      kw('END'),
      // `ABORT` ends multi-line mode; `ABORT LOADING JOB id` stops a job.
      seq(choice(kw('ABORT'), kw('RESUME')), optional(seq(kw('LOADING'), kw('JOB'), repeat($._command_argument)))),
      kw('VERSION'),
      kw('HELP'),
      kw('QUIT'),
      kw('EXIT'),
      seq(kw('CLEAR'), kw('GRAPH'), kw('STORE'), repeat($.option_flag)),
      seq(
        choice(kw('EXPORT'), kw('IMPORT')),
        repeat(choice($._command_argument, kw('GRAPH'), kw('ALL'), kw('TO'), kw('FROM'))),
      ),
      seq(kw('SET'), field('name', $.identifier), '=', field('value', $._option_value)),
      // Workload queues: `LIST WORKLOAD QUEUE`, `PUT WORKLOAD QUEUE FROM "queues.json"`.
      // `GET TokenBank TO "/x.cpp"`, `PUT ExprFunctions FROM "/x.hpp"`.
      seq(choice(kw('GET'), kw('PUT')), field('name', $.identifier), choice(kw('TO'), kw('FROM')), $.string),
      seq(
        choice(kw('LIST'), kw('GET'), kw('PUT')),
        kw('WORKLOAD'),
        kw('QUEUE'),
        repeat(choice($._command_argument, kw('FROM'))),
      ),
      $.file_include,
    )),

    // 4.3 appendix "Keywords & Reserved Words" lists CLEAR, GET, GRANT and PUT
    // among the non-reserved keywords: "users may use them for user-defined
    // identifiers". As the name of an `INSTALL QUERY` or `SHOW ...` command
    // they are read as names (a command keyword in this position would
    // otherwise win over the identifier); `USE`, `SHOW` and `ABORT` are
    // accepted the same way. A command that fits on its own (`ABORT`) still
    // starts a new command through the dynamic precedence of shell_command.
    _command_name: $ => choice(
      ...['ABORT', 'CLEAR', 'GET', 'GRANT', 'PUT', 'USE', 'SHOW'].map(w => alias(kw(w), $.identifier)),
    ),

    _command_argument: $ => choice(
      $.identifier,
      // `LIST WORKLOAD QUEUE` and `UPDATE DESCRIPTION ...` also begin a
      // command: the commands carry a dynamic precedence, so after a name
      // such a line is read as a new command.
      $._soft_name,
      $.qualified_identifier,
      $.string,
      $.integer,
      $.float,
      $.option_flag,
      alias('*', $.wildcard),
      // `SHOW VERTEX ?????`: a glob that matches one character
      '?',
      ',',
      '=',
    ),

    file_include: _ => /@[^\s@][^\s]*/,

    // ------------------------------------------------------------------
    // Lexical elements
    // ------------------------------------------------------------------

    global_accumulator: _ => /@@[A-Za-z_][A-Za-z0-9_]*/,

    local_accumulator: _ => /@[A-Za-z_][A-Za-z0-9_]*/,

    // `$0`, `$"name"` and `$sys.file_name` column references in loading jobs.
    column_reference: _ => token(seq(
      '$',
      // `$"indent":"length"` reads a nested JSON field.
      choice(/\d+/, /"[^"\n]*"(:"[^"\n]*")*/, /[A-Za-z_][A-Za-z0-9_.]*/),
    )),

    integer: _ => /\d+/,

    float: _ => token(choice(
      /\d+\.\d+([eE][+-]?\d+)?/,
      /\.\d+([eE][+-]?\d+)?/,
      /\d+[eE][+-]?\d+/,
    )),

    string: $ => choice(
      seq(
        '"',
        repeat(choice($.string_content, $.escape_sequence)),
        token.immediate('"'),
      ),
      // `"""{ "type": "s3" }"""`: JSON configuration of data sources and file
      // names, which may span lines and contain quotes.
      token(prec(1, seq('"""', /([^"]|"[^"]|""[^"])*/, '"""'))),
    ),

    string_content: _ => token.immediate(prec(1, /[^"\\]+/)),

    escape_sequence: _ => token.immediate(/\\(.|\r?\n)/),

    boolean: _ => choice(/true/i, /false/i),

    null: _ => /null/i,

    wildcard: _ => '_',

    // `ELSE IF` on one line (followed by a space) is always the IF's own
    // clause: deciding that early keeps long ELSE IF ladders from forking
    // the parser at every branch. Split over lines, `ELSE` and `IF` may also
    // be an ELSE branch that starts with a nested IF.
    _else_if: _ => alias(token(prec(1, /[eE][lL][sS][eE][ \t]+[iI][fF][ \t]/)), 'ELSE IF'),

    comment: _ => token(choice(
      seq('//', /[^\r\n]*/),
      seq('#', /[^\r\n]*/),
      seq('/*', /[^*]*\*+([^/*][^*]*\*+)*/, '/'),
    )),

    // Words that start a statement (`LIST<INT> l;`, `FILE f ("x");`,
    // `INSERT INTO`, `UPDATE s FROM`, `MAP<..> m;`) but are non-reserved, so a
    // variable may carry the name: `list = list + 1;`, `file.clear();`. The
    // lexer returns the keyword token, and the next token decides: `<`, a name
    // or `INTO` continue the statement, anything else makes it a name.
    _soft_keyword: _ => choice(kw('LIST'), kw('MAP'), kw('FILE'), kw('INSERT'), kw('UPDATE')),

    _name: $ => choice($.identifier, $._soft_name),

    _soft_name: $ => alias($._soft_keyword, $.identifier),

    // Must stay the last rule: when a keyword and an identifier match the
    // same text, tree-sitter prefers the token that is defined first.
    identifier: _ => /[A-Za-z_][A-Za-z0-9_]*/,
  },
});
