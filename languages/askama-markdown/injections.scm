; Template content is Markdown. Every run of it is parsed as one document, so
; a list or table that a tag interrupts still reads as one.
([
  (content)
  (raw_content)
] @injection.content
  (#set! injection.language "markdown")
  (#set! injection.combined))
