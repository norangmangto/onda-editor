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

/// Convert a `LanguageFn` to a `tree_sitter::Language`.
///
/// Grammar crates that target the `tree-sitter-language` stable-ABI layer (rather
/// than a specific tree-sitter release) expose `LANGUAGE: LanguageFn` instead of
/// `language() -> tree_sitter::Language`. tree-sitter 0.22 does not yet implement
/// `From<LanguageFn> for Language`, so we bridge the gap here.
///
/// # Safety
/// `LanguageFn` wraps a C function returning `*const TSLanguage` (type-erased as
/// `*const ()`). `tree_sitter::Language` is a single-pointer newtype over
/// `*const TSLanguage`. Both types are pointer-sized with identical representation,
/// making the transmute sound for any grammar produced by the tree-sitter CLI.
unsafe fn lang_fn(f: tree_sitter_language::LanguageFn) -> tree_sitter::Language {
    std::mem::transmute(f.into_raw()())
}

/// Return the tree-sitter `Language` for the given language name, if known.
fn ts_language(language_name: &str) -> Option<tree_sitter::Language> {
    match language_name {
        "rust" => Some(tree_sitter_rust::language()),
        "python" => Some(tree_sitter_python::language()),
        "json" => Some(tree_sitter_json::language()),
        // SAFETY: see lang_fn
        "toml" => Some(unsafe { lang_fn(tree_sitter_toml_ng::LANGUAGE) }),
        "yaml" => Some(unsafe { lang_fn(tree_sitter_yaml::LANGUAGE) }),
        // "markdown" and "hcl" grammars use ABI version 15 (tree-sitter-md, tree-sitter-hcl
        // >=1.0) which requires tree-sitter >=0.23. The extensions are registered in
        // LanguageRegistry so files open correctly; highlighting is deferred until the
        // workspace upgrades past 0.22.
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

/// Map a tree-sitter node kind to a `Scope`, for TOML source.
fn toml_scope(kind: &str) -> Option<Scope> {
    match kind {
        "comment" => Some(Scope::Comment),
        "string" => Some(Scope::String),
        "integer" | "float" | "local_date" | "local_date_time" | "local_time"
        | "offset_date_time" => Some(Scope::Number),
        "boolean" => Some(Scope::Constant),
        "bare_key" | "quoted_key" => Some(Scope::Type),
        "ERROR" => Some(Scope::Error),
        _ => None,
    }
}

/// Map a tree-sitter node kind to a `Scope`, for YAML source.
fn yaml_scope(kind: &str) -> Option<Scope> {
    match kind {
        "comment" => Some(Scope::Comment),
        "double_quote_scalar" | "single_quote_scalar" | "block_scalar" | "string_scalar" => {
            Some(Scope::String)
        }
        "integer_scalar" | "float_scalar" | "timestamp_scalar" => Some(Scope::Number),
        "boolean_scalar" | "null_scalar" => Some(Scope::Constant),
        "anchor" | "alias" | "tag" => Some(Scope::Attribute),
        "ERROR" => Some(Scope::Error),
        _ => None,
    }
}

// Scope functions for markdown and HCL are prepared but not yet wired to a tree-sitter
// language because the grammar crates that support those languages (tree-sitter-md,
// tree-sitter-hcl) require ABI version 15 (tree-sitter >=0.23). They will be activated
// when the workspace upgrades past 0.22.  The scope function bodies are kept here so
// the mapping logic isn't lost; they are dead code until then.
#[allow(dead_code)]
fn markdown_scope(kind: &str) -> Option<Scope> {
    match kind {
        "atx_h1_marker" | "atx_h2_marker" | "atx_h3_marker" | "atx_h4_marker"
        | "atx_h5_marker" | "atx_h6_marker" | "setext_h1_underline" | "setext_h2_underline" => {
            Some(Scope::Keyword)
        }
        "list_marker_dot"
        | "list_marker_minus"
        | "list_marker_parenthesis"
        | "list_marker_plus"
        | "list_marker_star" => Some(Scope::Keyword),
        "code_fence_content" | "indented_code_block" => Some(Scope::String),
        "fenced_code_block_delimiter" | "thematic_break" => Some(Scope::Operator),
        "link_destination" | "link_title" => Some(Scope::String),
        "link_label" => Some(Scope::Variable),
        "info_string" | "language" => Some(Scope::Attribute),
        "block_quote_marker" => Some(Scope::Comment),
        "ERROR" => Some(Scope::Error),
        _ => None,
    }
}

#[allow(dead_code)]
fn hcl_scope(kind: &str) -> Option<Scope> {
    match kind {
        "comment" => Some(Scope::Comment),
        "string_lit" | "template_literal" | "heredoc_template" | "quoted_template_start"
        | "quoted_template_end" => Some(Scope::String),
        "numeric_lit" => Some(Scope::Number),
        "bool_lit" => Some(Scope::Constant),
        "null_lit" => Some(Scope::Constant),
        "identifier" => Some(Scope::Variable),
        "{" | "}" | "[" | "]" | "(" | ")" | "," | "." => Some(Scope::Punctuation),
        "=" | "==" | "!=" | "<" | ">" | "<=" | ">=" | "&&" | "||" | "!" | "+" | "-" | "*"
        | "/" | "%" => Some(Scope::Operator),
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
        "toml" => toml_scope(kind),
        "yaml" => yaml_scope(kind),
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
        assert!(ts_language("toml").is_some());
        assert!(ts_language("yaml").is_some());
        // markdown + hcl grammars require tree-sitter ABI 15 (>=0.23) — deferred.
        assert!(ts_language("markdown").is_none());
        assert!(ts_language("hcl").is_none());
        assert!(ts_language("go").is_none()); // not bundled
        assert!(ts_language("csv").is_none()); // plain text, no grammar
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

    #[test]
    fn toml_scope_mapping() {
        assert_eq!(toml_scope("comment"), Some(Scope::Comment));
        assert_eq!(toml_scope("string"), Some(Scope::String));
        assert_eq!(toml_scope("integer"), Some(Scope::Number));
        assert_eq!(toml_scope("float"), Some(Scope::Number));
        assert_eq!(toml_scope("boolean"), Some(Scope::Constant));
        assert_eq!(toml_scope("bare_key"), Some(Scope::Type));
        assert_eq!(toml_scope("quoted_key"), Some(Scope::Type));
        assert_eq!(toml_scope("ERROR"), Some(Scope::Error));
    }

    #[test]
    fn yaml_scope_mapping() {
        assert_eq!(yaml_scope("comment"), Some(Scope::Comment));
        assert_eq!(yaml_scope("double_quote_scalar"), Some(Scope::String));
        assert_eq!(yaml_scope("integer_scalar"), Some(Scope::Number));
        assert_eq!(yaml_scope("boolean_scalar"), Some(Scope::Constant));
        assert_eq!(yaml_scope("null_scalar"), Some(Scope::Constant));
        assert_eq!(yaml_scope("anchor"), Some(Scope::Attribute));
        assert_eq!(yaml_scope("ERROR"), Some(Scope::Error));
    }

    #[test]
    fn parse_highlights_produces_spans_for_new_languages() {
        let toml_src = Rope::from_str("[package]\nname = \"foo\"\nversion = \"1.0\"\n# comment\n");
        let toml_hl = parse_highlights(&toml_src, "toml", 1).expect("toml grammar");
        assert!(!toml_hl.spans.is_empty(), "TOML should produce spans");

        let yaml_src = Rope::from_str("key: value\nflag: true\ncount: 42\n# comment\n");
        let yaml_hl = parse_highlights(&yaml_src, "yaml", 1).expect("yaml grammar");
        assert!(!yaml_hl.spans.is_empty(), "YAML should produce spans");

        // Markdown and HCL grammars are deferred (ABI 15 needs tree-sitter >=0.23).
        assert!(parse_highlights(&Rope::from_str("# Heading\n"), "markdown", 1).is_none());
        assert!(
            parse_highlights(&Rope::from_str("resource \"r\" \"n\" {}\n"), "hcl", 1).is_none()
        );

        // CSV has no grammar — returns None gracefully.
        assert!(parse_highlights(&Rope::from_str("a,b,c\n1,2,3\n"), "csv", 1).is_none());
    }
}
