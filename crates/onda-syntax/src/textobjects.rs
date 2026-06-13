//! Tree-sitter backed text objects (onda T18.2).
//!
//! Resolves `af`/`if` (function), `ac`/`ic` (class), `aa`/`ia` (argument) targets
//! by parsing the buffer with the language grammar and matching the captures in
//! `runtime/queries/<lang>/textobjects.scm`. When no grammar is available for the
//! language — or the buffer is too large to parse within the keypress budget — the
//! resolver returns `None` and the caller falls back gracefully.

use onda_core::Range;
use ropey::Rope;
use tree_sitter::{Node, Parser, Query, QueryCursor};

/// Buffers larger than this are not parsed synchronously for a text object — the
/// keypress→render budget (10ms) wins over text-object availability. Mirrors rule 2.
const MAX_PARSE_BYTES: usize = 2 * 1024 * 1024;

/// Which syntactic construct a text object targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextObjectKind {
    Function,
    Class,
    Parameter,
}

/// Inner (`i`) vs outer (`a`) variant of a text object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextObjectScope {
    Inner,
    Outer,
}

/// Return the tree-sitter `Language` for `name`, if a grammar is bundled.
fn ts_language(name: &str) -> Option<tree_sitter::Language> {
    match name {
        "rust" => Some(tree_sitter_rust::language()),
        "python" => Some(tree_sitter_python::language()),
        "go" => Some(tree_sitter_go::language()),
        "c" => Some(tree_sitter_c::language()),
        "typescript" => Some(tree_sitter_typescript::language_typescript()),
        "tsx" => Some(tree_sitter_typescript::language_tsx()),
        _ => None,
    }
}

/// Return the embedded `textobjects.scm` source for `name`, if one exists.
fn query_source(name: &str) -> Option<&'static str> {
    match name {
        "rust" => Some(include_str!(
            "../../../runtime/queries/rust/textobjects.scm"
        )),
        "python" => Some(include_str!(
            "../../../runtime/queries/python/textobjects.scm"
        )),
        "go" => Some(include_str!("../../../runtime/queries/go/textobjects.scm")),
        "c" => Some(include_str!("../../../runtime/queries/c/textobjects.scm")),
        "typescript" | "tsx" => Some(include_str!(
            "../../../runtime/queries/typescript/textobjects.scm"
        )),
        _ => None,
    }
}

/// Capture name for a `(kind, scope)` pair, e.g. `function.inner`.
fn capture_name(kind: TextObjectKind, scope: TextObjectScope) -> &'static str {
    match (kind, scope) {
        (TextObjectKind::Function, TextObjectScope::Inner) => "function.inner",
        (TextObjectKind::Function, TextObjectScope::Outer) => "function.outer",
        (TextObjectKind::Class, TextObjectScope::Inner) => "class.inner",
        (TextObjectKind::Class, TextObjectScope::Outer) => "class.outer",
        (TextObjectKind::Parameter, TextObjectScope::Inner) => "parameter.inner",
        (TextObjectKind::Parameter, TextObjectScope::Outer) => "parameter.outer",
    }
}

/// Resolve a tree-sitter text object at char offset `pos`.
///
/// Returns a char `Range` over the document, or `None` when the grammar is
/// unavailable, the buffer is too large, or no matching node contains the cursor.
pub fn text_object(
    rope: &Rope,
    pos: usize,
    language_name: &str,
    kind: TextObjectKind,
    scope: TextObjectScope,
) -> Option<Range> {
    if rope.len_bytes() > MAX_PARSE_BYTES {
        return None;
    }
    let ts_lang = ts_language(language_name)?;
    let query_src = query_source(language_name)?;

    let text: String = rope.to_string();
    let byte_pos = rope.char_to_byte(pos.min(rope.len_chars()));

    let mut parser = Parser::new();
    parser.set_language(&ts_lang).ok()?;
    let tree = parser.parse(text.as_bytes(), None)?;
    let root = tree.root_node();

    let query = Query::new(&ts_lang, query_src).ok()?;

    let node = match kind {
        TextObjectKind::Parameter => {
            resolve_parameter(&query, &root, text.as_bytes(), byte_pos, scope)?
        }
        TextObjectKind::Function | TextObjectKind::Class => {
            resolve_paired(&query, &root, text.as_bytes(), byte_pos, kind, scope)?
        }
    };

    let (start_byte, end_byte) = node;
    let start = rope.byte_to_char(start_byte);
    let end = rope.byte_to_char(end_byte);
    if start >= end {
        return None;
    }
    Some(Range::new(start, end))
}

