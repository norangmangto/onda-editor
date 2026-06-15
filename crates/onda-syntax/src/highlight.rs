use ropey::Rope;
use tree_sitter::{Node, Parser};

use thiserror::Error;

/// Errors from highlight operations.
#[derive(Debug, Error)]
pub enum HighlightError {
    #[error("highlight query failed: {0}")]
    Query(String),
}

/// A single highlight capture: byte range + scope name.
///
/// This is the "legacy" event-based representation kept for compatibility with
/// the existing public API surface exported from `lib.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Highlight {
    pub start_byte: usize,
    pub end_byte: usize,
    pub scope: String,
}

/// Configuration for the highlighter (query sources, etc.).
#[derive(Debug, Default)]
pub struct HighlightConfig {
    // future: compiled highlight queries per language
}

impl HighlightConfig {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Events emitted by incremental highlighting.
#[derive(Debug, Clone)]
pub enum HighlightEvent {
    /// A new highlight scope starts at the given byte offset.
    Start { byte: usize, scope: String },
    /// The most recent highlight scope ends at the given byte offset.
    End { byte: usize },
    /// Source bytes in the given range carry no highlight.
    Source { start: usize, end: usize },
}

// ─────────────────────────────────────────────────────────────────────────────
// Scope / Span / Highlights
// ─────────────────────────────────────────────────────────────────────────────

/// High-level syntactic scopes used for theming.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    Keyword,
    Type,
    Function,
    Variable,
    String,
    Number,
    Comment,
    Operator,
    Punctuation,
    Attribute,
    Constant,
    Error,
}

/// A byte-range span tagged with a `Scope`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub scope: Scope,
}

/// The complete set of highlight spans for one version of a buffer.
#[derive(Debug, Clone)]
pub struct Highlights {
    pub spans: Vec<Span>,
    pub version: u64,
}

impl Highlights {
    /// Returns all spans whose byte range overlaps `[start, end)`.
    pub fn spans_in_range(&self, start: usize, end: usize) -> impl Iterator<Item = &Span> {
        self.spans
            .iter()
            .filter(move |s| s.start < end && s.end > start)
    }

