// External scanner for the Askama grammar.
//
// It recognises what a context-free lexer cannot:
//
// - template content, which runs until the next `{{`, `{%` or `{#`, so the
//   injected language sees each run of text as one range;
// - comment bodies, which nest (`{# a {# b #} c #}`);
// - `raw` bodies, which end only at `{% endraw %}` with optional whitespace
//   control;
// - `{%`, as a different token for each keyword that continues or closes a
//   block (`else`, `elif`, `when`, `endwhen`, `endif`, …) and one block start
//   for everything else. A block's body then ends on one token of lookahead,
//   and a closing tag the open block does not expect is rejected before its
//   `{%` is consumed, so error recovery can close the inner block with a
//   missing end tag and let the outer block take the closing tag;
// - `let` and `set` at the start of a condition, which are keywords only when a
//   pattern and `=` follow (`{% if let Some(x) = y %}`) and variables
//   otherwise (`{% if set.is_empty() %}`), as askama decides by backtracking;
// - Rust raw strings (`r#"…"#`, `br"…"`, `cr"…"`) inside macro token trees.
//
// It never emits MISSING_END_TAG, which exists for error recovery to insert
// where a block is left open.
//
// The scanner keeps no state between tokens and never allocates. Its lookahead
// is bounded: one keyword past a `{%`, and at most LET_LOOKAHEAD characters
// past a `let` or `set`.

#include "tree_sitter/parser.h"

#include <stdbool.h>
#include <stdint.h>

enum TokenType {
  CONTENT,
  RAW_CONTENT,
  COMMENT_START,
  COMMENT_BODY,
  BLOCK_START,
  ELSE_START,
  ELIF_START,
  WHEN_START,
  ENDWHEN_START,
  ENDIF_START,
  ENDFOR_START,
  ENDMATCH_START,
  ENDBLOCK_START,
  ENDMACRO_START,
  ENDCALL_START,
  ENDFILTER_START,
  ENDLET_START,
  ENDSET_START,
  ENDRAW_START,
  LET_KEYWORD,
  SET_KEYWORD,
  RAW_STRING_LITERAL,
  MISSING_END_TAG,
  ERROR_SENTINEL,
};

// Room for the longest keyword the scanner tells apart, `endfilter`, and one
// more character, so a longer word never matches.
#define KEYWORD_CAPACITY 16

static const struct {
  const char *keyword;
  enum TokenType start;
} BRANCH_KEYWORDS[] = {
    {"else", ELSE_START},       {"elif", ELIF_START},         {"when", WHEN_START},
    {"endwhen", ENDWHEN_START}, {"endif", ENDIF_START},       {"endfor", ENDFOR_START},
    {"endmatch", ENDMATCH_START}, {"endblock", ENDBLOCK_START}, {"endmacro", ENDMACRO_START},
    {"endcall", ENDCALL_START}, {"endfilter", ENDFILTER_START}, {"endlet", ENDLET_START},
    {"endset", ENDSET_START},   {"endraw", ENDRAW_START},
};

// A `let` pattern longer than this is read as an expression. The longest in
// askama's tests and in koan's templates is under 100 characters.
#define LET_LOOKAHEAD 4096

void *tree_sitter_askama_external_scanner_create(void) { return NULL; }

void tree_sitter_askama_external_scanner_destroy(void *payload) { (void)payload; }

unsigned tree_sitter_askama_external_scanner_serialize(void *payload, char *buffer) {
  (void)payload;
  (void)buffer;
  return 0;
}

void tree_sitter_askama_external_scanner_deserialize(void *payload, const char *buffer,
                                                     unsigned length) {
  (void)payload;
  (void)buffer;
  (void)length;
}

// Askama skips ASCII whitespace only (`char::is_ascii_whitespace`).
static bool is_space(int32_t c) {
  return c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\f';
}

static bool is_whitespace_control(int32_t c) { return c == '-' || c == '+' || c == '~'; }

static bool is_keyword_char(int32_t c) {
  return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') ||
         c == '_';
}

static void advance(TSLexer *lexer) { lexer->advance(lexer, false); }

static bool equals(const char *keyword, unsigned length, const char *expected) {
  unsigned i = 0;
  for (; i < length; i++) {
    if (expected[i] != keyword[i]) return false;
  }
  return expected[i] == '\0';
}

// Advances over `word` while it matches. Returns false at the first mismatch,
// leaving the mismatching character unconsumed.
static bool match_word(TSLexer *lexer, const char *word) {
  for (const char *c = word; *c; c++) {
    if (lexer->lookahead != *c) return false;
    advance(lexer);
  }
  return true;
}

