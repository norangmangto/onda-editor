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
///
/// Grammar crates target the `tree-sitter-language` stable-ABI layer and expose
/// `LANGUAGE: LanguageFn`; tree-sitter 0.23+ implements `From<LanguageFn> for
/// Language`, so a plain `.into()` bridges the gap (no `unsafe` transmute needed).
fn ts_language(language_name: &str) -> Option<tree_sitter::Language> {
    match language_name {
        "rust" => Some(tree_sitter_rust::LANGUAGE.into()),
        "python" => Some(tree_sitter_python::LANGUAGE.into()),
        "json" => Some(tree_sitter_json::LANGUAGE.into()),
        "toml" => Some(tree_sitter_toml_ng::LANGUAGE.into()),
        "yaml" => Some(tree_sitter_yaml::LANGUAGE.into()),
        "markdown" => Some(tree_sitter_md::LANGUAGE.into()),
        "hcl" => Some(tree_sitter_hcl::LANGUAGE.into()),
        "bash" => Some(tree_sitter_bash::LANGUAGE.into()),
        "make" => Some(tree_sitter_make::LANGUAGE.into()),
        "go" => Some(tree_sitter_go::LANGUAGE.into()),
        "javascript" => Some(tree_sitter_javascript::LANGUAGE.into()),
        "typescript" => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        "html" => Some(tree_sitter_html::LANGUAGE.into()),
        "css" => Some(tree_sitter_css::LANGUAGE.into()),
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

/// Map a tree-sitter node kind to a `Scope`, for Markdown (tree-sitter-md block grammar).
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

/// Map a tree-sitter node kind to a `Scope`, for HCL / Terraform source.
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

/// Map a tree-sitter node kind to a `Scope`, for shell/bash source.
fn bash_scope(kind: &str) -> Option<Scope> {
    match kind {
        "if" | "then" | "else" | "elif" | "fi" | "for" | "while" | "until" | "do" | "done"
        | "case" | "esac" | "in" | "function" | "select" | "return" | "local" | "declare"
        | "export" | "readonly" | "unset" => Some(Scope::Keyword),
        "comment" => Some(Scope::Comment),
        "string" | "raw_string" | "ansi_c_string" | "heredoc_body" => Some(Scope::String),
        "number" => Some(Scope::Number),
        "command_name" => Some(Scope::Function),
        "variable_name" | "special_variable_name" => Some(Scope::Variable),
        "=" => Some(Scope::Operator),
        "{" | "}" | "(" | ")" | ";" => Some(Scope::Punctuation),
        "ERROR" => Some(Scope::Error),
        _ => None,
    }
}

/// Map a tree-sitter node kind to a `Scope`, for Makefiles.
fn make_scope(kind: &str) -> Option<Scope> {
    match kind {
        "comment" => Some(Scope::Comment),
        "variable_reference" => Some(Scope::Variable),
        "targets" => Some(Scope::Function),
        "=" | ":=" | "?=" | "+=" => Some(Scope::Operator),
        "$" | "(" | ")" | ":" => Some(Scope::Punctuation),
        "ERROR" => Some(Scope::Error),
        _ => None,
    }
}

/// Map a tree-sitter node kind to a `Scope`, for Go source.
fn go_scope(kind: &str) -> Option<Scope> {
    match kind {
        "package" | "import" | "func" | "return" | "if" | "else" | "for" | "range" | "type"
        | "struct" | "interface" | "map" | "chan" | "go" | "defer" | "var" | "const"
        | "switch" | "case" | "default" | "break" | "continue" | "fallthrough" | "select"
        | "goto" => Some(Scope::Keyword),
        "comment" => Some(Scope::Comment),
        "interpreted_string_literal" | "raw_string_literal" | "rune_literal" => Some(Scope::String),
        "int_literal" | "float_literal" | "imaginary_literal" => Some(Scope::Number),
        "true" | "false" | "nil" | "iota" => Some(Scope::Constant),
        "type_identifier" | "package_identifier" => Some(Scope::Type),
        "field_identifier" => Some(Scope::Variable),
        ":=" | "=" | "==" | "!=" | "<" | ">" | "<=" | ">=" | "&&" | "||" | "!" | "+" | "-"
        | "*" | "/" | "%" | "<-" | "..." => Some(Scope::Operator),
        "(" | ")" | "{" | "}" | "[" | "]" | "," | ";" | "." | ":" => Some(Scope::Punctuation),
        "ERROR" => Some(Scope::Error),
        _ => None,
    }
}

/// Map a tree-sitter node kind to a `Scope`, for JavaScript source.
fn javascript_scope(kind: &str) -> Option<Scope> {
    match kind {
        "const" | "let" | "var" | "function" | "return" | "if" | "else" | "for" | "while"
        | "do" | "switch" | "case" | "default" | "break" | "continue" | "class" | "extends"
        | "new" | "delete" | "typeof" | "instanceof" | "in" | "of" | "await" | "async"
        | "yield" | "throw" | "try" | "catch" | "finally" | "import" | "export" | "from"
        | "this" | "super" => Some(Scope::Keyword),
        "comment" => Some(Scope::Comment),
        "string" | "template_string" => Some(Scope::String),
        "number" => Some(Scope::Number),
        "true" | "false" | "null" | "undefined" => Some(Scope::Constant),
        "=" | "==" | "===" | "!=" | "!==" | "<" | ">" | "<=" | ">=" | "&&" | "||" | "!" | "+"
        | "-" | "*" | "/" | "%" | "=>" | "??" | "..." => Some(Scope::Operator),
        "(" | ")" | "{" | "}" | "[" | "]" | "," | ";" | "." | ":" => Some(Scope::Punctuation),
        "ERROR" => Some(Scope::Error),
        _ => None,
    }
}

/// Map a tree-sitter node kind to a `Scope`, for TypeScript source.
fn typescript_scope(kind: &str) -> Option<Scope> {
    match kind {
        "const" | "let" | "var" | "function" | "return" | "if" | "else" | "for" | "while"
        | "class" | "extends" | "implements" | "interface" | "type" | "enum" | "namespace"
        | "new" | "typeof" | "keyof" | "as" | "in" | "of" | "public" | "private"
        | "protected" | "readonly" | "static" | "abstract" | "async" | "await" | "import"
        | "export" | "from" | "declare" => Some(Scope::Keyword),
        "comment" => Some(Scope::Comment),
        "string" | "template_string" => Some(Scope::String),
        "number" => Some(Scope::Number),
        "true" | "false" | "null" | "undefined" => Some(Scope::Constant),
        "type_identifier" | "predefined_type" => Some(Scope::Type),
        "=" | "==" | "===" | "!=" | "!==" | "<" | ">" | "<=" | ">=" | "&&" | "||" | "!" | "+"
        | "-" | "*" | "/" | "%" | "=>" | "??" | "..." => Some(Scope::Operator),
        "(" | ")" | "{" | "}" | "[" | "]" | "," | ";" | "." | ":" => Some(Scope::Punctuation),
        "ERROR" => Some(Scope::Error),
        _ => None,
    }
}

/// Map a tree-sitter node kind to a `Scope`, for HTML source.
fn html_scope(kind: &str) -> Option<Scope> {
    match kind {
        "comment" => Some(Scope::Comment),
        "tag_name" => Some(Scope::Keyword),
        "attribute_name" => Some(Scope::Attribute),
        "quoted_attribute_value" | "attribute_value" => Some(Scope::String),
        "doctype" => Some(Scope::Constant),
        "<" | ">" | "</" | "/>" | "=" => Some(Scope::Punctuation),
        "ERROR" | "erroneous_end_tag_name" => Some(Scope::Error),
        _ => None,
    }
}

/// Map a tree-sitter node kind to a `Scope`, for CSS source.
fn css_scope(kind: &str) -> Option<Scope> {
    match kind {
        "comment" => Some(Scope::Comment),
        "property_name" => Some(Scope::Attribute),
        "class_name" | "id_name" | "tag_name" => Some(Scope::Type),
        "color_value" => Some(Scope::Constant),
        "integer_value" | "float_value" | "unit" => Some(Scope::Number),
        "string_value" => Some(Scope::String),
        "#" | "." | ":" | ";" | "{" | "}" | "(" | ")" | "," => Some(Scope::Punctuation),
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
        "markdown" => markdown_scope(kind),
        "hcl" => hcl_scope(kind),
        "bash" => bash_scope(kind),
        "make" => make_scope(kind),
        "go" => go_scope(kind),
        "javascript" => javascript_scope(kind),
        "typescript" => typescript_scope(kind),
        "html" => html_scope(kind),
        "css" => css_scope(kind),
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

/// Scopes cycled across CSV/TSV columns so adjacent columns read as distinct colors.
const CSV_COLUMN_SCOPES: [Scope; 6] = [
    Scope::Type,
    Scope::Function,
    Scope::String,
    Scope::Number,
    Scope::Constant,
    Scope::Variable,
];

/// Push a tinted span for one CSV/TSV cell, skipping empty cells.
fn push_csv_cell(spans: &mut Vec<Span>, start: usize, end: usize, col: usize) {
    if start < end {
        spans.push(Span {
            start,
            end,
            scope: CSV_COLUMN_SCOPES[col % CSV_COLUMN_SCOPES.len()],
        });
    }
}

/// Tint CSV/TSV columns: split each line on `delimiter` and tag each non-empty cell
/// with a rotating `Scope`. Byte offsets are absolute (matching the tree-sitter path).
///
/// This is a lightweight, non-grammar highlighter: it does not honour RFC-4180 quoted
/// fields (a delimiter inside quotes still splits). Adequate for column tinting;
/// structured CSV work uses the `:table` view.
fn csv_highlights(rope: &Rope, delimiter: char, version: u64) -> Highlights {
    let mut spans = Vec::new();
    let text = rope.to_string();
    let mut line_start = 0usize; // absolute byte offset of the current line
    for line in text.split_inclusive('\n') {
        let content = line.strip_suffix('\n').unwrap_or(line);
        let content = content.strip_suffix('\r').unwrap_or(content);
        let mut col = 0usize;
        let mut cell_start = line_start;
        let mut byte = line_start;
        for ch in content.chars() {
            if ch == delimiter {
                push_csv_cell(&mut spans, cell_start, byte, col);
                col += 1;
                byte += ch.len_utf8();
                cell_start = byte;
            } else {
                byte += ch.len_utf8();
            }
        }
        push_csv_cell(&mut spans, cell_start, byte, col);
        line_start += line.len();
    }
    Highlights { spans, version }
}

/// Parse the rope and return `Highlights`, or `None` if the language is not
/// supported or parsing fails.
pub fn parse_highlights(rope: &Rope, language_name: &str, version: u64) -> Option<Highlights> {
    // CSV/TSV have no tree-sitter grammar; tint columns directly.
    match language_name {
        "csv" => return Some(csv_highlights(rope, ',', version)),
        "tsv" => return Some(csv_highlights(rope, '\t', version)),
        _ => {}
    }

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
        assert!(ts_language("markdown").is_some());
        assert!(ts_language("hcl").is_some());
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
    fn markdown_scope_mapping() {
        assert_eq!(markdown_scope("atx_h1_marker"), Some(Scope::Keyword));
        assert_eq!(markdown_scope("list_marker_minus"), Some(Scope::Keyword));
        assert_eq!(markdown_scope("code_fence_content"), Some(Scope::String));
        assert_eq!(markdown_scope("info_string"), Some(Scope::Attribute));
        assert_eq!(markdown_scope("block_quote_marker"), Some(Scope::Comment));
        assert_eq!(markdown_scope("ERROR"), Some(Scope::Error));
        assert_eq!(markdown_scope("paragraph"), None);
    }

    #[test]
    fn hcl_scope_mapping() {
        assert_eq!(hcl_scope("comment"), Some(Scope::Comment));
        assert_eq!(hcl_scope("string_lit"), Some(Scope::String));
        assert_eq!(hcl_scope("numeric_lit"), Some(Scope::Number));
        assert_eq!(hcl_scope("bool_lit"), Some(Scope::Constant));
        assert_eq!(hcl_scope("identifier"), Some(Scope::Variable));
        assert_eq!(hcl_scope("=="), Some(Scope::Operator));
        assert_eq!(hcl_scope("ERROR"), Some(Scope::Error));
    }

    #[test]
    fn parse_highlights_produces_spans_for_new_languages() {
        let toml_src = Rope::from_str("[package]\nname = \"foo\"\nversion = \"1.0\"\n# comment\n");
        let toml_hl = parse_highlights(&toml_src, "toml", 1).expect("toml grammar");
        assert!(!toml_hl.spans.is_empty(), "TOML should produce spans");

        let yaml_src = Rope::from_str("key: value\nflag: true\ncount: 42\n# comment\n");
        let yaml_hl = parse_highlights(&yaml_src, "yaml", 1).expect("yaml grammar");
        assert!(!yaml_hl.spans.is_empty(), "YAML should produce spans");

        let md_src = Rope::from_str("# Heading\n\n- item\n\n```rust\nlet x = 1;\n```\n");
        let md_hl = parse_highlights(&md_src, "markdown", 1).expect("markdown grammar");
        assert!(!md_hl.spans.is_empty(), "Markdown should produce spans");

        let hcl_src =
            Rope::from_str("resource \"aws_instance\" \"web\" {\n  count = 2 # n\n}\n");
        let hcl_hl = parse_highlights(&hcl_src, "hcl", 1).expect("hcl grammar");
        assert!(!hcl_hl.spans.is_empty(), "HCL should produce spans");
    }

    #[test]
    fn parse_highlights_produces_spans_for_batch_languages() {
        let cases: &[(&str, &str)] = &[
            ("bash", "#!/bin/bash\n# c\nfor i in 1 2; do\n  echo \"$i\"\ndone\n"),
            ("make", "# c\nCC = gcc\nall: main.o\n\t$(CC) -o app main.o\n"),
            ("go", "package main\nimport \"fmt\"\n// c\nfunc main() { x := 42 }\n"),
            ("javascript", "// c\nconst x = 42;\nfunction f(a) { return a; }\n"),
            ("typescript", "// c\ninterface I { n: number }\nconst x: number = 42;\n"),
            ("html", "<!-- c -->\n<div class=\"a\"><p>hi</p></div>\n"),
            ("css", "/* c */\n.a { color: #fff; width: 100%; }\n"),
        ];
        for (lang, src) in cases {
            let hl = parse_highlights(&Rope::from_str(src), lang, 1)
                .unwrap_or_else(|| panic!("{lang} grammar should be available"));
            assert!(!hl.spans.is_empty(), "{lang} should produce spans");
        }
    }

    #[test]
    fn batch_scope_mappings() {
        assert_eq!(bash_scope("for"), Some(Scope::Keyword));
        assert_eq!(bash_scope("command_name"), Some(Scope::Function));
        assert_eq!(bash_scope("comment"), Some(Scope::Comment));
        assert_eq!(make_scope("variable_reference"), Some(Scope::Variable));
        assert_eq!(make_scope("targets"), Some(Scope::Function));
        assert_eq!(go_scope("func"), Some(Scope::Keyword));
        assert_eq!(go_scope("int_literal"), Some(Scope::Number));
        assert_eq!(go_scope("type_identifier"), Some(Scope::Type));
        assert_eq!(javascript_scope("const"), Some(Scope::Keyword));
        assert_eq!(javascript_scope("template_string"), Some(Scope::String));
        assert_eq!(typescript_scope("interface"), Some(Scope::Keyword));
        assert_eq!(typescript_scope("predefined_type"), Some(Scope::Type));
        assert_eq!(html_scope("tag_name"), Some(Scope::Keyword));
        assert_eq!(html_scope("attribute_name"), Some(Scope::Attribute));
        assert_eq!(css_scope("property_name"), Some(Scope::Attribute));
        assert_eq!(css_scope("color_value"), Some(Scope::Constant));
    }

    #[test]
    fn csv_highlights_tint_columns_by_index() {
        let src = Rope::from_str("a,bb,ccc\n1,2,3\n");
        let hl = parse_highlights(&src, "csv", 1).expect("csv tinting");
        // Row 0 cells: "a" 0..1, "bb" 2..4, "ccc" 5..8 — rotating scopes by column.
        let row0: Vec<_> = hl
            .spans_in_range(0, 8)
            .map(|s| (s.start, s.end, s.scope))
            .collect();
        assert_eq!(row0[0], (0, 1, CSV_COLUMN_SCOPES[0]));
        assert_eq!(row0[1], (2, 4, CSV_COLUMN_SCOPES[1]));
        assert_eq!(row0[2], (5, 8, CSV_COLUMN_SCOPES[2]));
    }

    #[test]
    fn tsv_highlights_use_tab_delimiter() {
        let src = Rope::from_str("x\ty\n");
        let hl = parse_highlights(&src, "tsv", 1).expect("tsv tinting");
        assert_eq!(hl.spans.len(), 2);
        assert_eq!(hl.spans[0].scope, CSV_COLUMN_SCOPES[0]);
        assert_eq!(hl.spans[1].scope, CSV_COLUMN_SCOPES[1]);
    }

    #[test]
    fn csv_skips_empty_cells() {
        let src = Rope::from_str("a,,c\n");
        let hl = parse_highlights(&src, "csv", 1).unwrap();
        // The empty middle cell yields no span; column indices still advance.
        assert_eq!(hl.spans.len(), 2);
        assert_eq!(hl.spans[0].scope, CSV_COLUMN_SCOPES[0]);
        assert_eq!(hl.spans[1].scope, CSV_COLUMN_SCOPES[2]);
    }

}