    /// Returns the scope of the first span that contains `byte_offset`, if any.
    pub fn scope_at(&self, byte_offset: usize) -> Option<Scope> {
        self.spans
            .iter()
            .find(|s| s.start <= byte_offset && byte_offset < s.end)
            .map(|s| s.scope)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tree-sitter language resolution
// ─────────────────────────────────────────────────────────────────────────────

/// Return the tree-sitter `Language` for the given language name, if known.
fn ts_language(language_name: &str) -> Option<tree_sitter::Language> {
    match language_name {
        "rust" => Some(tree_sitter_rust::language()),
        "python" => Some(tree_sitter_python::language()),
        "json" => Some(tree_sitter_json::language()),
        // TOML has no bundled grammar in the workspace — skip.
        _ => None,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Node → Scope mapping
// ─────────────────────────────────────────────────────────────────────────────

/// Map a tree-sitter node kind to a `Scope`, for Rust source.
fn rust_scope(kind: &str) -> Option<Scope> {
    match kind {
        // Keywords
        "fn" | "let" | "mut" | "pub" | "use" | "struct" | "enum" | "impl" | "trait" | "return"
        | "if" | "else" | "for" | "while" | "match" | "mod" | "self" | "super" | "crate"
        | "type" | "const" | "static" | "ref" | "move" | "async" | "await" | "dyn" | "where"
        | "extern" | "unsafe" | "break" | "continue" | "loop" | "in" | "as" => Some(Scope::Keyword),

        // Literals
        "string_literal" | "raw_string_literal" | "char_literal" => Some(Scope::String),
        "integer_literal" | "float_literal" => Some(Scope::Number),
        "boolean_literal" => Some(Scope::Constant),

        // Comments
        "line_comment" | "block_comment" => Some(Scope::Comment),

        // Types
        "type_identifier" | "primitive_type" => Some(Scope::Type),

        // Attributes
        "attribute_item" | "inner_attribute_item" => Some(Scope::Attribute),

        // Operators
        "+" | "-" | "*" | "/" | "%" | "==" | "!=" | "<" | ">" | "<=" | ">=" | "&&" | "||" | "!"
        | "&" | "|" | "^" | "<<" | ">>" | "=" | "+=" | "-=" | "*=" | "/=" | "%=" | "&=" | "|="
        | "^=" | "<<=" | ">>=" | "->" | "=>" | ".." | "..=" => Some(Scope::Operator),

        // Punctuation
        "(" | ")" | "[" | "]" | "{" | "}" | "," | ";" | ":" | "::" | "." => {
            Some(Scope::Punctuation)
        }

        // Error nodes
        "ERROR" => Some(Scope::Error),

        _ => None,
    }
}

/// Map a tree-sitter node kind to a `Scope`, for Python source.
fn python_scope(kind: &str) -> Option<Scope> {
    match kind {
        "def" | "class" | "return" | "if" | "elif" | "else" | "for" | "while" | "import"
        | "from" | "as" | "pass" | "break" | "continue" | "with" | "yield" | "lambda" | "and"
        | "or" | "not" | "in" | "is" | "del" | "raise" | "try" | "except" | "finally"
        | "global" | "nonlocal" | "assert" | "async" | "await" => Some(Scope::Keyword),

        "string" | "concatenated_string" | "string_content" => Some(Scope::String),
        "integer" | "float" => Some(Scope::Number),
        "true" | "false" | "none" => Some(Scope::Constant),

        "comment" => Some(Scope::Comment),

        "type" | "type_annotation" => Some(Scope::Type),

        "decorator" => Some(Scope::Attribute),

        "+" | "-" | "*" | "/" | "//" | "%" | "**" | "==" | "!=" | "<" | ">" | "<=" | ">=" | "="
        | "+=" | "-=" | "*=" | "/=" | "//=" | "%=" | "**=" | "&" | "|" | "^" | "~" | "<<"
        | ">>" | "->" => Some(Scope::Operator),

        "(" | ")" | "[" | "]" | "{" | "}" | "," | ":" | "." => Some(Scope::Punctuation),

        "ERROR" => Some(Scope::Error),

        _ => None,
    }
}

/// Map a tree-sitter node kind to a `Scope`, for JSON source.
fn json_scope(kind: &str) -> Option<Scope> {
    match kind {
        "string" | "string_content" => Some(Scope::String),
        "number" => Some(Scope::Number),
        "true" | "false" | "null" => Some(Scope::Constant),
        "{" | "}" | "[" | "]" | ":" | "," => Some(Scope::Punctuation),
        "ERROR" => Some(Scope::Error),
        _ => None,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tree walker
// ─────────────────────────────────────────────────────────────────────────────

fn walk_node(node: Node<'_>, language_name: &str, spans: &mut Vec<Span>) {
    let kind = node.kind();

    // Special case: for Rust `function_item`, tag its `name` child as Function.
    if language_name == "rust" && kind == "function_item" {
        if let Some(name_node) = node.child_by_field_name("name") {
            if name_node.start_byte() < name_node.end_byte() {
                spans.push(Span {
                    start: name_node.start_byte(),
                    end: name_node.end_byte(),
                    scope: Scope::Function,
                });
            }
        }
    }

    // Map the node itself to a scope if applicable.
    let scope_opt = match language_name {
        "rust" => rust_scope(kind),
        "python" => python_scope(kind),
        "json" => json_scope(kind),
        _ => None,
    };

    if let Some(scope) = scope_opt {
        let start = node.start_byte();
        let end = node.end_byte();
        if start < end {
            spans.push(Span { start, end, scope });
        }
    }

    // Recurse into children.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_node(child, language_name, spans);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Public parse entry point
// ─────────────────────────────────────────────────────────────────────────────

/// Parse the rope and return `Highlights`, or `None` if the language is not
/// supported or parsing fails.
pub fn parse_highlights(rope: &Rope, language_name: &str, version: u64) -> Option<Highlights> {
    let ts_lang = ts_language(language_name)?;

    // Collect rope content into a contiguous UTF-8 string for tree-sitter.
    let text: String = rope.to_string();

    let mut parser = Parser::new();
    // INVARIANT: language() returns a valid built-in language; set_language cannot fail for these.
    parser.set_language(&ts_lang).ok()?;

    let tree = parser.parse(text.as_bytes(), None)?;
    let root = tree.root_node();

    let mut spans: Vec<Span> = Vec::new();
    walk_node(root, language_name, &mut spans);

    // Sort by start byte so consumers can binary-search.
    spans.sort_by_key(|s| s.start);

    Some(Highlights { spans, version })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hl() -> Highlights {
        Highlights {
            spans: vec![
                Span {
                    start: 0,
                    end: 2,
                    scope: Scope::Keyword,
                },
                Span {
                    start: 3,
                    end: 7,
                    scope: Scope::Function,
                },
            ],
            version: 1,
        }
    }

    #[test]
    fn scope_at_finds_containing_span() {
        let h = hl();
        assert_eq!(h.scope_at(0), Some(Scope::Keyword));
        assert_eq!(h.scope_at(1), Some(Scope::Keyword));
        assert_eq!(h.scope_at(2), None); // end is exclusive
        assert_eq!(h.scope_at(3), Some(Scope::Function));
        assert_eq!(h.scope_at(100), None);
    }

    #[test]
    fn spans_in_range_overlap() {
        let h = hl();
        let got: Vec<_> = h.spans_in_range(1, 4).map(|s| s.scope).collect();
        assert_eq!(got, vec![Scope::Keyword, Scope::Function]);
        let only_fn: Vec<_> = h.spans_in_range(5, 9).map(|s| s.scope).collect();
        assert_eq!(only_fn, vec![Scope::Function]);
        let none: Vec<_> = h.spans_in_range(2, 3).map(|s| s.scope).collect();
        assert!(none.is_empty());
    }

    #[test]
    fn ts_language_available_for_bundled_only() {
        assert!(ts_language("rust").is_some());
        assert!(ts_language("python").is_some());
        assert!(ts_language("json").is_some());
        assert!(ts_language("toml").is_none()); // no bundled grammar
        assert!(ts_language("go").is_none());
        assert!(ts_language("nonsense").is_none());
    }

    #[test]
    fn rust_scope_mapping() {
        assert_eq!(rust_scope("fn"), Some(Scope::Keyword));
        assert_eq!(rust_scope("string_literal"), Some(Scope::String));
        assert_eq!(rust_scope("integer_literal"), Some(Scope::Number));
        assert_eq!(rust_scope("line_comment"), Some(Scope::Comment));
        assert_eq!(rust_scope("type_identifier"), Some(Scope::Type));
        assert_eq!(rust_scope("ERROR"), Some(Scope::Error));
        assert_eq!(rust_scope("some_unmapped_kind"), None);
    }

    #[test]
    fn python_scope_mapping() {
        assert_eq!(python_scope("def"), Some(Scope::Keyword));
        assert_eq!(python_scope("return"), Some(Scope::Keyword));
    }
}
