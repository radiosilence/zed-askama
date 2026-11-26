; Expressions follow Zed's Rust highlighting. Later patterns take precedence.

(identifier) @variable

((identifier) @variable.special
  (#any-of? @variable.special "self" "loop" "caller"))

; Assume uppercase names are types or enum variants, and all-caps names constants.
((identifier) @type
  (#match? @type "^[A-Z]"))

((identifier) @constant
  (#match? @constant "^_*[A-Z][A-Z\\d_]*$"))

(type_identifier) @type

(primitive_type) @type.builtin

[
  (field_identifier)
  (shorthand_field_identifier)
] @property

(shorthand_field_initializer
  (identifier) @property)

(field_initializer
  field: (_) @property)

(call_expression
  function: [
    (identifier) @function
    (scoped_identifier
      name: (identifier) @function)
    (field_expression
      field: (field_identifier) @function.method)
  ])

(generic_function
  function: [
    (identifier) @function
    (scoped_identifier
      name: (identifier) @function)
    (field_expression
      field: (field_identifier) @function.method)
  ])

(filter
  name: [
    (identifier) @function
    (scoped_identifier
      name: (identifier) @function)
    (generic_function
      function: [
        (identifier) @function
        (scoped_identifier
          name: (identifier) @function)
      ])
  ])

(macro_invocation
  macro: [
    (identifier) @function.special
    (scoped_identifier
      name: (identifier) @function.special)
  ])

(macro_invocation
  "!" @function.special)

(macro_tag
  name: (identifier) @function.definition)

(endmacro_tag
  name: (identifier) @function.definition)

(call_tag
  name: [
    (identifier) @function
    (scoped_identifier
      name: (identifier) @function)
  ])

[
  (block_tag
    name: (identifier) @label)
  (endblock_tag
    name: (identifier) @label)
]

(parameter
  name: (identifier) @variable.parameter)

(caller_parameters
  (identifier) @variable.parameter)

(named_argument
  name: (identifier) @variable.parameter)

(import_statement
  alias: (identifier) @variable.special)

(is_defined_expression
  name: (identifier) @variable)

; Literals

[
  (string_literal)
  (raw_string_literal)
  (char_literal)
] @string

[
  (extends_statement
    path: (string_literal) @string.special)
  (include_statement
    path: (string_literal) @string.special)
  (import_statement
    path: (string_literal) @string.special)
]

(escape_sequence) @string.escape

[
  (integer_literal)
  (float_literal)
] @number

(negative_literal
  "-" @number)

(boolean_literal) @boolean

(lifetime) @lifetime

[
  (comment)
  (line_comment)
  (block_comment)
] @comment

; Keywords

[
  "if"
  "elif"
  "else"
  "endif"
  "for"
  "in"
  "endfor"
  "match"
  "when"
  "endwhen"
  "endmatch"
  "break"
  "continue"
] @keyword.control

[
  "block"
  "endblock"
  "macro"
  "endmacro"
  "call"
  "endcall"
  "filter"
  "endfilter"
  "let"
  "set"
  "endlet"
  "endset"
  "decl"
  "declare"
  "raw"
  "endraw"
  "extends"
  "include"
  "import"
  "as"
  "with"
  (mutable_specifier)
] @keyword

(assignment_statement
  "mut" @keyword)

[
  "is"
  "not"
  "defined"
  "or"
  "bitor"
  "xor"
  "bitand"
] @keyword.operator

; Delimiters and punctuation

[
  "{{"
  "}}"
  "{%"
  "%}"
] @punctuation.special

[
  "("
  ")"
  "["
  "]"
  "{"
  "}"
] @punctuation.bracket

(type_arguments
  [
    "<"
    ">"
  ] @punctuation.bracket)

[
  "."
  ","
  ";"
  "::"
] @punctuation.delimiter

[
  "!="
  "%"
  "%="
  "&"
  "&="
  "&&"
  "*"
  "*="
  "+"
  "+="
  "-"
  "-="
  ".."
  "..="
  "/"
  "/="
  ":"
  "<<"
  "<<="
  "<"
  "<="
  "="
  "=="
  ">"
  ">="
  ">>"
  ">>="
  "@"
  "^="
  "|"
  "|="
  "||"
  "~"
  "?"
] @operator

(unary_expression
  "!" @operator)
