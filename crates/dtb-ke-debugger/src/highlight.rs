//! Syntax highlighting for the source pane: tree-sitter grammars → per-line token ranges.
//!
//! gpui-free on purpose — it hands back byte ranges and a coarse [`Token`] class, and the viewer maps
//! those to theme colours. Adding a language means one grammar dependency and one arm in [`config_for`].

use std::ops::Range;

use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

/// The coarse classes the palette has colours for (`syntax-*` roles in `themes/palettes/dtb.toml`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Token {
    Keyword,
    String,
    Comment,
    Function,
    Type,
    Number,
    Constant,
    Attribute,
}

/// Highlight names we ask the grammar for, in [`HighlightConfiguration::configure`] order; the index
/// tree-sitter reports maps back through [`token_for`].
const NAMES: &[&str] = &[
    "keyword",
    "label",
    "function",
    "function.method",
    "function.macro",
    "constructor",
    "type",
    "type.builtin",
    "string",
    "escape",
    "comment",
    "constant",
    "constant.builtin",
    "attribute",
];

fn token_for(name: &str) -> Option<Token> {
    Some(match name {
        "keyword" | "label" => Token::Keyword,
        "function" | "function.method" | "constructor" => Token::Function,
        "function.macro" | "attribute" => Token::Attribute,
        "type" | "type.builtin" => Token::Type,
        "string" | "escape" => Token::String,
        "comment" => Token::Comment,
        "constant" => Token::Constant,
        "constant.builtin" => Token::Number,
        _ => return None,
    })
}

/// A file's highlighting: for each line, its tokens as byte ranges **relative to the line's start**.
#[derive(Clone, Debug, Default)]
pub struct Highlighted {
    pub lines: Vec<Vec<(Range<usize>, Token)>>,
}

fn config_for(path: &str) -> Option<HighlightConfiguration> {
    let ext = path.rsplit('.').next()?.to_ascii_lowercase();
    let mut config = match ext.as_str() {
        "rs" => HighlightConfiguration::new(
            tree_sitter_rust::LANGUAGE.into(),
            "rust",
            tree_sitter_rust::HIGHLIGHTS_QUERY,
            tree_sitter_rust::INJECTIONS_QUERY,
            "",
        )
        .ok()?,
        _ => return None,
    };
    config.configure(NAMES);
    Some(config)
}

/// `None` when there is no grammar for the file (it is then shown plain) or highlighting failed.
pub fn highlight(path: &str, text: &str) -> Option<Highlighted> {
    let config = config_for(path)?;
    let mut highlighter = Highlighter::new();
    let events = highlighter
        .highlight(&config, text.as_bytes(), None, None, |_| None)
        .ok()?;

    // Byte offset of every line start, to cut token spans at newlines.
    let mut starts = vec![0usize];
    starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
    let mut lines: Vec<Vec<(Range<usize>, Token)>> = vec![Vec::new(); starts.len()];

    let mut stack: Vec<Option<Token>> = Vec::new();
    for event in events {
        match event.ok()? {
            HighlightEvent::HighlightStart(h) => {
                stack.push(NAMES.get(h.0).and_then(|n| token_for(n)))
            }
            HighlightEvent::HighlightEnd => {
                stack.pop();
            }
            HighlightEvent::Source { start, end } => {
                // The innermost highlight that names a token wins.
                let Some(token) = stack.iter().rev().find_map(|t| *t) else {
                    continue;
                };
                let first = starts.partition_point(|&s| s <= start) - 1;
                for line in first..starts.len() {
                    let (ls, le) = (
                        starts[line],
                        starts.get(line + 1).map_or(text.len(), |n| *n),
                    );
                    if ls >= end {
                        break;
                    }
                    // Newline itself is not part of the line's text.
                    let le = if text[..le].ends_with('\n') {
                        le - 1
                    } else {
                        le
                    };
                    let (a, b) = (start.max(ls), end.min(le));
                    if a < b {
                        lines[line].push((a - ls..b - ls, token));
                    }
                }
            }
        }
    }
    Some(Highlighted { lines })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens<'a>(h: &'a Highlighted, text: &'a str, line: usize) -> Vec<(&'a str, Token)> {
        let l = text.lines().nth(line).unwrap();
        h.lines[line]
            .iter()
            .map(|(r, t)| (&l[r.clone()], *t))
            .collect()
    }

    #[test]
    fn rust_tokens_land_on_the_right_lines_and_columns() {
        let text = "fn main() {\n    let s = \"hi\"; // note\n    println!(\"{}\", 42);\n}\n";
        let h = highlight("src/main.rs", text).expect("rust grammar");

        let l0 = tokens(&h, text, 0);
        assert!(l0.contains(&("fn", Token::Keyword)), "{l0:?}");
        assert!(l0.contains(&("main", Token::Function)), "{l0:?}");

        let l1 = tokens(&h, text, 1);
        assert!(l1.contains(&("let", Token::Keyword)), "{l1:?}");
        assert!(l1.contains(&("\"hi\"", Token::String)), "{l1:?}");
        assert!(l1.contains(&("// note", Token::Comment)), "{l1:?}");

        let l2 = tokens(&h, text, 2);
        assert!(
            l2.iter()
                .any(|(s, t)| s.starts_with("println") && *t == Token::Attribute),
            "{l2:?}"
        );
        assert!(l2.contains(&("42", Token::Number)), "{l2:?}");
    }

    #[test]
    fn a_multi_line_token_is_cut_at_line_ends() {
        let text = "/* a\n b */ fn f() {}\n";
        let h = highlight("x.rs", text).unwrap();
        assert_eq!(tokens(&h, text, 0), vec![("/* a", Token::Comment)]);
        assert_eq!(tokens(&h, text, 1)[0], (" b */", Token::Comment));
    }

    #[test]
    fn unknown_extensions_are_plain() {
        assert!(highlight("notes.txt", "hello").is_none());
        assert!(highlight("no-extension", "hello").is_none());
    }
}
