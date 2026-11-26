; A block's body indents between its opening and closing tags.
(_
  open: (_) @start
  close: (_)? @end) @indent

; `else` and `else if` sit level with the `if` or `for` they belong to.
(if_block
  alternative: (_
    open: (_) @outdent))

(for_block
  alternative: (_
    open: (_) @outdent))

(_
  "("
  ")" @end) @indent

(_
  "["
  "]" @end) @indent

(_
  "{"
  "}" @end) @indent