/// For function/class: find the smallest `<kind>.outer` capture containing the
/// cursor, then return its `outer` or paired `inner` capture from the same match.
/// Matching on the outer node (not the inner body) lets `dif` work even when the
/// cursor sits on the `fn` keyword rather than inside the body.
fn resolve_paired(
    query: &Query,
    root: &Node,
    text: &[u8],
    byte_pos: usize,
    kind: TextObjectKind,
    scope: TextObjectScope,
) -> Option<(usize, usize)> {
    let outer_name = capture_name(kind, TextObjectScope::Outer);
    let inner_name = capture_name(kind, TextObjectScope::Inner);
    let outer_idx = query.capture_index_for_name(outer_name)?;
    let inner_idx = query.capture_index_for_name(inner_name);

    let mut cursor = QueryCursor::new();
    let mut best: Option<(usize, (usize, usize))> = None; // (outer span len, chosen range)

    for m in cursor.matches(query, *root, text) {
        // Locate the outer node in this match and verify it contains the cursor.
        // Matches from other patterns (e.g. parameter) lack this capture — skip them.
        let outer_node = match m.captures.iter().find(|c| c.index == outer_idx) {
            Some(c) => c.node,
            None => continue,
        };
        if !contains(&outer_node, byte_pos) {
            continue;
        }
        let span_len = outer_node.end_byte() - outer_node.start_byte();

        let chosen = match scope {
            TextObjectScope::Outer => (outer_node.start_byte(), outer_node.end_byte()),
            TextObjectScope::Inner => {
                // Prefer the paired inner capture; fall back to the outer node.
                let inner = inner_idx.and_then(|idx| {
                    m.captures
                        .iter()
                        .find(|c| c.index == idx)
                        .map(|c| (c.node.start_byte(), c.node.end_byte()))
                });
                inner.unwrap_or((outer_node.start_byte(), outer_node.end_byte()))
            }
        };

        if best.as_ref().map(|(l, _)| span_len < *l).unwrap_or(true) {
            best = Some((span_len, chosen));
        }
    }

    best.map(|(_, range)| range)
}

/// For parameters/arguments: pick the smallest `parameter.inner` node containing
/// the cursor. Outer additionally absorbs a trailing `,` (or a leading one for the
/// last argument) so `daa` removes the separator too.
fn resolve_parameter(
    query: &Query,
    root: &Node,
    text: &[u8],
    byte_pos: usize,
    scope: TextObjectScope,
) -> Option<(usize, usize)> {
    let inner_idx = query.capture_index_for_name("parameter.inner")?;

    let mut cursor = QueryCursor::new();
    let mut best: Option<Node> = None;

    for m in cursor.matches(query, *root, text) {
        for cap in m.captures.iter().filter(|c| c.index == inner_idx) {
            if !contains(&cap.node, byte_pos) {
                continue;
            }
            let len = cap.node.end_byte() - cap.node.start_byte();
            let is_better = best
                .map(|b| len < (b.end_byte() - b.start_byte()))
                .unwrap_or(true);
            if is_better {
                best = Some(cap.node);
            }
        }
    }

    let node = best?;
    let (start, end) = (node.start_byte(), node.end_byte());
    if scope == TextObjectScope::Inner {
        return Some((start, end));
    }

    // Outer: extend across a trailing separator, else a leading one.
    let mut new_end = end;
    let mut i = end;
    // Skip trailing whitespace.
    while i < text.len() && (text[i] == b' ' || text[i] == b'\t') {
        i += 1;
    }
    if i < text.len() && text[i] == b',' {
        new_end = i + 1;
        // Absorb one following space for tidiness.
        if new_end < text.len() && text[new_end] == b' ' {
            new_end += 1;
        }
        return Some((start, new_end));
    }

    // No trailing separator (likely the last argument) — absorb a preceding one.
    let mut new_start = start;
    let mut j = start;
    while j > 0 && (text[j - 1] == b' ' || text[j - 1] == b'\t') {
        j -= 1;
    }
    if j > 0 && text[j - 1] == b',' {
        new_start = j - 1;
    }
    Some((new_start, new_end))
}

