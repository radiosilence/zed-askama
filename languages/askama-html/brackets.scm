("(" @open
  ")" @close)

("[" @open
  "]" @close)

("{" @open
  "}" @close)

(type_arguments
  "<" @open
  ">" @close)

(("{{" @open
  "}}" @close)
  (#set! rainbow.exclude))

(("{%" @open
  "%}" @close)
  (#set! rainbow.exclude))

((comment
  "{#" @open
  "#}" @close)
  (#set! rainbow.exclude))

(("\"" @open
  "\"" @close)
  (#set! rainbow.exclude))

; A block's opening and closing tags pair up like an HTML element's.
((_
  open: (_) @open
  close: (_) @close)
  (#set! newline.only)
  (#set! rainbow.exclude))
