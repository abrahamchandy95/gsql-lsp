//! Which text formats the client accepts, and Markdown to plain text.
//!
//! Choice of format: the server writes Markdown. It switches to plain text
//! only when the client declares a list of supported formats and `markdown` is
//! not in it. A client that declares nothing at all (a minimal test client)
//! keeps getting Markdown, as before. Every real editor (VS Code, Neovim,
//! Helix, Zed, eglot, lsp-mode, vim-lsp) lists `markdown` and `plaintext`.
//!
//! Document symbols follow the specification instead: hierarchical symbols
//! are sent only when `hierarchicalDocumentSymbolSupport` is `true`; the flat
//! `SymbolInformation[]` form is the default, which every client supports.

use serde_json::Value;

use super::types::MarkupContent;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientFormats {
    pub hover_markdown: bool,
    pub completion_markdown: bool,
    pub signature_markdown: bool,
    pub hierarchical_symbols: bool,
}

impl Default for ClientFormats {
    /// What a client that sent no capabilities gets.
    fn default() -> ClientFormats {
        ClientFormats {
            hover_markdown: true,
            completion_markdown: true,
            signature_markdown: true,
            hierarchical_symbols: false,
        }
    }
}

/// `false` only when the list at `pointer` is declared and lacks `markdown`.
fn accepts_markdown(capabilities: &Value, pointer: &str) -> bool {
    match capabilities.pointer(pointer).and_then(Value::as_array) {
        Some(formats) => formats.iter().any(|f| f.as_str() == Some("markdown")),
        None => true,
    }
}

impl ClientFormats {
    pub fn from_capabilities(capabilities: &Value) -> ClientFormats {
        ClientFormats {
            hover_markdown: accepts_markdown(capabilities, "/textDocument/hover/contentFormat"),
            completion_markdown: accepts_markdown(
                capabilities,
                "/textDocument/completion/completionItem/documentationFormat",
            ),
            signature_markdown: accepts_markdown(
                capabilities,
                "/textDocument/signatureHelp/signatureInformation/documentationFormat",
            ),
            hierarchical_symbols: capabilities
                .pointer("/textDocument/documentSymbol/hierarchicalDocumentSymbolSupport")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }
    }
}

impl MarkupContent {
    /// The same content as plain text.
    pub fn into_plain(self) -> MarkupContent {
        if self.kind == "markdown" { MarkupContent { kind: "plaintext", value: to_plain(&self.value) } } else { self }
    }
}

/// Markdown as readable plain text: code fences dropped (the code stays),
/// heading markers dropped, list items kept as `- item`, emphasis markers and backticks removed,
/// links written as `text (url)`.
pub fn to_plain(markdown: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        let run = |c: char| trimmed.chars().take_while(|&x| x == c).count();
        if let Some((c, n)) = fence {
            if run(c) >= n && trimmed.trim_start_matches(c).trim().is_empty() {
                fence = None;
            } else {
                out.push(line.to_string());
            }
            continue;
        }
        let opener = [('`', run('`')), ('~', run('~'))].into_iter().find(|&(_, n)| n >= 3);
        if let Some(opener) = opener {
            fence = Some(opener);
            continue;
        }
        let mut text = trimmed;
        let indent = &line[..line.len() - trimmed.len()];
        let hashes = text.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&hashes) && text[hashes..].starts_with(' ') {
            out.push(inline(text[hashes..].trim()));
            continue;
        }
        let mut bullet = "";
        for marker in ["- ", "* ", "+ "] {
            if let Some(rest) = text.strip_prefix(marker) {
                text = rest;
                bullet = "- ";
                break;
            }
        }
        out.push(format!("{indent}{bullet}{}", inline(text)));
    }
    out.join("\n")
}

