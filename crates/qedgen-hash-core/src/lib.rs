//! Canonical hashing core shared by the `qedgen` CLI and the
//! `qedgen-macros` proc macro (T3, finding F1).
//!
//! Before this crate existed, `qedgen/src/spec/spec_hash.rs` and
//! `qedgen-macros/src/{spec_bind,verified}.rs` were hand-kept
//! byte-for-byte mirrors. That drift class already bit once:
//! `qedgen check --update-hashes` wrote `to_string()`-based hashes the
//! proc-macro (using `canonical_token_string`) immediately rejected.
//! Both crates now call THESE implementations, so agreement holds by
//! construction — a change here recompiles both sides.
//!
//! Hash values are load-bearing: every checked-in
//! `#[qed(verified, hash = …, spec_hash = …)]` stamp encodes this
//! algorithm's output. See `tests/stamp_crosscheck.rs`, which re-derives
//! stamps from `examples/` — any behavior change here fails it.

use sha2::{Digest, Sha256};

/// SHA-256 hash of a string, truncated to 16 hex characters.
pub fn sha256_hex16(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let full = format!("{:x}", hasher.finalize());
    full[..16].to_string()
}

/// Canonical token string: each token in order, single-space separated.
///
/// Why a custom walker instead of `to_token_stream().to_string()`:
/// rustc-supplied proc-macro `TokenStream`s carry per-`Punct` `Spacing`
/// info plus per-token span metadata that subtly affect the default
/// `to_string` output. Two visually-equivalent functions therefore
/// produce different `to_string` bytes when one was parsed by rustc
/// (preserving source spacing) and the other by
/// `proc_macro2::TokenStream::from_str` (using its own defaults).
/// Forcing `Spacing::Alone` on every Punct narrows but doesn't eliminate
/// the gap (Group/Ident formatting still varies). Only a hand-rolled
/// traversal that emits `<token> ' '` for every token — independent of
/// any built-in formatter — gives a canonical form that depends purely
/// on the syntactic structure, making the CLI-side `body_hash_for_*`
/// agree with the proc-macro's compile-time recomputation regardless of
/// how the function was originally tokenized.
pub fn canonical_token_string(stream: &proc_macro2::TokenStream) -> String {
    use proc_macro2::{Delimiter, TokenTree};
    let mut out = String::new();
    fn walk(stream: proc_macro2::TokenStream, out: &mut String) {
        for tt in stream {
            match tt {
                TokenTree::Group(g) => {
                    let (open, close) = match g.delimiter() {
                        Delimiter::Brace => ('{', '}'),
                        Delimiter::Bracket => ('[', ']'),
                        Delimiter::Parenthesis => ('(', ')'),
                        Delimiter::None => (' ', ' '),
                    };
                    if g.delimiter() != Delimiter::None {
                        out.push(open);
                        out.push(' ');
                    }
                    walk(g.stream(), out);
                    if g.delimiter() != Delimiter::None {
                        out.push(close);
                        out.push(' ');
                    }
                }
                TokenTree::Ident(i) => {
                    out.push_str(&i.to_string());
                    out.push(' ');
                }
                TokenTree::Literal(l) => {
                    out.push_str(&l.to_string());
                    out.push(' ');
                }
                TokenTree::Punct(p) => {
                    out.push(p.as_char());
                    out.push(' ');
                }
            }
        }
    }
    walk(stream.clone(), &mut out);
    out
}

/// `sha256_hex16 ∘ canonical_token_string` — the exact composition behind
/// every body hash and accounts-struct hash. Callers strip outer
/// attributes from the syn item, then hash its token stream through this.
pub fn token_stream_hash(stream: &proc_macro2::TokenStream) -> String {
    sha256_hex16(&canonical_token_string(stream))
}

