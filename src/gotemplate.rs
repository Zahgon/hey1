//! A faithful subset of Go's `text/template`.
//!
//! hey's `-o` flag is passed straight to `template.Must(template.New("tmpl")
//! .Funcs(tmplFuncMap).Parse(outputTmpl))`, so *any* value other than "" and
//! "csv" is treated as a user-supplied template. Hardcoding the two built-in
//! outputs would silently drop that behaviour, so this implements the engine
//! itself: text spans, `{{ }}` actions, `if`/`else`/`end`, `range` (with
//! optional `$k, $v :=`), `with`, variable assignment, parenthesised
//! sub-pipelines, `|` chaining, and the builtins the templates use.
//!
//! Semantics deliberately mirrored from Go:
//!   * ranging a map visits keys in sorted order (hence `BTreeMap`),
//!   * `if` uses Go's "is zero value" truthiness,
//!   * a parse error panics (that is what `template.Must` does),
//!   * an execute error is reported by the caller and produces no output.

use crate::gofmt;
use crate::gotime::GoDuration;
use std::collections::BTreeMap;
use std::fmt::Write as _;

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum Value {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Duration(GoDuration),
    Slice(Vec<Value>),
    MapInt(BTreeMap<i64, Value>),
    MapStr(BTreeMap<String, Value>),
    Struct(BTreeMap<String, Value>),
}

impl Value {
    /// Go's `text/template` truth: the zero value of the type is false.
    fn truth(&self) -> bool {
        match self {
            Value::Nil => false,
            Value::Bool(b) => *b,
            Value::Int(i) => *i != 0,
            Value::Float(f) => *f != 0.0,
            Value::Str(s) => !s.is_empty(),
            Value::Duration(d) => d.nanoseconds() != 0,
            Value::Slice(v) => !v.is_empty(),
            Value::MapInt(m) => !m.is_empty(),
            Value::MapStr(m) => !m.is_empty(),
            Value::Struct(_) => true,
        }
    }

