/**
 * @file Askama templates, with the default delimiters.
 *
 * The specification is askama 0.16.1's own parser (`askama_parser`). Where the
 * two disagree, this grammar is wrong. Node names follow tree-sitter-rust where
 * the meaning matches, so Rust highlighting conventions carry over.
 *
 * Every `{% … %}` tag starts with a `{%` token from the external scanner, which
 * looks at the keyword after it: each keyword that continues or closes a block
 * (`else`, `elif`, `when`, `endif`, …) has a start token of its own, and every
 * other tag starts with `_block_start`. That single token of lookahead is what
 * lets a block's body end deterministically, so each block is one node whose
 * children are its tags, branches and bodies.
 *
 * Every closing tag may also be `_missing_end_tag`, a token the scanner never
 * emits. Error recovery inserts it where a block is left open, so an unclosed
 * block shows as a MISSING closing tag rather than an ERROR over the rest of
 * the file.
 */

/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

const PREC = {
  range: 1,
  let_chain: 2,
  or: 3,
  and: 4,
  compare: 5,
  bitor: 6,
  xor: 7,
  bitand: 8,
  shift: 9,
  additive: 10,
  concat: 11,
  multiplicative: 12,
  is_as: 13,
  filter: 14,
  unary: 15,
  postfix: 16,
};

const INTEGER_SUFFIX = '(i8|i16|i32|i64|i128|isize|u8|u16|u32|u64|u128|usize)';
const FLOAT_SUFFIX = '(f16|f32|f64|f128)';
const PRIMITIVE_TYPES = [
  'i8', 'i16', 'i32', 'i64', 'i128', 'isize',
  'u8', 'u16', 'u32', 'u64', 'u128', 'usize',
  'f16', 'f32', 'f64', 'f128',
  'bool',
];

/**
 * A comma-separated list of one or more `rule`, with an optional trailing comma.
 *
 * @param {RuleOrLiteral} rule
 */
const commaSep1 = (rule) => seq(rule, repeat(seq(',', rule)), optional(','));

/**
 * A comma-separated list of zero or more `rule`, with an optional trailing comma.
 *
 * @param {RuleOrLiteral} rule
 */
const commaSep = (rule) => optional(commaSep1(rule));