/// Balanced-brace scan over `bytes`, starting at `open` (the index of the
/// opening `{`), treating `//` line comments, `/* */` block comments, and
/// `"…"` string literals (incl. `\"` escapes) as opaque regions. Returns
/// the index of the matching `}`, or `None` if the block is unterminated.
///
/// This scanner used to be pasted four times (twice per crate — once in
/// each `extract_handler_block`, once in each `spec_context_digest`);
/// this is the single copy.
pub fn scan_balanced_block(bytes: &[u8], open: usize) -> Option<usize> {
    let mut cursor = open;
    let mut depth = 0i32;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut in_str = false;
    while cursor < bytes.len() {
        let b = bytes[cursor];
        if in_line_comment {
            if b == b'\n' {
                in_line_comment = false;
            }
            cursor += 1;
            continue;
        }
        if in_block_comment {
            if b == b'*' && cursor + 1 < bytes.len() && bytes[cursor + 1] == b'/' {
                in_block_comment = false;
                cursor += 2;
                continue;
            }
            cursor += 1;
            continue;
        }
        if in_str {
            if b == b'\\' && cursor + 1 < bytes.len() {
                cursor += 2;
                continue;
            }
            if b == b'"' {
                in_str = false;
            }
            cursor += 1;
            continue;
        }
        if b == b'/' && cursor + 1 < bytes.len() {
            let nxt = bytes[cursor + 1];
            if nxt == b'/' {
                in_line_comment = true;
                cursor += 2;
                continue;
            }
            if nxt == b'*' {
                in_block_comment = true;
                cursor += 2;
                continue;
            }
        }
        if b == b'"' {
            in_str = true;
            cursor += 1;
            continue;
        }
        if b == b'{' {
            depth += 1;
        } else if b == b'}' {
            depth -= 1;
            if depth == 0 {
                return Some(cursor);
            }
        }
        cursor += 1;
    }
    None
}

/// Extract the raw text of a `handler <name> { ... }` block (braces
/// included) via keyword search + balanced-brace scanning
/// (`scan_balanced_block`). Hand-rolled to avoid pulling regex into the
/// proc-macro dependency chain.
pub fn extract_handler_block(source: &str, handler_name: &str) -> Option<String> {
    let needle = "handler";
    let bytes = source.as_bytes();
    let mut search_from = 0;
    while let Some(pos) = source[search_from..].find(needle) {
        let abs = search_from + pos;
        // Require whitespace (or SOF) before and whitespace after the keyword.
        let prev_ok = abs == 0 || bytes[abs - 1].is_ascii_whitespace();
        let after = abs + needle.len();
        if !prev_ok || after >= bytes.len() || !bytes[after].is_ascii_whitespace() {
            search_from = abs + 1;
            continue;
        }
        // Skip whitespace, then capture the identifier (ASCII alnum + `_`).
        let rest = &source[after..];
        let rest_trimmed = rest.trim_start();
        let ws_consumed = rest.len() - rest_trimmed.len();
        let mut id_end = 0;
        for (i, c) in rest_trimmed.char_indices() {
            if c.is_ascii_alphanumeric() || c == '_' {
                id_end = i + c.len_utf8();
            } else {
                break;
            }
        }
        if id_end == 0 {
            search_from = abs + 1;
            continue;
        }
        if &rest_trimmed[..id_end] != handler_name {
            search_from = abs + 1;
            continue;
        }
        // Found the handler: scan forward to the opening brace, then
        // balanced-match to its close.
        let mut cursor = after + ws_consumed + id_end;
        while cursor < bytes.len() && bytes[cursor] != b'{' {
            cursor += 1;
        }
        if cursor >= bytes.len() {
            return None;
        }
        let close = scan_balanced_block(bytes, cursor)?;
        return Some(source[cursor..close + 1].to_string());
    }
    None
}

/// Normalize a handler block before hashing so cosmetic edits don't fire
/// drift while semantic edits do: strip `//` and `/* */` comments,
/// collapse whitespace runs outside strings to one space, trim; string
/// literals (incl. `\"` escapes) pass through verbatim — interior spaces
/// are semantic.
pub fn normalize_spec_block(block: &str) -> String {
    let bytes = block.as_bytes();
    let mut out = String::with_capacity(block.len());
    let mut i = 0;
    let mut in_str = false;
    let mut last_emit_was_ws = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_str {
            out.push(b as char);
            if b == b'\\' && i + 1 < bytes.len() {
                out.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            if b == b'"' {
                in_str = false;
            }
            i += 1;
            last_emit_was_ws = false;
            continue;
        }
        // Line comment
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            // The newline ends the comment; fall through so the
            // whitespace-collapse arm below treats it as a separator.
            continue;
        }
        // Block comment
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = i.saturating_add(2);
            // Treat the comment gap as a single whitespace separator
            // unless we'd otherwise emit two spaces in a row.
            if !out.is_empty() && !last_emit_was_ws {
                out.push(' ');
                last_emit_was_ws = true;
            }
            continue;
        }
        if b == b'"' {
            in_str = true;
            out.push('"');
            i += 1;
            last_emit_was_ws = false;
            continue;
        }
        if b.is_ascii_whitespace() {
            if !out.is_empty() && !last_emit_was_ws {
                out.push(' ');
                last_emit_was_ws = true;
            }
            i += 1;
            continue;
        }
        out.push(b as char);
        last_emit_was_ws = false;
        i += 1;
    }
    out.trim().to_string()
}