/// True when `node`'s byte range contains `byte_pos` (half-open, with the end
/// treated inclusively so a cursor at the closing delimiter still matches).
#[inline]
fn contains(node: &Node, byte_pos: usize) -> bool {
    node.start_byte() <= byte_pos && byte_pos <= node.end_byte()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(
        src: &str,
        cursor: usize,
        lang: &str,
        kind: TextObjectKind,
        scope: TextObjectScope,
    ) -> Option<String> {
        let rope = Rope::from_str(src);
        let r = text_object(&rope, cursor, lang, kind, scope)?;
        Some(rope.slice(r.from()..r.to()).to_string())
    }

    // Locate the char index of the first occurrence of `needle` in `src`.
    fn at(src: &str, needle: &str) -> usize {
        src.find(needle).expect("needle not found")
    }

    use TextObjectKind::*;
    use TextObjectScope::*;

    // ── Rust ───────────────────────────────────────────────────────────────
    #[test]
    fn rust_function_outer() {
        let src = "fn main() {\n    let x = 1;\n}\n";
        let got = obj(src, at(src, "let"), "rust", Function, Outer).unwrap();
        assert!(got.starts_with("fn main()"));
        assert!(got.trim_end().ends_with('}'));
    }

    #[test]
    fn rust_function_outer_on_keyword() {
        let src = "fn main() {\n    let x = 1;\n}\n";
        // Cursor on the `fn` keyword still selects the whole function.
        let got = obj(src, 0, "rust", Function, Outer).unwrap();
        assert!(got.starts_with("fn main()"));
    }

    #[test]
    fn rust_function_inner() {
        let src = "fn main() {\n    let x = 1;\n}\n";
        let got = obj(src, at(src, "let"), "rust", Function, Inner).unwrap();
        assert!(got.contains("let x = 1;"));
        assert!(!got.starts_with("fn"));
    }

    #[test]
    fn rust_struct_class_outer() {
        let src = "struct Point {\n    x: i32,\n    y: i32,\n}\n";
        let got = obj(src, at(src, "x:"), "rust", Class, Outer).unwrap();
        assert!(got.starts_with("struct Point"));
    }

    #[test]
    fn rust_struct_class_inner() {
        let src = "struct Point {\n    x: i32,\n    y: i32,\n}\n";
        let got = obj(src, at(src, "x:"), "rust", Class, Inner).unwrap();
        assert!(got.contains("x: i32"));
        assert!(!got.contains("struct"));
    }

    #[test]
    fn rust_impl_class() {
        let src = "impl Foo {\n    fn bar(&self) {}\n}\n";
        let got = obj(src, at(src, "fn bar"), "rust", Class, Outer).unwrap();
        assert!(got.starts_with("impl Foo"));
    }

    #[test]
    fn rust_parameter_inner() {
        let src = "fn add(a: i32, b: i32) -> i32 { a + b }\n";
        let got = obj(src, at(src, "a: i32"), "rust", Parameter, Inner).unwrap();
        assert_eq!(got, "a: i32");
    }

    #[test]
    fn rust_parameter_inner_second() {
        let src = "fn add(a: i32, b: i32) -> i32 { a + b }\n";
        let got = obj(src, at(src, "b: i32"), "rust", Parameter, Inner).unwrap();
        assert_eq!(got, "b: i32");
    }

    #[test]
    fn rust_parameter_outer_trailing_comma() {
        let src = "fn add(a: i32, b: i32) -> i32 { a + b }\n";
        let got = obj(src, at(src, "a: i32"), "rust", Parameter, Outer).unwrap();
        assert_eq!(got, "a: i32, ");
    }

    #[test]
    fn rust_call_argument_inner() {
        let src = "fn main() { foo(bar, baz); }\n";
        let got = obj(src, at(src, "baz"), "rust", Parameter, Inner).unwrap();
        assert_eq!(got, "baz");
    }

    #[test]
    fn rust_nested_function_picks_inner() {
        let src = "fn outer() {\n    let f = || {\n        let y = 2;\n    };\n}\n";
        // Cursor inside the closure body — smallest enclosing function wins.
        let got = obj(src, at(src, "let y"), "rust", Function, Outer).unwrap();
        assert!(got.contains("let y = 2;"));
        assert!(got.starts_with("||"));
    }

    #[test]
    fn rust_no_grammar_returns_none() {
        let src = "anything";
        assert!(obj(src, 0, "no-such-lang", Function, Outer).is_none());
    }

    // ── Python ─────────────────────────────────────────────────────────────
    #[test]
    fn python_function_outer() {
        let src = "def foo():\n    return 1\n";
        let got = obj(src, at(src, "return"), "python", Function, Outer).unwrap();
        assert!(got.starts_with("def foo()"));
    }

    #[test]
    fn python_function_inner() {
        let src = "def foo():\n    return 1\n";
        let got = obj(src, at(src, "return"), "python", Function, Inner).unwrap();
        assert!(got.contains("return 1"));
        assert!(!got.contains("def"));
    }

    #[test]
    fn python_class_outer() {
        let src = "class C:\n    def m(self):\n        pass\n";
        let got = obj(src, at(src, "def m"), "python", Class, Outer).unwrap();
        assert!(got.starts_with("class C"));
    }

    #[test]
    fn python_class_inner() {
        let src = "class C:\n    x = 1\n";
        let got = obj(src, at(src, "x ="), "python", Class, Inner).unwrap();
        assert!(got.contains("x = 1"));
        assert!(!got.contains("class"));
    }

    #[test]
    fn python_parameter_inner() {
        let src = "def add(a, b):\n    return a + b\n";
        let got = obj(src, at(src, "b)"), "python", Parameter, Inner).unwrap();
        assert_eq!(got, "b");
    }

    #[test]
    fn python_parameter_outer() {
        let src = "def add(a, b):\n    return a + b\n";
        let got = obj(src, at(src, "a,"), "python", Parameter, Outer).unwrap();
        assert_eq!(got, "a, ");
    }

    #[test]
    fn python_call_argument_inner() {
        let src = "print(value, end='')\n";
        let got = obj(src, at(src, "value"), "python", Parameter, Inner).unwrap();
        assert_eq!(got, "value");
    }

    #[test]
    fn python_lambda_function() {
        let src = "f = lambda x: x + 1\n";
        let got = obj(src, at(src, "x +"), "python", Function, Outer).unwrap();
        assert!(got.starts_with("lambda x"));
    }

    #[test]
    fn python_nested_class_method() {
        let src = "class Outer:\n    def m(self):\n        return 0\n";
        let got = obj(src, at(src, "return"), "python", Function, Outer).unwrap();
        assert!(got.starts_with("def m"));
    }

    #[test]
    fn python_function_empty_body() {
        let src = "def noop():\n    pass\n";
        let got = obj(src, at(src, "pass"), "python", Function, Inner).unwrap();
        assert!(got.contains("pass"));
    }

    // ── Go ─────────────────────────────────────────────────────────────────
    #[test]
    fn go_function_outer() {
        let src = "func add(a int, b int) int {\n\treturn a + b\n}\n";
        let got = obj(src, at(src, "return"), "go", Function, Outer).unwrap();
        assert!(got.starts_with("func add"));
    }

    #[test]
    fn go_function_inner() {
        let src = "func add(a int, b int) int {\n\treturn a + b\n}\n";
        let got = obj(src, at(src, "return"), "go", Function, Inner).unwrap();
        assert!(got.contains("return a + b"));
        assert!(!got.contains("func"));
    }

    #[test]
    fn go_struct_class_outer() {
        let src = "type Point struct {\n\tX int\n\tY int\n}\n";
        let got = obj(src, at(src, "X int"), "go", Class, Outer).unwrap();
        assert!(got.starts_with("type Point struct"));
    }

    #[test]
    fn go_struct_class_inner() {
        let src = "type Point struct {\n\tX int\n\tY int\n}\n";
        let got = obj(src, at(src, "X int"), "go", Class, Inner).unwrap();
        assert!(got.contains("X int"));
        assert!(!got.contains("type"));
    }

    #[test]
    fn go_interface_class() {
        let src = "type R interface {\n\tRead() int\n}\n";
        let got = obj(src, at(src, "Read"), "go", Class, Outer).unwrap();
        assert!(got.starts_with("type R interface"));
    }

    #[test]
    fn go_parameter_inner() {
        let src = "func add(a int, b int) int {\n\treturn 0\n}\n";
        let got = obj(src, at(src, "a int"), "go", Parameter, Inner).unwrap();
        assert_eq!(got, "a int");
    }

    #[test]
    fn go_parameter_outer() {
        let src = "func add(a int, b int) int {\n\treturn 0\n}\n";
        let got = obj(src, at(src, "a int"), "go", Parameter, Outer).unwrap();
        assert_eq!(got, "a int, ");
    }

    #[test]
    fn go_method_function() {
        let src = "func (p *Point) Norm() int {\n\treturn 0\n}\n";
        let got = obj(src, at(src, "return"), "go", Function, Outer).unwrap();
        assert!(got.starts_with("func (p *Point)"));
    }

    #[test]
    fn go_func_literal() {
        let src = "func main() {\n\tf := func() { return }\n}\n";
        let got = obj(src, at(src, "return"), "go", Function, Outer).unwrap();
        assert!(got.starts_with("func()"));
    }

    #[test]
    fn go_call_argument_inner() {
        let src = "func main() {\n\tfmt.Println(x, y)\n}\n";
        let got = obj(src, at(src, "y)"), "go", Parameter, Inner).unwrap();
        assert_eq!(got, "y");
    }

    // ── TypeScript ─────────────────────────────────────────────────────────
    #[test]
    fn ts_function_outer() {
        let src = "function add(a: number, b: number) {\n  return a + b;\n}\n";
        let got = obj(src, at(src, "return"), "typescript", Function, Outer).unwrap();
        assert!(got.starts_with("function add"));
    }

    #[test]
    fn ts_function_inner() {
        let src = "function add(a: number, b: number) {\n  return a + b;\n}\n";
        let got = obj(src, at(src, "return"), "typescript", Function, Inner).unwrap();
        assert!(got.contains("return a + b;"));
        assert!(!got.contains("function"));
    }

    #[test]
    fn ts_class_outer() {
        let src = "class C {\n  m() { return 1; }\n}\n";
        let got = obj(src, at(src, "m()"), "typescript", Class, Outer).unwrap();
        assert!(got.starts_with("class C"));
    }

    #[test]
    fn ts_class_inner() {
        let src = "class C {\n  x = 1;\n}\n";
        let got = obj(src, at(src, "x ="), "typescript", Class, Inner).unwrap();
        assert!(got.contains("x = 1;"));
        assert!(!got.contains("class C"));
    }

    #[test]
    fn ts_method_function() {
        let src = "class C {\n  greet() {\n    return 'hi';\n  }\n}\n";
        let got = obj(src, at(src, "return"), "typescript", Function, Outer).unwrap();
        assert!(got.starts_with("greet()"));
    }

    #[test]
    fn ts_arrow_function() {
        let src = "const f = (x: number) => {\n  return x;\n};\n";
        let got = obj(src, at(src, "return x"), "typescript", Function, Outer).unwrap();
        assert!(got.starts_with("(x: number) =>"));
    }

    #[test]
    fn ts_parameter_inner() {
        let src = "function add(a: number, b: number) {}\n";
        let got = obj(src, at(src, "a: number"), "typescript", Parameter, Inner).unwrap();
        assert_eq!(got, "a: number");
    }

    #[test]
    fn ts_parameter_outer() {
        let src = "function add(a: number, b: number) {}\n";
        let got = obj(src, at(src, "a: number"), "typescript", Parameter, Outer).unwrap();
        assert_eq!(got, "a: number, ");
    }

    #[test]
    fn ts_interface_class() {
        let src = "interface I {\n  x: number;\n}\n";
        let got = obj(src, at(src, "x:"), "typescript", Class, Outer).unwrap();
        assert!(got.starts_with("interface I"));
    }

    #[test]
    fn ts_call_argument_inner() {
        let src = "foo(alpha, beta);\n";
        let got = obj(src, at(src, "beta"), "typescript", Parameter, Inner).unwrap();
        assert_eq!(got, "beta");
    }

    // ── C ──────────────────────────────────────────────────────────────────
    #[test]
    fn c_function_outer() {
        let src = "int add(int a, int b) {\n    return a + b;\n}\n";
        let got = obj(src, at(src, "return"), "c", Function, Outer).unwrap();
        assert!(got.starts_with("int add"));
    }

    #[test]
    fn c_function_inner() {
        let src = "int add(int a, int b) {\n    return a + b;\n}\n";
        let got = obj(src, at(src, "return"), "c", Function, Inner).unwrap();
        assert!(got.contains("return a + b;"));
        assert!(!got.starts_with("int add"));
    }

    #[test]
    fn c_struct_class_outer() {
        let src = "struct Point {\n    int x;\n    int y;\n};\n";
        let got = obj(src, at(src, "int x"), "c", Class, Outer).unwrap();
        assert!(got.starts_with("struct Point"));
    }

    #[test]
    fn c_struct_class_inner() {
        let src = "struct Point {\n    int x;\n    int y;\n};\n";
        let got = obj(src, at(src, "int x"), "c", Class, Inner).unwrap();
        assert!(got.contains("int x;"));
        assert!(!got.contains("struct"));
    }

    #[test]
    fn c_enum_class() {
        let src = "enum Color {\n    RED,\n    GREEN,\n};\n";
        let got = obj(src, at(src, "RED"), "c", Class, Outer).unwrap();
        assert!(got.starts_with("enum Color"));
    }

    #[test]
    fn c_union_class() {
        let src = "union U {\n    int i;\n    float f;\n};\n";
        let got = obj(src, at(src, "int i"), "c", Class, Inner).unwrap();
        assert!(got.contains("int i;"));
    }

    #[test]
    fn c_parameter_inner() {
        let src = "int add(int a, int b) { return 0; }\n";
        // `at("a,")` points at the first parameter; `"int a"` would match the return type.
        let got = obj(src, at(src, "a,"), "c", Parameter, Inner).unwrap();
        assert_eq!(got, "int a");
    }

    #[test]
    fn c_parameter_outer() {
        let src = "int add(int a, int b) { return 0; }\n";
        let got = obj(src, at(src, "a,"), "c", Parameter, Outer).unwrap();
        assert_eq!(got, "int a, ");
    }

    #[test]
    fn c_parameter_inner_second() {
        let src = "int add(int a, int b) { return 0; }\n";
        let got = obj(src, at(src, "int b"), "c", Parameter, Inner).unwrap();
        assert_eq!(got, "int b");
    }

    #[test]
    fn c_call_argument_inner() {
        let src = "int main() { foo(p, q); }\n";
        let got = obj(src, at(src, "q)"), "c", Parameter, Inner).unwrap();
        assert_eq!(got, "q");
    }
}