module.exports = grammar({
  name: 'askama',

  externals: ($) => [
    $.content,
    $.raw_content,
    $._comment_start,
    $._comment_body,
    $._block_start,
    $._else_start,
    $._elif_start,
    $._when_start,
    $._endwhen_start,
    $._endif_start,
    $._endfor_start,
    $._endmatch_start,
    $._endblock_start,
    $._endmacro_start,
    $._endcall_start,
    $._endfilter_start,
    $._endlet_start,
    $._endset_start,
    $._endraw_start,
    $._let_keyword,
    $._set_keyword,
    $.raw_string_literal,
    $._missing_end_tag,
    $._error_sentinel,
  ],

  extras: (_) => [/\s/],

  word: ($) => $.identifier,

  supertypes: ($) => [$._expression, $._pattern, $._type],

  conflicts: (_) => [],

  rules: {
    template: ($) => repeat($._node),

    _node: ($) => choice(
      $.content,
      $.comment,
      $.expression_tag,
      $.extends_statement,
      $.include_statement,
      $.import_statement,
      $.let_statement,
      $.declare_statement,
      $.assignment_statement,
      $.break_statement,
      $.continue_statement,
      $.if_block,
      $.for_block,
      $.match_block,
      $.block_definition,
      $.macro_definition,
      $.call_block,
      $.filter_block,
      $.let_block,
      $.raw_block,
    ),

    // Delimiters. Whitespace control (`-`, `+`, `~`) is part of the delimiter
    // token, because `{{-x}}` trims and does not negate.

    _expression_start: (_) => alias(token(/\{\{[-+~]?/), '{{'),
    _expression_end: (_) => alias(token(/[-+~]?\}\}/), '}}'),
    _block_end: (_) => alias(token(/[-+~]?%\}/), '%}'),
    _open: ($) => alias($._block_start, '{%'),

    comment: ($) => seq(
      alias($._comment_start, '{#'),
      optional(alias($._comment_body, $.comment_body)),
      '#}',
    ),

    expression_tag: ($) => seq(
      $._expression_start,
      field('expression', $._expression),
      $._expression_end,
    ),

    // Statements: a single tag each.

    extends_statement: ($) => seq(
      $._open, 'extends', field('path', $.string_literal), $._block_end,
    ),

    include_statement: ($) => seq(
      $._open, 'include', field('path', $.string_literal), $._block_end,
    ),

    import_statement: ($) => seq(
      $._open,
      'import',
      field('path', $.string_literal),
      'as',
      field('alias', $.identifier),
      $._block_end,
    ),

    let_statement: ($) => seq(
      $._open,
      choice('let', 'set'),
      $._let_binding,
      optional(seq(':', field('type', $._type))),
      '=',
      field('value', $._expression),
      $._block_end,
    ),

    declare_statement: ($) => seq(
      $._open,
      choice('decl', 'declare'),
      optional($.mutable_specifier),
      field('name', $.identifier),
      $._block_end,
    ),

    assignment_statement: ($) => seq(
      $._open,
      'mut',
      field('left', $._expression),
      field('operator', choice('=', '+=', '-=', '*=', '/=', '%=', '&=', '|=', '^=', '<<=', '>>=')),
      field('right', $._expression),
      $._block_end,
    ),

    break_statement: ($) => seq($._open, 'break', $._block_end),

    continue_statement: ($) => seq($._open, 'continue', $._block_end),

    // Blocks. Each block is one node: its opening tag, its body, its branches
    // and its closing tag.

    if_block: ($) => seq(
      field('open', $.if_tag),
      repeat(field('body', $._node)),
      repeat(field('alternative', choice($.else_if_clause, $.else_clause))),
      field('close', choice($.endif_tag, alias($._missing_end_tag, $.endif_tag))),
    ),

    if_tag: ($) => seq($._open, 'if', field('condition', $._condition), $._block_end),

    else_if_clause: ($) => seq(
      field('open', $.else_if_tag),
      repeat(field('body', $._node)),
    ),

    else_if_tag: ($) => seq(
      choice(
        seq(alias($._else_start, '{%'), 'else', 'if'),
        seq(alias($._elif_start, '{%'), 'elif'),
      ),
      field('condition', $._condition),
      $._block_end,
    ),

    else_clause: ($) => seq(
      field('open', $.else_tag),
      repeat(field('body', $._node)),
    ),

    else_tag: ($) => seq(alias($._else_start, '{%'), 'else', $._block_end),

    endif_tag: ($) => seq(alias($._endif_start, '{%'), 'endif', $._block_end),

    _condition: ($) => choice($._expression, $.let_condition, $.let_chain),

    // `let` and `set` here come from the scanner, which reads them as keywords
    // only when a pattern and `=` follow; otherwise they are variables.
    let_condition: ($) => prec.right(PREC.let_chain, seq(
      choice(alias($._let_keyword, 'let'), alias($._set_keyword, 'set')),
      field('pattern', $._pattern),
      '=',
      field('value', $._condition),
    )),

    let_chain: ($) => prec.right(PREC.let_chain, seq(
      field('left', $._expression),
      field('operator', choice('&&', '||')),
      field('right', $.let_condition),
    )),

    for_block: ($) => seq(
      field('open', $.for_tag),
      repeat(field('body', $._node)),
      optional(field('alternative', $.else_clause)),
      field('close', choice($.endfor_tag, alias($._missing_end_tag, $.endfor_tag))),
    ),

    for_tag: ($) => seq(
      $._open,
      'for',
      field('pattern', $._pattern),
      'in',
      field('iterable', $._expression),
      optional(seq('if', field('condition', $._expression))),
      $._block_end,
    ),

    endfor_tag: ($) => seq(alias($._endfor_start, '{%'), 'endfor', $._block_end),

    // Only whitespace and comments may stand between `{% match %}` and the
    // first arm, or after an `{% endwhen %}`.
    match_block: ($) => seq(
      field('open', $.match_tag),
      repeat($.comment),
      choice(
        seq(repeat1(field('arm', $.when_clause)), optional(field('arm', $.else_clause))),
        field('arm', $.else_clause),
      ),
      field('close', choice($.endmatch_tag, alias($._missing_end_tag, $.endmatch_tag))),
    ),

    match_tag: ($) => seq($._open, 'match', field('value', $._expression), $._block_end),

    when_clause: ($) => seq(
      field('open', $.when_tag),
      repeat(field('body', $._node)),
      optional(seq(field('close', $.endwhen_tag), repeat($.comment))),
    ),

    when_tag: ($) => seq(
      alias($._when_start, '{%'),
      'when',
      field('pattern', $._pattern),
      repeat(seq('|', field('pattern', $._pattern))),
      $._block_end,
    ),

    endwhen_tag: ($) => seq(alias($._endwhen_start, '{%'), 'endwhen', $._block_end),

    endmatch_tag: ($) => seq(alias($._endmatch_start, '{%'), 'endmatch', $._block_end),

    block_definition: ($) => seq(
      field('open', $.block_tag),
      repeat(field('body', $._node)),
      field('close', choice($.endblock_tag, alias($._missing_end_tag, $.endblock_tag))),
    ),

    block_tag: ($) => seq($._open, 'block', field('name', $.identifier), $._block_end),

    endblock_tag: ($) => seq(
      alias($._endblock_start, '{%'), 'endblock', optional(field('name', $.identifier)), $._block_end,
    ),

    macro_definition: ($) => seq(
      field('open', $.macro_tag),
      repeat(field('body', $._node)),
      field('close', choice($.endmacro_tag, alias($._missing_end_tag, $.endmacro_tag))),
    ),

    macro_tag: ($) => seq(
      $._open,
      'macro',
      field('name', $.identifier),
      optional(field('parameters', $.parameters)),
      $._block_end,
    ),

    parameters: ($) => seq('(', commaSep($.parameter), ')'),

    parameter: ($) => seq(
      field('name', $.identifier),
      optional(seq(':', field('type', $._type))),
      optional(seq('=', field('default', $._expression))),
    ),

    endmacro_tag: ($) => seq(
      alias($._endmacro_start, '{%'), 'endmacro', optional(field('name', $.identifier)), $._block_end,
    ),

    call_block: ($) => seq(
      field('open', $.call_tag),
      repeat(field('body', $._node)),
      field('close', choice($.endcall_tag, alias($._missing_end_tag, $.endcall_tag))),
    ),

    call_tag: ($) => seq(
      $._open,
      'call',
      optional(field('parameters', $.caller_parameters)),
      field('name', choice(
        $.identifier,
        alias($._call_scoped_identifier, $.scoped_identifier),
      )),
      optional(field('arguments', $.arguments)),
      $._block_end,
    ),

    _call_scoped_identifier: ($) => seq(
      field('path', $.identifier),
      '::',
      field('name', $.identifier),
    ),

    caller_parameters: ($) => seq('(', commaSep($.identifier), ')'),

    endcall_tag: ($) => seq(alias($._endcall_start, '{%'), 'endcall', $._block_end),

    filter_block: ($) => seq(
      field('open', $.filter_tag),
      repeat(field('body', $._node)),
      field('close', choice($.endfilter_tag, alias($._missing_end_tag, $.endfilter_tag))),
    ),

    filter_tag: ($) => seq(
      $._open,
      'filter',
      field('filter', $.filter),
      repeat(seq('|', field('filter', $.filter))),
      $._block_end,
    ),

    endfilter_tag: ($) => seq(alias($._endfilter_start, '{%'), 'endfilter', $._block_end),

    // `{% let x %}` with no value opens a block that `{% endlet %}` closes, and
    // `{% set x %}` one that `{% endset %}` closes.
    let_block: ($) => choice(
      seq(
        field('open', alias($._let_tag, $.let_tag)),
        repeat(field('body', $._node)),
        field('close', choice(alias($._endlet_tag, $.endlet_tag), alias($._missing_end_tag, $.endlet_tag))),
      ),
      seq(
        field('open', alias($._set_tag, $.let_tag)),
        repeat(field('body', $._node)),
        field('close', choice(alias($._endset_tag, $.endlet_tag), alias($._missing_end_tag, $.endlet_tag))),
      ),
    ),

    _let_tag: ($) => seq($._open, 'let', $._let_tag_binding, $._block_end),

    _set_tag: ($) => seq($._open, 'set', $._let_tag_binding, $._block_end),

    // The parser rejects any pattern but a name here, which this grammar leaves
    // to the language server so the tag stays one node while being typed.
    _let_tag_binding: ($) => seq(
      $._let_binding,
      optional(seq(':', field('type', $._type))),
    ),

    // `mut` applies to a plain name only.
    _let_binding: ($) => choice(
      seq($.mutable_specifier, field('pattern', $.identifier)),
      field('pattern', $._pattern),
    ),

    _endlet_tag: ($) => seq(alias($._endlet_start, '{%'), 'endlet', $._block_end),

    _endset_tag: ($) => seq(alias($._endset_start, '{%'), 'endset', $._block_end),

    raw_block: ($) => seq(
      field('open', $.raw_tag),
      optional(field('body', $.raw_content)),
      field('close', choice($.endraw_tag, alias($._missing_end_tag, $.endraw_tag))),
    ),

    raw_tag: ($) => seq($._open, 'raw', $._block_end),

    endraw_tag: ($) => seq(alias($._endraw_start, '{%'), 'endraw', $._block_end),

    // Expressions, from loosest to tightest:
    // ranges, `||`, `&&`, comparisons, `bitor`, `xor`, `bitand`, shifts,
    // `+ -`, `~`, `* / %`, `is`/`as`, filters, prefix operators, suffixes.

    _expression: ($) => choice(
      $.range_expression,
      $.binary_expression,
      $.is_defined_expression,
      $.type_cast_expression,
      $.filter_expression,
      $.unary_expression,
      $.reference_expression,
      $.try_expression,
      $.field_expression,
      $.index_expression,
      $.call_expression,
      $.generic_function,
      $.macro_invocation,
      $.struct_expression,
      $.parenthesized_expression,
      $.tuple_expression,
      $.unit_expression,
      $.array_expression,
      $.identifier,
      $.scoped_identifier,
      $._literal,
    ),

    _literal: ($) => choice(
      $.string_literal,
      $.char_literal,
      $.boolean_literal,
      $.integer_literal,
      $.float_literal,
    ),

    range_expression: ($) => prec.left(PREC.range, choice(
      seq(field('start', $._expression), field('operator', choice('..', '..=')), field('end', $._expression)),
      seq(field('start', $._expression), field('operator', choice('..', '..='))),
      seq(field('operator', choice('..', '..=')), field('end', $._expression)),
      field('operator', choice('..', '..=')),
    )),

    binary_expression: ($) => {
      /** @type {[number, RuleOrLiteral][]} */
      const table = [
        [PREC.or, '||'],
        [PREC.and, '&&'],
        [PREC.compare, choice('==', '!=', '<', '<=', '>', '>=')],
        [PREC.bitor, 'bitor'],
        [PREC.xor, 'xor'],
        [PREC.bitand, 'bitand'],
        [PREC.shift, choice('<<', '>>')],
        [PREC.additive, choice('+', '-')],
        [PREC.concat, '~'],
        [PREC.multiplicative, choice('*', '/', '%')],
      ];
      return choice(...table.map(([precedence, operator]) => prec.left(precedence, seq(
        field('left', $._expression),
        field('operator', operator),
        field('right', $._expression),
      ))));
    },

    is_defined_expression: ($) => prec(PREC.is_as, seq(
      field('name', $.identifier),
      'is',
      optional('not'),
      'defined',
    )),

    type_cast_expression: ($) => prec.left(PREC.is_as, seq(
      field('value', $._expression),
      'as',
      field('type', $.primitive_type),
    )),

    filter_expression: ($) => prec.left(PREC.filter, seq(
      field('value', $._expression),
      '|',
      field('filter', $.filter),
    )),

    filter: ($) => prec.right(seq(
      field('name', choice(
        $.identifier,
        $.scoped_identifier,
        alias($._generic_filter_name, $.generic_function),
      )),
      optional(field('arguments', $.arguments)),
    )),

    _generic_filter_name: ($) => seq(
      field('function', choice($.identifier, $.scoped_identifier)),
      '::',
      field('type_arguments', $.type_arguments),
    ),

    unary_expression: ($) => prec(PREC.unary, seq(
      field('operator', choice('-', '!', '*')),
      field('argument', $._expression),
    )),

    reference_expression: ($) => prec(PREC.unary, seq('&', field('value', $._expression))),

    try_expression: ($) => prec(PREC.postfix, seq($._expression, '?')),

    field_expression: ($) => prec(PREC.postfix, seq(
      field('value', $._expression),
      '.',
      field('field', choice(
        alias($.identifier, $.field_identifier),
        alias(token(/[0-9]+/), $.integer_literal),
      )),
    )),

    index_expression: ($) => prec(PREC.postfix, seq(
      field('value', $._expression),
      '[',
      field('index', $._expression),
      ']',
    )),

    call_expression: ($) => prec(PREC.postfix, seq(
      field('function', $._expression),
      field('arguments', $.arguments),
    )),

    // Named arguments come after every positional one.
    arguments: ($) => seq('(', optional($._positional_arguments), ')'),

    _positional_arguments: ($) => choice(
      seq($._expression, optional(seq(',', optional($._positional_arguments)))),
      $._named_arguments,
    ),

    _named_arguments: ($) => seq(
      $.named_argument,
      optional(seq(',', optional($._named_arguments))),
    ),

    named_argument: ($) => seq(
      field('name', $.identifier),
      '=',
      field('value', $._expression),
    ),

    generic_function: ($) => prec(PREC.postfix, seq(
      field('function', choice($.identifier, $.scoped_identifier, $.field_expression)),
      '::',
      field('type_arguments', $.type_arguments),
    )),

    macro_invocation: ($) => prec(PREC.postfix, seq(
      field('macro', choice($.identifier, $.scoped_identifier)),
      '!',
      $.token_tree,
    )),

    struct_expression: ($) => prec(PREC.postfix, seq(
      field('name', choice(
        alias($.identifier, $.type_identifier),
        $.scoped_identifier,
        alias($.generic_type_with_turbofish, $.generic_type),
      )),
      field('body', $.field_initializer_list),
    )),

    // The base (`..base`) comes last.
    field_initializer_list: ($) => seq(
      '{',
      repeat(seq($._field_initializer, ',')),
      optional(choice($._field_initializer, seq($.base_field_initializer, optional(',')))),
      '}',
    ),

    _field_initializer: ($) => choice($.shorthand_field_initializer, $.field_initializer),

    shorthand_field_initializer: ($) => $.identifier,

    field_initializer: ($) => seq(
      field('field', choice(
        alias($.identifier, $.field_identifier),
        alias(token(/[0-9]+/), $.integer_literal),
      )),
      ':',
      field('value', $._expression),
    ),

    base_field_initializer: ($) => seq('..', $._expression),

    parenthesized_expression: ($) => seq('(', $._expression, ')'),

    tuple_expression: ($) => seq(
      '(',
      seq($._expression, ','),
      repeat(seq($._expression, ',')),
      optional($._expression),
      ')',
    ),

    unit_expression: (_) => seq('(', ')'),

    array_expression: ($) => seq(
      '[',
      choice(
        seq($._expression, ';', field('length', $._expression)),
        commaSep($._expression),
      ),
      ']',
    ),

    // Paths. A turbofish may follow any segment (`Vec::<u8>::new`).

    scoped_identifier: ($) => seq(
      field('path', optional(choice(
        $.identifier,
        $.scoped_identifier,
        alias($.generic_type_with_turbofish, $.generic_type),
      ))),
      '::',
      field('name', $.identifier),
    ),

    generic_type_with_turbofish: ($) => seq(
      field('type', choice($.identifier, $.scoped_identifier)),
      '::',
      field('type_arguments', $.type_arguments),
    ),

    // Types, as askama's parser takes them: references, tuples, arrays and paths
    // with generic arguments.

    _type: ($) => choice(
      $.reference_type,
      $.tuple_type,
      $.unit_type,
      $.array_type,
      $.generic_type,
      $.scoped_type_identifier,
      alias($.identifier, $.type_identifier),
      $.primitive_type,
    ),

    reference_type: ($) => seq('&', field('type', $._type)),

    tuple_type: ($) => seq('(', choice(commaSep1($._type), ','), ')'),

    unit_type: (_) => seq('(', ')'),

    array_type: ($) => seq(
      '[',
      field('element', $._type),
      optional(seq(';', field('length', $.integer_literal))),
      ']',
    ),

    generic_type: ($) => seq(
      field('type', choice(alias($.identifier, $.type_identifier), $.scoped_type_identifier)),
      field('type_arguments', $.type_arguments),
    ),

    scoped_type_identifier: ($) => seq(
      field('path', choice($.identifier, alias($._type_path, $.scoped_identifier))),
      '::',
      field('name', alias($.identifier, $.type_identifier)),
    ),

    _type_path: ($) => seq(
      field('path', choice($.identifier, alias($._type_path, $.scoped_identifier))),
      '::',
      field('name', $.identifier),
    ),

    type_arguments: ($) => seq('<', commaSep($._type), '>'),

    primitive_type: (_) => choice(...PRIMITIVE_TYPES),

    // Patterns (askama's "targets").

    _pattern: ($) => choice(
      $._literal_pattern,
      $.identifier,
      $.scoped_identifier,
      $.tuple_pattern,
      $.slice_pattern,
      $.tuple_struct_pattern,
      $.struct_pattern,
      $.or_pattern,
      '_',
    ),

    _literal_pattern: ($) => choice(
      $.string_literal,
      $.char_literal,
      $.boolean_literal,
      $.integer_literal,
      $.float_literal,
      $.negative_literal,
    ),

    negative_literal: ($) => seq('-', choice($.integer_literal, $.float_literal)),

    or_pattern: ($) => prec.left(seq($._pattern, 'or', $._pattern)),

    _rest_pattern: ($) => choice($.remaining_field_pattern, $.captured_pattern),

    remaining_field_pattern: (_) => '..',

    captured_pattern: ($) => seq(field('name', $.identifier), '@', $.remaining_field_pattern),

    tuple_pattern: ($) => seq('(', commaSep(choice($._pattern, $._rest_pattern)), ')'),

    slice_pattern: ($) => seq('[', commaSep(choice($._pattern, $._rest_pattern)), ']'),

    _pattern_path: ($) => choice(
      $.identifier,
      $.scoped_identifier,
      alias($.generic_type_with_turbofish, $.generic_type),
    ),

    tuple_struct_pattern: ($) => seq(
      field('type', $._pattern_path),
      optional('with'),
      '(',
      commaSep(choice($._pattern, $._rest_pattern)),
      ')',
    ),

    // `..` comes last, with no comma after it.
    struct_pattern: ($) => seq(
      field('type', $._pattern_path),
      optional('with'),
      '{',
      repeat(seq($.field_pattern, ',')),
      optional(choice($.field_pattern, $.remaining_field_pattern)),
      '}',
    ),

    field_pattern: ($) => choice(
      field('name', alias($.identifier, $.shorthand_field_identifier)),
      seq(
        field('name', alias($.identifier, $.field_identifier)),
        ':',
        field('pattern', $._pattern),
      ),
    ),

    // Rust macro calls take any balanced token tree, as in Rust.

    token_tree: ($) => choice(
      seq('(', repeat($._token), ')'),
      seq('[', repeat($._token), ']'),
      seq('{', repeat($._token), '}'),
    ),

    _token: ($) => choice(
      $.token_tree,
      $.identifier,
      $.string_literal,
      $.raw_string_literal,
      $.char_literal,
      $.integer_literal,
      $.float_literal,
      $.lifetime,
      $.line_comment,
      $.block_comment,
      $._punctuation,
    ),

    lifetime: (_) => token(seq("'", /[_\p{XID_Start}][\p{XID_Continue}]*/)),

    line_comment: (_) => token(seq('//', /[^\n]*/)),

    block_comment: (_) => token(seq('/*', /[^*]*\*+([^/*][^*]*\*+)*/, '/')),

    _punctuation: (_) => token(prec(-1, /[!#$%&*+,\-./:;<=>?@^|~]/)),

    // Literals.

    string_literal: ($) => seq(
      alias(/[bc]?"/, '"'),
      repeat(choice($.escape_sequence, $.string_content)),
      token.immediate('"'),
    ),

    string_content: (_) => token.immediate(prec(1, /[^"\\]+/)),

    escape_sequence: (_) => token.immediate(seq(
      '\\',
      choice(/[nrt\\0'"]/, /x[0-9a-fA-F]{2}/, /u\{[0-9a-fA-F]{1,6}\}/),
    )),

    char_literal: (_) => token(seq(
      optional('b'),
      "'",
      choice(
        /[^'\\]/,
        seq('\\', choice(/[nrt\\0'"]/, /x[0-9a-fA-F]{2}/, /u\{[0-9a-fA-F]{1,6}\}/)),
      ),
      "'",
    )),

    boolean_literal: (_) => choice('true', 'false'),

    integer_literal: (_) => token(choice(
      new RegExp(`[0-9][0-9_]*(${INTEGER_SUFFIX}|${FLOAT_SUFFIX})?`),
      new RegExp(`0b_*[01][01_]*${INTEGER_SUFFIX}?`),
      new RegExp(`0o_*[0-7][0-7_]*${INTEGER_SUFFIX}?`),
      new RegExp(`0x_*[0-9a-fA-F][0-9a-fA-F_]*${INTEGER_SUFFIX}?`),
    )),

    float_literal: (_) => token(choice(
      new RegExp(`[0-9][0-9_]*\\.[0-9][0-9_]*([eE]([0-9][0-9_]*|[+-]_*[0-9][0-9_]*))?${FLOAT_SUFFIX}?`),
      new RegExp(`[0-9][0-9_]*[eE]([0-9][0-9_]*|[+-]_*[0-9][0-9_]*)${FLOAT_SUFFIX}?`),
    )),

    mutable_specifier: (_) => 'mut',

    identifier: (_) => /[_\p{XID_Start}][\p{XID_Continue}]*/,
  },
});
