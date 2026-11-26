; Macros are functions and template blocks are classes, so `]m` moves between
; macros and `]]` between blocks.
(macro_definition
  open: (_)
  body: (_)* @function.inside
  close: (_)) @function.around

(call_block
  open: (_)
  body: (_)* @function.inside
  close: (_)) @function.around

(block_definition
  open: (_)
  body: (_)* @class.inside
  close: (_)) @class.around

(comment
  (comment_body) @comment.inside) @comment.around