/// Inline markup of one line.
fn inline(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' if chars.get(i + 1).is_some_and(|n| n.is_ascii_punctuation()) => {
                out.push(chars[i + 1]);
                i += 2;
            }
            '`' => {
                let n = chars[i..].iter().take_while(|&&x| x == '`').count();
                match find_run(&chars, i + n, '`', n) {
                    Some(close) => {
                        out.extend(&chars[i + n..close]);
                        i = close + n;
                    }
                    None => {
                        out.extend(&chars[i..i + n]);
                        i += n;
                    }
                }
            }
            '[' => match link(&chars, i) {
                Some((text, url, end)) => {
                    let text = inline(&text);
                    if text == url || url.is_empty() {
                        out.push_str(&text);
                    } else {
                        out.push_str(&format!("{text} ({url})"));
                    }
                    i = end;
                }
                None => {
                    out.push(c);
                    i += 1;
                }
            },
            '*' | '_' => {
                let n = if chars.get(i + 1) == Some(&c) { 2 } else { 1 };
                let before = if i == 0 { None } else { Some(chars[i - 1]) };
                let after = chars.get(i + n).copied();
                let opens = after.is_some_and(|a| !a.is_whitespace() && a != c)
                    && before.is_none_or(|b| !b.is_alphanumeric() && b != c);
                if opens && let Some(close) = find_closer(&chars, i + n, c, n) {
                    out.push_str(&inline(&chars[i + n..close].iter().collect::<String>()));
                    i = close + n;
                    continue;
                }
                out.extend(std::iter::repeat_n(c, n));
                i += n;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// Start of the next run of exactly `n` `c` characters at or after `from`.
fn find_run(chars: &[char], from: usize, c: char, n: usize) -> Option<usize> {
    let mut i = from;
    while i < chars.len() {
        if chars[i] == c {
            let len = chars[i..].iter().take_while(|&&x| x == c).count();
            if len == n {
                return Some(i);
            }
            i += len;
        } else {
            i += 1;
        }
    }
    None
}

/// Closing marker of emphasis opened before `from`: `n` `c` characters, not
/// preceded by whitespace and (for `_`) not followed by a letter or digit.
/// Code spans are skipped.
fn find_closer(chars: &[char], from: usize, c: char, n: usize) -> Option<usize> {
    let mut i = from;
    while i < chars.len() {
        if chars[i] == '`' {
            let ticks = chars[i..].iter().take_while(|&&x| x == '`').count();
            match find_run(chars, i + ticks, '`', ticks) {
                Some(close) => i = close + ticks,
                None => i += ticks,
            }
        } else if chars[i] == c && chars[i..].iter().take_while(|&&x| x == c).count() >= n {
            let followed = chars.get(i + n).copied();
            let ok = i > from
                && !chars[i - 1].is_whitespace()
                && (c == '*' || followed.is_none_or(|f| !f.is_alphanumeric()))
                && followed != Some(c);
            if ok {
                return Some(i);
            }
            i += n;
        } else {
            i += 1;
        }
    }
    None
}

/// `[text](url)` at `start`: the text, the url and the index after it.
fn link(chars: &[char], start: usize) -> Option<(String, String, usize)> {
    let mut depth = 0;
    let mut close = None;
    for (offset, &c) in chars[start..].iter().enumerate() {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(start + offset);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let end = close + 2 + chars[close + 2..].iter().position(|&c| c == ')')?;
    let text: String = chars[start + 1..close].iter().collect();
    let url: String = chars[close + 2..end].iter().collect();
    Some((text, url.trim().to_string(), end + 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strips_code_fences_and_keeps_the_code() {
        assert_eq!(to_plain("text\n```gsql\nSELECT *\n  FROM x;\n```\nafter"), "text\nSELECT *\n  FROM x;\nafter");
        assert_eq!(to_plain("~~~\n**not bold**\n~~~"), "**not bold**");
    }

    #[test]
    fn fence_content_is_not_interpreted() {
        assert_eq!(to_plain("```\n# not a heading\n- not a list\n```"), "# not a heading\n- not a list");
    }

    #[test]
    fn removes_inline_code_markers() {
        assert_eq!(to_plain("use `a * b` and ``x`y``"), "use a * b and x`y");
        assert_eq!(to_plain("unclosed ` tick"), "unclosed ` tick");
    }

    #[test]
    fn removes_emphasis() {
        assert_eq!(to_plain("**bold** and *italic* and __b__ and _i_"), "bold and italic and b and i");
        assert_eq!(to_plain("*vertex type*"), "vertex type");
        assert_eq!(to_plain("**bold `code` inside**"), "bold code inside");
    }

    #[test]
    fn leaves_non_emphasis_alone() {
        assert_eq!(to_plain("page_rank and my_var_name"), "page_rank and my_var_name");
        assert_eq!(to_plain("2 * 3 * 4"), "2 * 3 * 4");
        assert_eq!(to_plain("@@total*2"), "@@total*2");
        assert_eq!(to_plain("a_b_ and _c"), "a_b_ and _c");
    }

    #[test]
    fn turns_headings_and_lists_into_plain_lines() {
        assert_eq!(to_plain("# Title\n## Sub *x*\ntext"), "Title\nSub x\ntext");
        assert_eq!(to_plain("- one\n* two\n  - three"), "- one\n- two\n  - three");
        assert_eq!(to_plain("#hashtag"), "#hashtag");
    }

    #[test]
    fn writes_links_as_text_and_url() {
        assert_eq!(to_plain("see [the docs](https://x.y/z) now"), "see the docs (https://x.y/z) now");
        assert_eq!(to_plain("[https://x.y](https://x.y)"), "https://x.y");
        assert_eq!(to_plain("array[0] and (x)"), "array[0] and (x)");
    }

    #[test]
    fn unescapes_punctuation() {
        assert_eq!(to_plain(r"a \*b\* c"), "a *b* c");
    }

    #[test]
    fn markup_content_converts_only_markdown() {
        let plain = MarkupContent::markdown("**x**").into_plain();
        assert_eq!((plain.kind, plain.value.as_str()), ("plaintext", "x"));
        assert_eq!(plain.clone().into_plain(), plain);
    }

    #[test]
    fn parses_client_capabilities() {
        // Nothing declared: Markdown, flat symbols.
        let none = ClientFormats::from_capabilities(&json!({}));
        assert_eq!(none, ClientFormats::default());
        assert!(none.hover_markdown && none.completion_markdown && none.signature_markdown);
        assert!(!none.hierarchical_symbols);
        // A full client.
        let full = ClientFormats::from_capabilities(&json!({ "textDocument": {
            "hover": { "contentFormat": ["markdown", "plaintext"] },
            "completion": { "completionItem": { "documentationFormat": ["plaintext", "markdown"] } },
            "signatureHelp": { "signatureInformation": { "documentationFormat": ["markdown"] } },
            "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
        }}));
        assert!(full.hover_markdown && full.completion_markdown && full.signature_markdown);
        assert!(full.hierarchical_symbols);
        // Plain text only.
        let plain = ClientFormats::from_capabilities(&json!({ "textDocument": {
            "hover": { "contentFormat": ["plaintext"] },
            "completion": { "completionItem": { "documentationFormat": [] } },
            "signatureHelp": { "signatureInformation": { "documentationFormat": ["plaintext"] } },
            "documentSymbol": { "dynamicRegistration": true },
        }}));
        assert!(!plain.hover_markdown && !plain.completion_markdown && !plain.signature_markdown);
        assert!(!plain.hierarchical_symbols);
        // Capability objects without the list keep Markdown.
        let objects = ClientFormats::from_capabilities(&json!({ "textDocument": { "hover": {}, "completion": {} } }));
        assert!(objects.hover_markdown && objects.completion_markdown);
    }
}