// Called with the lexer on the `%` of `{%`, the `{` already consumed. Ends the
// token after the optional whitespace control and reads the keyword after it
// as lookahead.
static bool scan_tag_start(TSLexer *lexer, const bool *valid_symbols) {
  advance(lexer);
  if (is_whitespace_control(lexer->lookahead)) advance(lexer);
  lexer->mark_end(lexer);

  while (is_space(lexer->lookahead)) advance(lexer);

  char keyword[KEYWORD_CAPACITY];
  unsigned length = 0;
  while (is_keyword_char(lexer->lookahead) && length < KEYWORD_CAPACITY) {
    keyword[length++] = (char)lexer->lookahead;
    advance(lexer);
  }

  for (unsigned i = 0; i < sizeof BRANCH_KEYWORDS / sizeof BRANCH_KEYWORDS[0]; i++) {
    if (!equals(keyword, length, BRANCH_KEYWORDS[i].keyword)) continue;
    enum TokenType start = BRANCH_KEYWORDS[i].start;
    if (!valid_symbols[start]) return false;
    lexer->result_symbol = start;
    return true;
  }
  if (!valid_symbols[BLOCK_START]) return false;
  lexer->result_symbol = BLOCK_START;
  return true;
}

// Called with the lexer on the `#` of `{#`, the `{` already consumed. Emits
// only the opening delimiter, after checking the comment is closed: an
// unclosed comment is a parse error in askama.
static bool scan_comment_start(TSLexer *lexer) {
  advance(lexer);
  lexer->mark_end(lexer);
  unsigned depth = 0;
  while (!lexer->eof(lexer)) {
    if (lexer->lookahead == '{') {
      advance(lexer);
      if (lexer->lookahead == '#') {
        advance(lexer);
        depth++;
      }
    } else if (lexer->lookahead == '#') {
      advance(lexer);
      if (lexer->lookahead == '}') {
        advance(lexer);
        if (depth == 0) {
          lexer->result_symbol = COMMENT_START;
          return true;
        }
        depth--;
      }
    } else {
      advance(lexer);
    }
  }
  return false;
}

// Scans a comment's body up to, not including, the `#}` that closes it.
static bool scan_comment_body(TSLexer *lexer) {
  unsigned depth = 0;
  bool has_body = false;
  while (!lexer->eof(lexer)) {
    if (lexer->lookahead == '{') {
      advance(lexer);
      has_body = true;
      if (lexer->lookahead == '#') {
        advance(lexer);
        depth++;
      }
    } else if (lexer->lookahead == '#') {
      lexer->mark_end(lexer);
      advance(lexer);
      if (lexer->lookahead == '}') {
        if (depth == 0) {
          lexer->result_symbol = COMMENT_BODY;
          return has_body;
        }
        advance(lexer);
        depth--;
      }
      has_body = true;
    } else {
      advance(lexer);
      has_body = true;
    }
  }
  return false;
}

// Scans template content up to the next delimiter. A delimiter at the very
// start is scanned as that delimiter instead.
static bool scan_content(TSLexer *lexer, const bool *valid_symbols) {
  bool has_content = false;
  while (!lexer->eof(lexer)) {
    if (lexer->lookahead != '{') {
      advance(lexer);
      has_content = true;
      continue;
    }
    lexer->mark_end(lexer);
    advance(lexer);
    int32_t next = lexer->lookahead;
    if (next == '{' || next == '%' || next == '#') {
      if (has_content) {
        lexer->result_symbol = CONTENT;
        return true;
      }
      if (next == '%') return scan_tag_start(lexer, valid_symbols);
      if (next == '#' && valid_symbols[COMMENT_START]) return scan_comment_start(lexer);
      return false;
    }
    has_content = true;
  }
  lexer->mark_end(lexer);
  lexer->result_symbol = CONTENT;
  return has_content;
}

// Scans the body of a `raw` block up to its `{% endraw %}`, which may carry
// whitespace control on either side. An empty body yields the `{%` of the
// closing tag instead. A body that never closes runs to the end of the file,
// so the missing tag is reported there.
static bool scan_raw_content(TSLexer *lexer) {
  bool has_content = false;
  for (;;) {
    if (lexer->eof(lexer)) {
      lexer->mark_end(lexer);
      lexer->result_symbol = RAW_CONTENT;
      return has_content;
    }
    if (lexer->lookahead != '{') {
      advance(lexer);
      has_content = true;
      continue;
    }
    if (has_content) lexer->mark_end(lexer);
    advance(lexer);
    if (lexer->lookahead != '%') {
      has_content = true;
      continue;
    }
    advance(lexer);
    if (is_whitespace_control(lexer->lookahead)) advance(lexer);
    if (!has_content) lexer->mark_end(lexer);
    while (is_space(lexer->lookahead)) advance(lexer);
    if (match_word(lexer, "endraw")) {
      while (is_space(lexer->lookahead)) advance(lexer);
      if (is_whitespace_control(lexer->lookahead)) advance(lexer);
      if (lexer->lookahead == '%') {
        advance(lexer);
        if (lexer->lookahead == '}') {
          lexer->result_symbol = has_content ? RAW_CONTENT : ENDRAW_START;
          return true;
        }
      }
    }
    has_content = true;
  }
}

