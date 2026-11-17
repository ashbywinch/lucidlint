//! The Rust comment layer: which bytes of a `.rs` file are comments, read
//! without a parser so a file that does not parse still yields its directives.

/// `//` and `/* */` comments with their (1-based) line.
///
/// Python gets its comments from ruff's tokens; Rust needs its own scan. It is
/// string-aware because `//` inside a literal is not a comment, and it counts
/// lines because suppressions bind by line — a scanner that loses one newline
/// silently moves every later suppression out of its window.
///
/// The rule that keeps that honest is one line long: **every advance goes
/// through `skip_to`, and `skip_to` adds the newlines in the span it steps
/// over**. No arm counts lines itself, so a new literal form cannot forget.
pub fn rs_comment_lines(source: &str) -> Vec<(usize, String)> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut at = 0usize;
    let mut line = 1usize;
    while at < bytes.len() {
        let end = match bytes[at] {
            b'/' if bytes.get(at + 1) == Some(&b'/') => {
                let end = line_comment_end(bytes, at);
                out.push((line, source[at..end].to_string()));
                end
            }
            b'/' if bytes.get(at + 1) == Some(&b'*') => {
                let end = block_comment_end(bytes, at);
                out.push((line, source[at..end].to_string()));
                end
            }
            b'"' => literal_end(bytes, at),
            b'\'' if is_char_literal(bytes, at) => literal_end(bytes, at),
            b'r' => raw_string_end(bytes, at).unwrap_or(at + 1),
            _ => at + 1,
        };
        line += bytes[at..end].iter().filter(|b| **b == b'\n').count();
        at = end;
    }
    out
}

/// The index of the newline that ends the line comment starting at `at`, or the
/// end of the source when it is the last line.
fn line_comment_end(bytes: &[u8], at: usize) -> usize {
    bytes[at..]
        .iter()
        .position(|b| *b == b'\n')
        .map_or(bytes.len(), |k| at + k)
}

/// The index just past the `*/` that closes the block comment starting at `at`.
fn block_comment_end(bytes: &[u8], at: usize) -> usize {
    let mut j = at + 2;
    while j + 1 < bytes.len() {
        if bytes[j] == b'*' && bytes[j + 1] == b'/' {
            return j + 2;
        }
        j += 1;
    }
    bytes.len()
}

/// The index just past the quoted literal that starts at `at` (`"…"` or
/// `'…'`), honouring backslash escapes. An unterminated literal runs to the end.
fn literal_end(bytes: &[u8], at: usize) -> usize {
    let quote = bytes[at];
    let mut j = at + 1;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' => j = (j + 2).min(bytes.len()),
            b if b == quote => return j + 1,
            _ => j += 1,
        }
    }
    bytes.len()
}

/// The index just past the raw string that starts at `at` (`r"…"`, `r#"…"#`),
/// or None when those bytes are not a raw string (`r` used as a name).
fn raw_string_end(bytes: &[u8], at: usize) -> Option<usize> {
    let mut j = at + 1;
    let mut hashes = 0usize;
    while bytes.get(j) == Some(&b'#') {
        hashes += 1;
        j += 1;
    }
    if bytes.get(j) != Some(&b'"') {
        return None;
    }
    j += 1;
    while j < bytes.len() {
        let closes = bytes[j] == b'"'
            && bytes
                .get(j + 1..j + 1 + hashes)
                .is_some_and(|tail| tail.iter().all(|b| *b == b'#'));
        if closes {
            return Some(j + 1 + hashes);
        }
        j += 1;
    }
    Some(bytes.len())
}

/// A `'` that opens a char literal (`'x'`, `'\n'`). A bare `'a` is a lifetime:
/// treating it as a literal would swallow to the next apostrophe and lose every
/// comment in between.
fn is_char_literal(bytes: &[u8], at: usize) -> bool {
    bytes.get(at + 2) == Some(&b'\'') || (bytes.get(at + 1) == Some(&b'\\') && bytes.get(at + 3) == Some(&b'\''))
}

// --------------------------------------------------------------------------- duplicate skeletons