/// Digest of everything in `source` *except* handler blocks. Handler
/// blocks are sealed individually by `spec_hash_for_handler`; all other
/// top-level items (`const`, `type`, `pda`, `event`, `errors`,
/// `interface`, `import`, `invariant`, `property`, `environment`) are
/// shared context that changes every handler's effective contract, so
/// this digest is folded into each handler's spec_hash. (GH issue #31.)
///
/// Algorithm: balanced-brace scan (as `extract_handler_block`, collecting
/// all ranges), remove handler ranges, normalize, sha256-hex16.
///
/// Conservative-by-design: a top-level change NO handler references still
/// invalidates every hash — over-invalidates vs. dataflow analysis, but
/// simple and deterministic.
pub fn spec_context_digest(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut search_from = 0;
    let mut last_emit = 0usize;
    let needle = "handler";

    while let Some(pos) = source[search_from..].find(needle) {
        let abs = search_from + pos;
        let prev_ok = abs == 0 || bytes[abs - 1].is_ascii_whitespace();
        let after = abs + needle.len();
        if !prev_ok || after >= bytes.len() || !bytes[after].is_ascii_whitespace() {
            search_from = abs + 1;
            continue;
        }
        // Skip past `handler <name>` to find the opening brace, if any.
        let mut cursor = after;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        // Identifier
        while cursor < bytes.len()
            && (bytes[cursor].is_ascii_alphanumeric() || bytes[cursor] == b'_')
        {
            cursor += 1;
        }
        // Whitespace + optional `(...)` params + `:` + lifecycle clause —
        // everything up to the first `{` that opens the body.
        while cursor < bytes.len() && bytes[cursor] != b'{' {
            cursor += 1;
        }
        if cursor >= bytes.len() {
            search_from = abs + 1;
            continue;
        }
        match scan_balanced_block(bytes, cursor) {
            Some(close) => {
                let block_end = close + 1;
                out.push_str(&source[last_emit..abs]);
                out.push(' ');
                last_emit = block_end;
                search_from = block_end;
            }
            None => {
                // Unterminated handler block — bail out, hash what we have.
                break;
            }
        }
    }
    out.push_str(&source[last_emit..]);
    sha256_hex16(&normalize_spec_block(&out))
}

/// Spec hash for a handler. `None` if the block is absent or the handler
/// is bodyless (`handler foo : A -> B` with no braces — an empty
/// contract). The block is normalized before hashing, and
/// `spec_context_digest(source)` is folded in so top-level shared
/// declarations propagate into every handler's hash.
pub fn spec_hash_for_handler(source: &str, handler_name: &str) -> Option<String> {
    let block = extract_handler_block(source, handler_name)?;
    let normalized = normalize_spec_block(&block);
    let context = spec_context_digest(source);
    Some(sha256_hex16(&format!("{}:{}", normalized, context)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_balanced_block_basic() {
        let src = b"{ a { b } c }";
        assert_eq!(scan_balanced_block(src, 0), Some(src.len() - 1));
    }

    #[test]
    fn scan_balanced_block_ignores_comments_and_strings() {
        let src = r#"{ // }
  /* } */ "}" x }"#;
        let bytes = src.as_bytes();
        assert_eq!(scan_balanced_block(bytes, 0), Some(bytes.len() - 1));
    }

    #[test]
    fn scan_balanced_block_unterminated_is_none() {
        assert_eq!(scan_balanced_block(b"{ a { b }", 0), None);
    }

    #[test]
    fn sha256_hex16_shape() {
        let h = sha256_hex16("hello");
        assert_eq!(h.len(), 16);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(h, sha256_hex16("hello"));
        assert_ne!(h, sha256_hex16("world"));
    }
}
