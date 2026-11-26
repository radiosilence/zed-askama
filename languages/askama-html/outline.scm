(block_definition
  open: (block_tag
    "block" @context
    name: (identifier) @name)) @item

(macro_definition
  open: (macro_tag
    "macro" @context
    name: (identifier) @name)) @item

(call_block
  open: (call_tag
    "call" @context
    name: (_) @name)) @item

(extends_statement
  "extends" @context
  path: (string_literal) @name) @item

(include_statement
  "include" @context
  path: (string_literal) @name) @item

(import_statement
  "import" @context
  path: (string_literal) @name
  "as" @context
  alias: (identifier) @name) @item