// Skips a string or character literal starting at the quote under the lexer.
static void skip_quoted(TSLexer *lexer, unsigned *budget) {
  int32_t quote = lexer->lookahead;
  advance(lexer);
  while (!lexer->eof(lexer) && lexer->lookahead != quote && *budget > 0) {
    if (lexer->lookahead == '\\') advance(lexer);
    advance(lexer);
    (*budget)--;
  }
  advance(lexer);
}

// Called on the `l` or `s` of a possible `let` or `set` where a condition may
// start. Emits the keyword when a pattern follows and then a single `=`, the
// test askama applies by trying `let <pattern> =` and backtracking on failure.
static bool scan_let_keyword(TSLexer *lexer, const bool *valid_symbols) {
  enum TokenType symbol = lexer->lookahead == 'l' ? LET_KEYWORD : SET_KEYWORD;
  if (!valid_symbols[symbol]) return false;
  if (!match_word(lexer, symbol == LET_KEYWORD ? "let" : "set")) return false;
  if (is_keyword_char(lexer->lookahead)) return false;
  lexer->mark_end(lexer);

  unsigned depth = 0;
  unsigned budget = LET_LOOKAHEAD;
  while (!lexer->eof(lexer) && budget > 0) {
    int32_t c = lexer->lookahead;
    budget--;
    switch (c) {
      case '(':
      case '[':
      case '{':
        depth++;
        break;
      case ')':
      case ']':
      case '}':
        if (depth == 0) return false;
        depth--;
        break;
      case '"':
      case '\'':
        skip_quoted(lexer, &budget);
        continue;
      case '=':
        advance(lexer);
        if (depth != 0 || lexer->lookahead == '=') return false;
        lexer->result_symbol = symbol;
        return true;
      case '.':
        advance(lexer);
        if (lexer->lookahead != '.') return false;
        break;
      case '!':
      case '?':
      case '*':
      case '+':
      case '&':
      case '|':
      case '~':
      case '^':
      case '%':
      case ';':
      case '/':
        return false;
      default:
        break;
    }
    advance(lexer);
  }
  return false;
}

// Scans a Rust raw string: `r`, `br` or `cr`, up to 255 `#`, a quoted body
// and the same number of `#`.
static bool scan_raw_string(TSLexer *lexer) {
  if (lexer->lookahead == 'b' || lexer->lookahead == 'c') advance(lexer);
  if (lexer->lookahead != 'r') return false;
  advance(lexer);

  unsigned hashes = 0;
  while (lexer->lookahead == '#' && hashes < 256) {
    advance(lexer);
    hashes++;
  }
  if (lexer->lookahead != '"' || hashes > 255) return false;
  advance(lexer);

  while (!lexer->eof(lexer)) {
    if (lexer->lookahead != '"') {
      advance(lexer);
      continue;
    }
    advance(lexer);
    unsigned closing = 0;
    while (closing < hashes && lexer->lookahead == '#') {
      advance(lexer);
      closing++;
    }
    if (closing == hashes) {
      lexer->mark_end(lexer);
      lexer->result_symbol = RAW_STRING_LITERAL;
      return true;
    }
  }
  return false;
}

bool tree_sitter_askama_external_scanner_scan(void *payload, TSLexer *lexer,
                                              const bool *valid_symbols) {
  (void)payload;
  // During error recovery every symbol is valid. Content and tags are still
  // scanned so recovery can resynchronise on them; raw bodies and raw strings
  // are not, since they would swallow the rest of the file.
  bool recovering = valid_symbols[ERROR_SENTINEL];

  if (valid_symbols[COMMENT_BODY] && !recovering) return scan_comment_body(lexer);

  if (valid_symbols[RAW_CONTENT] && !recovering) return scan_raw_content(lexer);

  if (valid_symbols[CONTENT]) return scan_content(lexer, valid_symbols);

  while (is_space(lexer->lookahead)) lexer->advance(lexer, true);

  if (lexer->lookahead == '{') {
    advance(lexer);
    if (lexer->lookahead == '%') return scan_tag_start(lexer, valid_symbols);
    if (lexer->lookahead == '#' && valid_symbols[COMMENT_START]) return scan_comment_start(lexer);
    return false;
  }

  if ((valid_symbols[LET_KEYWORD] || valid_symbols[SET_KEYWORD]) && !recovering &&
      (lexer->lookahead == 'l' || lexer->lookahead == 's')) {
    return scan_let_keyword(lexer, valid_symbols);
  }

  if (valid_symbols[RAW_STRING_LITERAL] && !recovering &&
      (lexer->lookahead == 'r' || lexer->lookahead == 'b' || lexer->lookahead == 'c')) {
    return scan_raw_string(lexer);
  }

  return false;
}