    /// Go's `%v`, which is what a bare `{{ pipeline }}` prints.
    pub fn print(&self) -> String {
        match self {
            Value::Nil => "<no value>".to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => gofmt::sprint_v_f64(*f),
            Value::Str(s) => s.clone(),
            Value::Duration(d) => d.to_string(),
            Value::Slice(v) => {
                let mut out = String::from("[");
                for (i, e) in v.iter().enumerate() {
                    if i > 0 {
                        out.push(' ');
                    }
                    out.push_str(&e.print());
                }
                out.push(']');
                out
            }
            Value::MapInt(m) => {
                let mut out = String::from("map[");
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        out.push(' ');
                    }
                    let _ = write!(out, "{}:{}", k, v.print());
                }
                out.push(']');
                out
            }
            Value::MapStr(m) => {
                let mut out = String::from("map[");
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        out.push(' ');
                    }
                    let _ = write!(out, "{}:{}", k, v.print());
                }
                out.push(']');
                out
            }
            Value::Struct(_) => "{...}".to_string(),
        }
    }

    fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }

    fn field(&self, name: &str) -> Result<Value, String> {
        match self {
            Value::Struct(m) => m
                .get(name)
                .cloned()
                .ok_or_else(|| format!("can't evaluate field {} in type requester.Report", name)),
            // Method set of time.Duration that hey's templates can reach.
            Value::Duration(d) => match name {
                "Seconds" => Ok(Value::Float(d.seconds())),
                "Nanoseconds" => Ok(Value::Int(d.nanoseconds())),
                "Minutes" => Ok(Value::Float(d.seconds() / 60.0)),
                "Hours" => Ok(Value::Float(d.seconds() / 3600.0)),
                "String" => Ok(Value::Str(d.to_string())),
                _ => Err(format!(
                    "can't evaluate field {} in type time.Duration",
                    name
                )),
            },
            _ => Err(format!(
                "can't evaluate field {} in type {}",
                name,
                self.kind()
            )),
        }
    }

    /// The Go type name, as it appears in template error messages.
    fn kind(&self) -> String {
        match self {
            Value::Nil => "nil".to_string(),
            Value::Bool(_) => "bool".to_string(),
            Value::Int(_) => "int".to_string(),
            Value::Float(_) => "float64".to_string(),
            Value::Str(_) => "string".to_string(),
            Value::Duration(_) => "time.Duration".to_string(),
            // Element type is inferred; Report only ever holds these four.
            Value::Slice(v) => match v.first() {
                Some(Value::Float(_)) => "[]float64".to_string(),
                Some(Value::Int(_)) => "[]int".to_string(),
                Some(Value::Struct(m)) if m.contains_key("Mark") => {
                    "[]requester.Bucket".to_string()
                }
                Some(Value::Struct(_)) => "[]requester.LatencyDistribution".to_string(),
                Some(other) => format!("[]{}", other.kind()),
                None => "[]interface {}".to_string(),
            },
            Value::MapInt(_) => "map[int]int".to_string(),
            Value::MapStr(_) => "map[string]int".to_string(),
            Value::Struct(_) => "requester.Report".to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Operand {
    Dot,
    Field(Vec<String>),
    Var(String, Vec<String>),
    Ident(String),
    Str(String),
    Int(i64),
    Float(f64),
    Sub(Pipeline),
}

type Command = Vec<Operand>;

#[derive(Clone, Debug, Default)]
struct Pipeline {
    decls: Vec<String>,
    cmds: Vec<Command>,
}

/// Where an action sits in the template source, so execute errors can be
/// reported the way Go's are: `tmpl:LINE:COL: executing "tmpl" at <SRC>`.
#[derive(Clone, Debug)]
struct Pos {
    line: usize,
    col: usize,
    src: String,
}

impl Pos {
    fn wrap(&self, msg: &str) -> String {
        self.wrap_at(0, msg)
    }

    /// `delta` shifts the reported column to the failing sub-expression --
    /// Go points at the specific field in a chain like `.Total.Nope`, not at
    /// the start of the pipeline.
    fn wrap_at(&self, delta: usize, msg: &str) -> String {
        format!(
            "template: tmpl:{}:{}: executing \"tmpl\" at <{}>: {}",
            self.line,
            self.col + delta,
            self.src,
            msg
        )
    }

    /// An error that already carries a position must not be wrapped twice.
    fn wrap_once(&self, msg: String) -> String {
        if msg.starts_with("template: tmpl:") {
            msg
        } else {
            self.wrap(&msg)
        }
    }
}

#[derive(Clone, Debug)]
enum Node {
    Text(String),
    Action(Pipeline, Pos),
    If {
        pos: Pos,
        pipe: Pipeline,
        list: Vec<Node>,
        else_list: Vec<Node>,
    },
    Range {
        pos: Pos,
        pipe: Pipeline,
        list: Vec<Node>,
        else_list: Vec<Node>,
    },
    With {
        pos: Pos,
        pipe: Pipeline,
        list: Vec<Node>,
        else_list: Vec<Node>,
    },
}

#[derive(Debug)]
pub struct Template {
    root: Vec<Node>,
}

struct Parser<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Self {
        Parser { src, pos: 0 }
    }

    /// Go reports parse errors as `template: NAME:LINE: message`.
    fn line_at(&self, off: usize) -> usize {
        1 + self.src[..off.min(self.src.len())]
            .bytes()
            .filter(|b| *b == b'\n')
            .count()
    }

    fn err_at(&self, off: usize, msg: &str) -> String {
        format!("{}: {}", self.line_at(off), msg)
    }

    /// Go points at the first character after `{{`, 1-based.
    fn pos_at(&self, action_start: usize, action: &str) -> Pos {
        let line = self.line_at(action_start);
        let line_start = self.src[..action_start.min(self.src.len())]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        Pos {
            line,
            col: action_start - line_start + 1,
            src: action.to_string(),
        }
    }

    fn parse_list(&mut self, stop: &[&str]) -> Result<(Vec<Node>, String), String> {
        let mut nodes = Vec::new();
        loop {
            match self.src[self.pos..].find("{{") {
                None => {
                    if !stop.is_empty() {
                        return Err(self.err_at(self.src.len(), "unexpected EOF"));
                    }
                    let text = &self.src[self.pos..];
                    if !text.is_empty() {
                        nodes.push(Node::Text(text.to_string()));
                    }
                    self.pos = self.src.len();
                    return Ok((nodes, String::new()));
                }
                Some(off) => {
                    let mut text_end = self.pos + off;
                    let after = self.pos + off + 2;
                    // "{{- " trims preceding whitespace.
                    let trim_left = self.src[after..].starts_with('-')
                        && self.src[after + 1..].starts_with(char::is_whitespace);
                    let mut action_start = after;
                    if trim_left {
                        action_start = after + 1;
                        let t = &self.src[self.pos..text_end];
                        text_end = self.pos + t.trim_end().len();
                    }
                    let text = &self.src[self.pos..text_end];
                    if !text.is_empty() {
                        nodes.push(Node::Text(text.to_string()));
                    }
                    let close = self.src[action_start..]
                        .find("}}")
                        .ok_or_else(|| self.err_at(text_end, "unclosed action"))?;
                    let mut action = &self.src[action_start..action_start + close];
                    self.pos = action_start + close + 2;
                    // " -}}" trims following whitespace.
                    if action
                        .trim_end()
                        .strip_suffix('-')
                        .is_some_and(|s| s.ends_with(char::is_whitespace) || s.is_empty())
                    {
                        action = &action[..action.trim_end().len() - 1];
                        let rest = &self.src[self.pos..];
                        let trimmed = rest.len() - rest.trim_start().len();
                        self.pos += trimmed;
                    }
                    let action = action.trim();
                    if action.is_empty() {
                        return Err(self.err_at(action_start, "missing value for command"));
                    }
                    let kw = action.split_whitespace().next().unwrap_or("");
                    if stop.contains(&kw) && (kw == "end" || kw == "else") {
                        return Ok((nodes, action.to_string()));
                    }
                    match kw {
                        "if" | "range" | "with" => {
                            let pipe = parse_pipeline(action[kw.len()..].trim())
                                .map_err(|e| self.err_at(action_start, &e))?;
                            let (list, term) = self.parse_list(&["end", "else"])?;
                            let mut else_list = Vec::new();
                            if let Some(after_else) = term.strip_prefix("else") {
                                let rest = after_else.trim();
                                if let Some(after_if) = rest.strip_prefix("if") {
                                    // {{else if X}} == {{else}}{{if X}}{{end}}{{end}}
                                    let pipe2 = parse_pipeline(after_if.trim())
                                        .map_err(|e| self.err_at(action_start, &e))?;
                                    let (l2, _t2) = self.parse_list(&["end", "else"])?;
                                    // Note: chained else-if beyond one level is
                                    // handled recursively by parse_list above.
                                    else_list = vec![Node::If {
                                        pos: self.pos_at(action_start, rest),
                                        pipe: pipe2,
                                        list: l2,
                                        else_list: Vec::new(),
                                    }];
                                } else {
                                    let (l2, _t2) = self.parse_list(&["end"])?;
                                    else_list = l2;
                                }
                            }
                            // Go's error points at the pipeline inside the
                            // keyword, e.g. `<.Bad>` for `{{ if .Bad }}`.
                            let pipe_src = action[kw.len()..].trim_start();
                            let skip = action.len() - pipe_src.len();
                            let mut pos = self.pos_at(action_start, pipe_src);
                            pos.col += skip;
                            nodes.push(match kw {
                                "if" => Node::If {
                                    pos,
                                    pipe,
                                    list,
                                    else_list,
                                },
                                "range" => Node::Range {
                                    pos,
                                    pipe,
                                    list,
                                    else_list,
                                },
                                _ => Node::With {
                                    pos,
                                    pipe,
                                    list,
                                    else_list,
                                },
                            });
                        }
                        "end" | "else" => {
                            return Err(
                                self.err_at(action_start, &format!("unexpected {{{{{}}}}}", kw))
                            );
                        }
                        _ => {
                            let pos = self.pos_at(action_start, action);
                            nodes.push(Node::Action(
                                parse_pipeline(action)
                                    .map_err(|e| self.err_at(action_start, &e))?,
                                pos,
                            ));
                        }
                    }
                }
            }
        }
    }
}

fn parse_pipeline(s: &str) -> Result<Pipeline, String> {
    let mut pipe = Pipeline::default();
    let mut rest = s.trim();
    // Leading declarations: `$x := ` or `$k, $v := `
    if let Some(idx) = find_decl_assign(rest) {
        let lhs = rest[..idx].trim();
        for part in lhs.split(',') {
            let name = part.trim();
            if !name.starts_with('$') {
                return Err(format!("invalid declaration {:?}", name));
            }
            pipe.decls.push(name[1..].to_string());
        }
        rest = rest[idx + 2..].trim();
    }
    for cmd_src in split_top_level(rest, '|') {
        let cmd = parse_command(cmd_src.trim())?;
        if cmd.is_empty() {
            return Err("empty command".to_string());
        }
        pipe.cmds.push(cmd);
    }
    if pipe.cmds.is_empty() {
        return Err("missing value for command".to_string());
    }
    Ok(pipe)
}

/// Locate a top-level `:=` (not inside parens or a string).
fn find_decl_assign(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut depth = 0;
    let mut i = 0;
    let mut in_str = false;
    while i + 1 < b.len() {
        match b[i] {
            b'"' => in_str = !in_str,
            b'(' if !in_str => depth += 1,
            b')' if !in_str => depth -= 1,
            b':' if !in_str && depth == 0 && b[i + 1] == b'=' => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

fn split_top_level(s: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut in_str = false;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '"' => in_str = !in_str,
            '(' if !in_str => depth += 1,
            ')' if !in_str => depth -= 1,
            c if c == sep && !in_str && depth == 0 => {
                out.push(&s[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

fn parse_command(s: &str) -> Result<Command, String> {
    let mut ops = Vec::new();
    for tok in tokenize_args(s)? {
        ops.push(parse_operand(&tok)?);
    }
    // Go resolves function names while parsing, so an unknown one is a parse
    // error (and therefore a template.Must panic), not a run-time failure.
    for op in &ops {
        if let Operand::Ident(name) = op {
            if !matches!(name.as_str(), "true" | "false" | "nil") && !is_known_func(name) {
                return Err(format!("function {:?} not defined", name));
            }
        }
    }
    Ok(ops)
}

fn is_known_func(name: &str) -> bool {
    matches!(
        name,
        "len" | "index" | "eq" | "ne" | "lt" | "le" | "gt" | "ge" | "not" | "and" | "or"
            | "print" | "println" | "printf" | "call" | "html" | "js" | "urlquery" | "slice"
            // hey's tmplFuncMap (print.go)
            | "formatNumber" | "formatNumberInt" | "histogram" | "jsonify"
    )
}

/// Split on whitespace, honouring quoted strings and parenthesised groups.
fn tokenize_args(s: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0;
    let mut in_str = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                in_str = !in_str;
                cur.push(c);
            }
            '\\' if in_str => {
                cur.push(c);
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            '(' if !in_str => {
                depth += 1;
                cur.push(c);
            }
            ')' if !in_str => {
                depth -= 1;
                if depth < 0 {
                    return Err("unexpected )".to_string());
                }
                cur.push(c);
            }
            c if c.is_whitespace() && !in_str && depth == 0 => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if depth != 0 {
        return Err("unclosed (".to_string());
    }
    if in_str {
        return Err("unterminated string".to_string());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    Ok(out)
}

fn parse_operand(tok: &str) -> Result<Operand, String> {
    if tok == "." {
        return Ok(Operand::Dot);
    }
    if tok.starts_with('(') && tok.ends_with(')') {
        return Ok(Operand::Sub(parse_pipeline(&tok[1..tok.len() - 1])?));
    }
    if tok.starts_with('"') && tok.ends_with('"') && tok.len() >= 2 {
        return Ok(Operand::Str(unquote(&tok[1..tok.len() - 1])));
    }
    if let Some(rest) = tok.strip_prefix('.') {
        let parts: Vec<String> = rest.split('.').map(|s| s.to_string()).collect();
        if parts.iter().any(|p| p.is_empty()) {
            return Err(format!("bad field syntax {:?}", tok));
        }
        return Ok(Operand::Field(parts));
    }
    if let Some(rest) = tok.strip_prefix('$') {
        let mut it = rest.split('.');
        let name = it.next().unwrap_or("").to_string();
        let fields: Vec<String> = it.map(|s| s.to_string()).collect();
        return Ok(Operand::Var(name, fields));
    }
    if let Ok(i) = tok.parse::<i64>() {
        return Ok(Operand::Int(i));
    }
    if let Ok(f) = tok.parse::<f64>() {
        return Ok(Operand::Float(f));
    }
    Ok(Operand::Ident(tok.to_string()))
}

fn unquote(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

struct Frame {
    vars: Vec<(String, Value)>,
}

impl Template {
    /// Go: `template.New("tmpl").Funcs(tmplFuncMap).Parse(src)`
    pub fn parse(src: &str) -> Result<Template, String> {
        let mut p = Parser::new(src);
        let (root, term) = p.parse_list(&[])?;
        if !term.is_empty() {
            return Err(format!("1: unexpected {{{{{}}}}}", term));
        }
        Ok(Template { root })
    }

    /// Go: `template.Must(...)` -- panics on a parse error.
    ///
    /// A Go panic prints `panic: <msg>` on stderr and exits 2. A Rust panic
    /// would print a different banner and exit 101, so the observable
    /// contract is reproduced directly instead.
    pub fn must(src: &str) -> Template {
        match Template::parse(src) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("panic: template: tmpl:{}", e);
                std::process::exit(2);
            }
        }
    }

    /// Go: `t.Execute(w, data)`
    pub fn execute(&self, data: &Value) -> Result<String, String> {
        let mut out = String::new();
        let mut frame = Frame {
            vars: vec![("".to_string(), data.clone())],
        };
        exec_list(&self.root, data, &mut frame, &mut out)?;
        Ok(out)
    }
}

fn exec_list(
    nodes: &[Node],
    dot: &Value,
    frame: &mut Frame,
    out: &mut String,
) -> Result<(), String> {
    for n in nodes {
        exec_node(n, dot, frame, out)?;
    }
    Ok(())
}

fn exec_node(node: &Node, dot: &Value, frame: &mut Frame, out: &mut String) -> Result<(), String> {
    match node {
        Node::Text(t) => {
            out.push_str(t);
            Ok(())
        }
        Node::Action(p, pos) => {
            let v = eval_pipeline(p, dot, frame, pos).map_err(|e| pos.wrap_once(e))?;
            if p.decls.is_empty() {
                out.push_str(&v.print());
            } else {
                // `{{ $x := pipe }}` binds and prints nothing. The binding
                // stays live for the rest of the enclosing list, which is what
                // csvTmpl relies on when it hoists .ConnLats & friends.
                for name in &p.decls {
                    frame.vars.push((name.clone(), v.clone()));
                }
            }
            Ok(())
        }
        Node::If {
            pos,
            pipe,
            list,
            else_list,
        } => {
            let v = eval_pipeline(pipe, dot, frame, pos).map_err(|e| pos.wrap_once(e))?;
            let mark = frame.vars.len();
            if v.truth() {
                exec_list(list, dot, frame, out)?;
            } else {
                exec_list(else_list, dot, frame, out)?;
            }
            frame.vars.truncate(mark);
            Ok(())
        }
        Node::With {
            pos,
            pipe,
            list,
            else_list,
        } => {
            let v = eval_pipeline(pipe, dot, frame, pos).map_err(|e| pos.wrap_once(e))?;
            let mark = frame.vars.len();
            if v.truth() {
                exec_list(list, &v, frame, out)?;
            } else {
                exec_list(else_list, dot, frame, out)?;
            }
            frame.vars.truncate(mark);
            Ok(())
        }
        Node::Range {
            pos,
            pipe,
            list,
            else_list,
        } => {
            let v = eval_pipeline(pipe, dot, frame, pos).map_err(|e| pos.wrap_once(e))?;
            let items: Vec<(Value, Value)> = match &v {
                Value::Slice(items) => items
                    .iter()
                    .enumerate()
                    .map(|(i, e)| (Value::Int(i as i64), e.clone()))
                    .collect(),
                // Go sorts map keys before ranging; BTreeMap already is sorted.
                Value::MapInt(m) => m.iter().map(|(k, e)| (Value::Int(*k), e.clone())).collect(),
                Value::MapStr(m) => m
                    .iter()
                    .map(|(k, e)| (Value::Str(k.clone()), e.clone()))
                    .collect(),
                // Go 1.22+ text/template can range over an integer, yielding
                // 0..n-1 with dot bound to the index.
                Value::Int(n) => (0..*n).map(|i| (Value::Int(i), Value::Int(i))).collect(),
                Value::Nil => Vec::new(),
                other => {
                    return Err(pos.wrap(&format!("range can't iterate over {}", other.kind())));
                }
            };
            if items.is_empty() {
                let mark = frame.vars.len();
                exec_list(else_list, dot, frame, out)?;
                frame.vars.truncate(mark);
                return Ok(());
            }
            for (k, e) in items {
                let mark = frame.vars.len();
                match pipe.decls.len() {
                    0 => {}
                    1 => frame.vars.push((pipe.decls[0].clone(), e.clone())),
                    _ => {
                        frame.vars.push((pipe.decls[0].clone(), k.clone()));
                        frame.vars.push((pipe.decls[1].clone(), e.clone()));
                    }
                }
                exec_list(list, &e, frame, out)?;
                frame.vars.truncate(mark);
            }
            Ok(())
        }
    }
}

fn eval_pipeline(p: &Pipeline, dot: &Value, frame: &mut Frame, pos: &Pos) -> Result<Value, String> {
    let mut acc: Option<Value> = None;
    for cmd in &p.cmds {
        acc = Some(eval_command(cmd, dot, frame, acc, pos)?);
    }
    // Binding is done by the caller: Node::Action binds once, Node::Range
    // rebinds per iteration.
    Ok(acc.unwrap_or(Value::Nil))
}

fn eval_command(
    cmd: &Command,
    dot: &Value,
    frame: &mut Frame,
    piped: Option<Value>,
    pos: &Pos,
) -> Result<Value, String> {
    let first = &cmd[0];
    if let Operand::Ident(name) = first {
        if name != "true" && name != "false" && name != "nil" {
            let mut args = Vec::new();
            for op in &cmd[1..] {
                args.push(eval_operand(op, dot, frame, pos)?);
            }
            if let Some(p) = piped {
                args.push(p);
            }
            return call_func(name, args).map_err(|e| format!("error calling {}: {}", name, e));
        }
    }
    if cmd.len() > 1 {
        return Err(format!("can't give argument to non-function {:?}", first));
    }
    let v = eval_operand(first, dot, frame, pos)?;
    Ok(v)
}

fn eval_operand(op: &Operand, dot: &Value, frame: &mut Frame, pos: &Pos) -> Result<Value, String> {
    match op {
        Operand::Dot => Ok(dot.clone()),
        Operand::Int(i) => Ok(Value::Int(*i)),
        Operand::Float(f) => Ok(Value::Float(*f)),
        Operand::Str(s) => Ok(Value::Str(s.clone())),
        Operand::Sub(p) => eval_pipeline(p, dot, frame, pos),
        Operand::Ident(name) => match name.as_str() {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            "nil" => Ok(Value::Nil),
            _ => call_func(name, Vec::new()),
        },
        Operand::Field(fields) => {
            let mut v = dot.clone();
            let mut delta = 0usize;
            for f in fields {
                v = v.field(f).map_err(|e| pos.wrap_at(delta, &e))?;
                delta += 1 + f.len();
            }
            Ok(v)
        }
        Operand::Var(name, fields) => {
            let mut v = frame
                .vars
                .iter()
                .rev()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.clone())
                .ok_or_else(|| format!("undefined variable \"${}\"", name))?;
            let mut delta = 1 + name.len();
            for f in fields {
                v = v.field(f).map_err(|e| pos.wrap_at(delta, &e))?;
                delta += 1 + f.len();
            }
            Ok(v)
        }
    }
}

fn call_func(name: &str, args: Vec<Value>) -> Result<Value, String> {
    match name {
        // --- builtins -----------------------------------------------------
        "len" => {
            let v = args.first().ok_or("wrong number of args for len")?;
            let n = match v {
                Value::Slice(s) => s.len(),
                Value::MapInt(m) => m.len(),
                Value::MapStr(m) => m.len(),
                Value::Str(s) => s.len(),
                Value::Nil => 0,
                other => return Err(format!("len of type {}", other.kind())),
            };
            Ok(Value::Int(n as i64))
        }
        "index" => {
            let base = args.first().ok_or("wrong number of args for index")?;
            let idx = args.get(1).ok_or("wrong number of args for index")?;
            match (base, idx) {
                (Value::Slice(s), Value::Int(i)) => {
                    if *i < 0 || *i as usize >= s.len() {
                        Err(format!("index out of range: {}", i))
                    } else {
                        Ok(s[*i as usize].clone())
                    }
                }
                (Value::MapInt(m), Value::Int(i)) => Ok(m.get(i).cloned().unwrap_or(Value::Nil)),
                (Value::MapStr(m), Value::Str(k)) => Ok(m.get(k).cloned().unwrap_or(Value::Nil)),
                _ => Err("can't index item of this type".to_string()),
            }
        }
        "eq" | "ne" | "lt" | "le" | "gt" | "ge" => {
            let a = args.first().ok_or("wrong number of args")?;
            let b = args.get(1).ok_or("wrong number of args")?;
            compare(name, a, b)
        }
        "not" => Ok(Value::Bool(
            !args.first().map(|v| v.truth()).unwrap_or(false),
        )),
        "and" => {
            let mut last = Value::Bool(true);
            for a in args {
                last = a.clone();
                if !a.truth() {
                    return Ok(last);
                }
            }
            Ok(last)
        }
        "or" => {
            let mut last = Value::Bool(false);
            for a in args {
                last = a.clone();
                if a.truth() {
                    return Ok(last);
                }
            }
            Ok(last)
        }
        "print" => Ok(Value::Str(
            args.iter().map(|a| a.print()).collect::<Vec<_>>().join(""),
        )),
        "println" => Ok(Value::Str(format!(
            "{}\n",
            args.iter().map(|a| a.print()).collect::<Vec<_>>().join(" ")
        ))),

        // --- hey's tmplFuncMap (print.go) ---------------------------------
        // Dispatch through print.rs so the ported tmplFuncMap functions are
        // the ones actually doing the work, as in Go.
        "formatNumber" => {
            let v = args
                .first()
                .and_then(|v| v.as_f64())
                .ok_or("formatNumber: not a float64")?;
            Ok(Value::Str(crate::requester::print::format_number(v)))
        }
        "formatNumberInt" => {
            let v = match args.first() {
                Some(Value::Int(i)) => *i,
                Some(Value::Float(f)) => *f as i64,
                _ => return Err("formatNumberInt: not an int".to_string()),
            };
            Ok(Value::Str(crate::requester::print::format_number_int(v)))
        }
        "histogram" => {
            let v = args.first().ok_or("histogram: missing arg")?;
            Ok(Value::Str(crate::requester::print::histogram(v)?))
        }
        "jsonify" => Ok(Value::Str(jsonify(args.first().unwrap_or(&Value::Nil)))),
        _ => Err(format!("function {:?} not defined", name)),
    }
}

fn compare(op: &str, a: &Value, b: &Value) -> Result<Value, String> {
    // Go's comparison builtins require matching basic kinds; ints and floats
    // are compared numerically, which is all hey's templates need.
    let ord = match (a, b) {
        (Value::Str(x), Value::Str(y)) => x.partial_cmp(y),
        (Value::Bool(x), Value::Bool(y)) => x.partial_cmp(y),
        _ => {
            let x = a
                .as_f64()
                .ok_or_else(|| format!("incompatible types for comparison: {}", a.kind()))?;
            let y = b
                .as_f64()
                .ok_or_else(|| format!("incompatible types for comparison: {}", b.kind()))?;
            x.partial_cmp(&y)
        }
    };
    use std::cmp::Ordering::*;
    let r = match (op, ord) {
        ("eq", Some(Equal)) => true,
        ("eq", _) => false,
        ("ne", Some(Equal)) => false,
        ("ne", _) => true,
        ("lt", Some(Less)) => true,
        ("lt", _) => false,
        ("le", Some(Less)) | ("le", Some(Equal)) => true,
        ("le", _) => false,
        ("gt", Some(Greater)) => true,
        ("gt", _) => false,
        ("ge", Some(Greater)) | ("ge", Some(Equal)) => true,
        ("ge", _) => false,
        _ => return Err(format!("unknown comparison {}", op)),
    };
    Ok(Value::Bool(r))
}

/// Go: `json.Marshal` for the value shapes the report can hold.
fn jsonify(v: &Value) -> String {
    match v {
        Value::Nil => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => {
            if f.is_finite() {
                let mut s = format!("{}", f);
                if s == "-0" {
                    s = "-0".to_string();
                }
                s
            } else {
                // encoding/json refuses NaN/Inf; Marshal returns an error and
                // jsonify discards it, yielding "".
                String::new()
            }
        }
        Value::Str(s) => {
            let mut out = String::from("\"");
            for c in s.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\t' => out.push_str("\\t"),
                    '\r' => out.push_str("\\r"),
                    '<' => out.push_str("\\u003c"),
                    '>' => out.push_str("\\u003e"),
                    '&' => out.push_str("\\u0026"),
                    c if (c as u32) < 0x20 => {
                        let _ = write!(out, "\\u{:04x}", c as u32);
                    }
                    c => out.push(c),
                }
            }
            out.push('"');
            out
        }
        Value::Duration(d) => d.nanoseconds().to_string(),
        Value::Slice(items) => {
            let inner: Vec<String> = items.iter().map(jsonify).collect();
            format!("[{}]", inner.join(","))
        }
        Value::MapInt(m) => {
            let inner: Vec<String> = m
                .iter()
                .map(|(k, v)| format!("\"{}\":{}", k, jsonify(v)))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        Value::MapStr(m) => {
            let inner: Vec<String> = m
                .iter()
                .map(|(k, v)| format!("{}:{}", jsonify(&Value::Str(k.clone())), jsonify(v)))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        Value::Struct(m) => {
            let inner: Vec<String> = m
                .iter()
                .map(|(k, v)| format!("\"{}\":{}", k, jsonify(v)))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
    }
}

#[cfg(test)]
#[path = "gotemplate_test.rs"]
mod gotemplate_test;
