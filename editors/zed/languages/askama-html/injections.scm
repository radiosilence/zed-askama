; Template content is HTML. Every run of it is parsed as one document, so
; elements opened before a tag and closed after it still pair up.
([
  (content)
  (raw_content)
] @injection.content
  (#set! injection.language "html")
  (#set! injection.combined))
