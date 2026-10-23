//! Syntax-only reader for the console-exercise subset documented in README.md.
use crate::{Block, Case, Document, Result, kind, text};
use regex::Regex;
use std::{fs, path::Path, sync::LazyLock};

static COMPACT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"@"(?:""|[^"])*"|"(?:\\.|[^"\\])*"|\s+"#).unwrap());
static CONSOLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:global::)?(?:System\.)?Console\.(ReadLine|Read|ReadKey|Write|WriteLine)$")
        .unwrap()
});
static OUT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^out\s+(?:(\S+)\s+)?(\w+)$").unwrap());
static CONVERT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:System\.)?Convert\.To\w+$").unwrap());

fn compact(s: &str) -> String {
    COMPACT
        .replace_all(s, |m: &regex::Captures<'_>| {
            if m[0].chars().all(char::is_whitespace) {
                " ".to_owned()
            } else {
                m[0].to_owned()
            }
        })
        .trim()
        .to_owned()
}

fn literal(s: &str) -> String {
    let Some(quote) = s.find('"') else {
        return s.to_owned();
    };
    let verbatim = s.starts_with("@\"") || s.starts_with("$@\"") || s.starts_with("@$\"");
    if !verbatim && !s.starts_with('"') && !s.starts_with("$\"") {
        return s.to_owned();
    }
    // Keep Python's label conversion for string-led expressions as well as literals.
    let end = s.char_indices().next_back().unwrap().0;
    let inner = s.get(quote + 1..end).unwrap_or("");
    if verbatim {
        return inner.replace("\"\"", "\"");
    }
    let mut result = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            result.push(c);
            continue;
        }
        let Some(escaped) = chars.next() else {
            result.push('\\');
            break;
        };
        match escaped {
            'n' => result.push('\n'),
            'r' => (),
            't' => result.push_str("    "),
            '0' => result.push('␀'),
            '"' | '\'' | '\\' => result.push(escaped),
            'u' | 'U' => {
                let digits: String = chars
                    .by_ref()
                    .take(if escaped == 'u' { 4 } else { 8 })
                    .collect();
                if let Some(c) = u32::from_str_radix(&digits, 16)
                    .ok()
                    .and_then(char::from_u32)
                {
                    result.push(c);
                } else {
                    result.push('\\');
                    result.push(escaped);
                    result.push_str(&digits);
                }
            }
            _ => {
                result.push('\\');
                result.push(escaped);
            }
        }
    }
    result
}

fn op(k: &str, s: impl Into<String>, line: usize) -> Block {
    let mut item = Block::new(k, s);
    item.line = Some(line);
    item
}

pub(crate) fn group(items: Vec<Block>) -> Vec<Block> {
    let mut result: Vec<Block> = Vec::new();
    for mut item in items {
        if let Some(previous) = result.last_mut()
            && matches!(kind(&item), "process" | "input" | "output")
            && kind(previous) == kind(&item)
            && text(previous).chars().count() + text(&item).chars().count() < 550
            && text(previous).matches('\n').count() + text(&item).matches('\n').count() < 7
        {
            let prefix = match kind(&item) {
                "input" => "Ievade:\n",
                "output" => "Izvade:\n",
                _ => "",
            };
            previous.text = format!(
                "{}\n{}",
                text(previous),
                text(&item).strip_prefix(prefix).unwrap_or(text(&item))
            );
            if let Some(prompt) = item.prompt.take() {
                previous.prompt = Some(
                    [previous.prompt.as_deref().unwrap_or(""), prompt.as_str()]
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
            }
            continue;
        }
        result.push(item);
    }
    result
}

#[derive(Clone, Debug)]
struct Token {
    start: usize,
    end: usize,
    line: usize,
    mate: Option<usize>,
}

// ponytail: this lexer covers console exercises, not C# type checking or preprocessing;
// use Tree-sitter if whole-language syntax validation becomes a requirement.
fn lex(source: &str) -> Result<Vec<Token>> {
    let bytes = source.as_bytes();
    let (mut i, mut line) = (0, 1);
    let mut tokens: Vec<Token> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    while i < bytes.len() {
        let c = source[i..].chars().next().unwrap();
        if c.is_whitespace() {
            if c == '\n' {
                line += 1;
            }
            i += c.len_utf8();
            continue;
        }
        if source[i..].starts_with("//") {
            i = source[i..].find('\n').map_or(bytes.len(), |n| i + n);
            continue;
        }
        if source[i..].starts_with("/*") {
            let end = source[i + 2..]
                .find("*/")
                .ok_or_else(|| format!("C# line {line}: invalid or unsupported C# syntax"))?
                + i
                + 4;
            line += source[i..end].matches('\n').count();
            i = end;
            continue;
        }
        let start = i;
        let start_line = line;
        let quote = if matches!(c, '"' | '\'') {
            Some(i)
        } else if matches!(c, '$' | '@') {
            let mut q = i;
            while q < bytes.len() && matches!(bytes[q], b'$' | b'@') {
                q += 1;
            }
            (q < bytes.len() && bytes[q] == b'"').then_some(q)
        } else {
            None
        };
        if let Some(q) = quote {
            i = string_end(
                source,
                q,
                source[start..q].contains('@'),
                source[start..q].contains('$'),
            )
            .ok_or_else(|| format!("C# line {line}: invalid or unsupported C# syntax"))?;
        } else if c.is_alphabetic() || c == '_' || c == '@' || c.is_ascii_digit() {
            i += c.len_utf8();
            while i < bytes.len() {
                let n = source[i..].chars().next().unwrap();
                if n.is_alphanumeric() || n == '_' {
                    i += n.len_utf8();
                } else {
                    break;
                }
            }
        } else {
            let operator = [
                "??=", ">>=", "<<=", "=>", "==", "!=", "<=", ">=", "&&", "||", "??", "++", "--",
                "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "::", "?.", "<<", ">>",
            ]
            .into_iter()
            .find(|op| source[i..].starts_with(op));
            i += operator.map_or(c.len_utf8(), str::len);
        }
        line += source[start..i].matches('\n').count();
        let index = tokens.len();
        tokens.push(Token {
            start,
            end: i,
            line: start_line,
            mate: None,
        });
        let s = &source[start..i];
        if matches!(s, "(" | "[" | "{") {
            stack.push(index);
        }
        if matches!(s, ")" | "]" | "}") {
            let open = stack
                .pop()
                .ok_or_else(|| format!("C# line {line}: invalid or unsupported C# syntax"))?;
            let expected = match s {
                ")" => "(",
                "]" => "[",
                _ => "{",
            };
            if &source[tokens[open].start..tokens[open].end] != expected {
                return Err(format!("C# line {line}: invalid or unsupported C# syntax"));
            }
            tokens[open].mate = Some(index);
            tokens[index].mate = Some(open);
        }
    }
    if let Some(index) = stack.pop() {
        return Err(format!(
            "C# line {}: invalid or unsupported C# syntax",
            tokens[index].line
        ));
    }
    Ok(tokens)
}

fn string_end(s: &str, quote: usize, verbatim: bool, interpolated: bool) -> Option<usize> {
    let b = s.as_bytes();
    let delimiter = b[quote];
    let count = b[quote..].iter().take_while(|c| **c == delimiter).count();
    if delimiter == b'"' && count >= 3 {
        let marker = "\"".repeat(count);
        return s[quote + count..]
            .find(&marker)
            .map(|n| quote + count + n + count);
    }
    let mut i = quote + 1;
    let mut braces = 0;
    while i < b.len() {
        if !verbatim && b[i] == b'\\' {
            i += 2;
            continue;
        }
        if interpolated && b[i] == b'{' {
            if braces == 0 && b.get(i + 1) == Some(&b'{') {
                i += 2;
                continue;
            }
            braces += 1;
        } else if interpolated && b[i] == b'}' && braces > 0 {
            braces -= 1;
        }
        if b[i] == delimiter {
            if braces > 0 {
                i = string_end(s, i, false, false)?;
                continue;
            }
            if verbatim && b.get(i + 1) == Some(&delimiter) {
                i += 2;
                continue;
            }
            return Some(i + 1);
        }
        i += 1;
    }
    None
}

#[derive(Clone)]
struct Call {
    start: usize,
    open: usize,
    end: usize,
    function: String,
}

struct Reader<'a> {
    source: &'a str,
    tokens: Vec<Token>,
    position: usize,
}

impl Reader<'_> {
    fn validate_syntax(&self) -> Result<()> {
        for i in 0..self.tokens.len() {
            let next = self.token(i + 1);
            if (matches!(self.token(i), "=" | "+=" | "-=" | "*=" | "/=" | "%=")
                && matches!(next, ";" | "," | ")" | "}" | ""))
                || (self.token(i) == "case" && next == ":")
                || (matches!(self.token(i), "if" | "while" | "switch")
                    && next == "("
                    && self.tokens[i + 1].mate == Some(i + 2))
            {
                return self.fail(i, "invalid or unsupported C# syntax");
            }
            if (matches!(
                self.token(i),
                "+" | "-" | "*" | "/" | "%" | "&&" | "||" | "==" | "!=" | "<" | "<=" | ">="
            ) && matches!(next, ";" | "}" | ""))
                || (matches!(
                    self.token(i),
                    "int"
                        | "long"
                        | "short"
                        | "byte"
                        | "float"
                        | "double"
                        | "decimal"
                        | "bool"
                        | "char"
                        | "string"
                        | "void"
                ) && next == "=")
            {
                return self.fail(i, "invalid or unsupported C# syntax");
            }
            if self.token(i) == "("
                && i > 0
                && identifier(self.token(i - 1))
                && !matches!(
                    self.token(i - 1),
                    "for" | "foreach" | "if" | "while" | "switch" | "catch" | "using" | "lock"
                )
            {
                let end = self.tokens[i].mate.unwrap();
                if end > i + 1 && self.split(i + 1, end, ",").iter().any(|(a, b)| a == b) {
                    return self.fail(i, "invalid or unsupported C# syntax");
                }
            }
        }
        Ok(())
    }
    fn token(&self, i: usize) -> &str {
        self.tokens
            .get(i)
            .map(|t| &self.source[t.start..t.end])
            .unwrap_or("")
    }
    fn raw(&self, a: usize, b: usize) -> String {
        if a >= b {
            String::new()
        } else {
            compact(&self.source[self.tokens[a].start..self.tokens[b - 1].end])
        }
    }
    fn line(&self, i: usize) -> usize {
        self.tokens.get(i).map_or(1, |t| t.line)
    }
    fn fail<T>(&self, i: usize, message: &str) -> Result<T> {
        Err(format!("C# line {}: {message}", self.line(i)))
    }
    fn expect(&mut self, s: &str) -> Result<()> {
        if self.token(self.position) != s {
            return self.fail(self.position, "invalid or unsupported C# syntax");
        }
        self.position += 1;
        Ok(())
    }
    fn split(&self, a: usize, b: usize, separator: &str) -> Vec<(usize, usize)> {
        let mut pieces = Vec::new();
        let mut start = a;
        let mut i = a;
        while i < b {
            if self.token(i) == separator {
                pieces.push((start, i));
                start = i + 1;
            }
            if matches!(self.token(i), "(" | "[" | "{") {
                i = self.tokens[i].mate.unwrap() + 1;
            } else if self.token(i) == "<"
                && let Some(end) = self.generic_end(i, b)
            {
                i = end;
            } else {
                i += 1;
            }
        }
        pieces.push((start, b));
        pieces
    }
    fn generic_end(&self, start: usize, limit: usize) -> Option<usize> {
        let mut depth = 0;
        for i in start..limit {
            match self.token(i) {
                "<" => depth += 1,
                ">" => depth -= 1,
                ">>" => depth -= 2,
                "," | "." | "::" | "?" | "[" | "]" | "*" => (),
                s if identifier(s) => (),
                _ => return None,
            }
            if depth == 0 {
                return Some(i + 1);
            }
            if depth < 0 {
                return None;
            }
        }
        None
    }
    fn type_end(&self, start: usize, limit: usize) -> Option<usize> {
        if start >= limit {
            return None;
        }
        let mut i = start;
        if self.token(i) == "(" {
            let end = self.tokens[i].mate?;
            if end >= limit {
                return None;
            }
            let parts = self.split(i + 1, end, ",");
            if parts.len() < 2 {
                return None;
            }
            for (a, b) in parts {
                let ty_end = self.type_end(a, b)?;
                if ty_end != b && !(ty_end + 1 == b && identifier(self.token(ty_end))) {
                    return None;
                }
            }
            i = end + 1;
        } else {
            if !identifier(self.token(i)) {
                return None;
            }
            i += 1;
            loop {
                if self.token(i) == "<" {
                    i = self.generic_end(i, limit)?;
                }
                if matches!(self.token(i), "." | "::") && identifier(self.token(i + 1)) {
                    i += 2;
                } else {
                    break;
                }
            }
        }
        while i < limit {
            match self.token(i) {
                "?" | "*" => i += 1,
                "[" => {
                    let end = self.tokens[i].mate?;
                    if (i + 1..end).any(|j| self.token(j) != ",") {
                        return None;
                    }
                    i = end + 1;
                }
                _ => break,
            }
        }
        Some(i)
    }
    fn expressions(&self, a: usize, b: usize, line: usize) -> Vec<Block> {
        if a == b {
            return Vec::new();
        }
        let mut result = Vec::new();
        for (index, (a, b)) in self.split(a, b, ",").into_iter().enumerate() {
            // Tree-sitter exposes comma tokens in for initializers and updates.
            if index > 0 {
                result.push(op("process", ",", line));
            }
            result.push(op("process", self.raw(a, b), line));
        }
        result
    }
    fn calls(&self, a: usize, b: usize) -> Vec<Call> {
        let mut result = Vec::new();
        for i in a..b {
            if self.token(i) != "(" || i == a {
                continue;
            }
            let end = self.tokens[i].mate.unwrap() + 1;
            if end > b {
                continue;
            }
            let mut start = i - 1;
            if !identifier(self.token(start)) {
                continue;
            }
            while start >= a + 2
                && matches!(self.token(start - 1), "." | "::")
                && identifier(self.token(start - 2))
            {
                start -= 2;
            }
            result.push(Call {
                start,
                open: i,
                end,
                function: self.raw(start, i),
            });
        }
        result
    }
    fn reads(&self, a: usize, b: usize) -> Vec<Call> {
        self.calls(a, b)
            .into_iter()
            .filter(|c| {
                CONSOLE
                    .captures(&c.function)
                    .is_some_and(|m| matches!(&m[1], "ReadLine" | "Read" | "ReadKey"))
            })
            .collect()
    }
    fn input_value(
        &self,
        a: usize,
        mut b: usize,
        name: &str,
        mut ty: String,
        line: usize,
    ) -> Result<Block> {
        let reads = self.reads(a, b);
        if reads.len() != 1 {
            return self.fail(a, "assign each console input to a separate variable");
        }
        if self.token(b.saturating_sub(1)) == "!" {
            b -= 1;
        }
        let function = self
            .calls(a, b)
            .into_iter()
            .find(|c| c.start == a && c.end == b)
            .map(|c| c.function)
            .unwrap_or_default();
        let direct = reads[0].start == a && reads[0].end == b;
        if !direct && !function.ends_with(".Parse") && !CONVERT.is_match(&function) {
            return self.fail(
                a,
                "compound input expression: assign the input before the calculation",
            );
        }
        if ty == "var" {
            if let Some(prefix) = function.strip_suffix(".Parse") {
                ty = prefix.to_owned();
            } else if direct {
                ty = match function.rsplit('.').next().unwrap_or("") {
                    "Read" => "int",
                    "ReadKey" => "ConsoleKeyInfo",
                    _ => "string",
                }
                .into();
            }
        }
        Ok(op(
            "input",
            format!(
                "Ievade:\n{name}{}",
                if ty.is_empty() {
                    String::new()
                } else {
                    format!(" ({ty})")
                }
            ),
            line,
        ))
    }
    fn condition(&self, a: usize, b: usize) -> Result<(Vec<Block>, String)> {
        let mut inputs = Vec::new();
        let mut replacements: Vec<(usize, usize, String)> = Vec::new();
        let calls = self.calls(a, b);
        for read in self.reads(a, b) {
            for i in a..read.start {
                if matches!(self.token(i), "&&" | "||" | "??") {
                    let scope = (a..i).rev().find(|&j| {
                        matches!(self.token(j), "(" | "[" | "{") && self.tokens[j].mate.unwrap() > i
                    });
                    let separate_argument = (i + 1..read.start).any(|j| {
                        matches!(self.token(j), "," | "?" | ":")
                            && (a..j).rev().find(|&k| {
                                matches!(self.token(k), "(" | "[" | "{")
                                    && self.tokens[k].mate.unwrap() > j
                            }) == scope
                    });
                    if !separate_argument
                        && scope.is_none_or(|j| self.tokens[j].mate.unwrap() > read.end - 1)
                    {
                        return self.fail(
                            read.start,
                            "conditional console input: assign the input before this condition",
                        );
                    }
                }
            }
            let parse = calls
                .iter()
                .filter(|c| {
                    c.start <= read.start && c.end >= read.end && c.function.contains(".TryParse")
                })
                .min_by_key(|c| c.end - c.start);
            if let Some(parse) = parse {
                let arguments = self.split(parse.open + 1, parse.end - 1, ",");
                let (out_a, out_b) = *arguments.last().unwrap();
                let out = self.raw(out_a, out_b);
                let m = OUT.captures(&out).ok_or_else(|| {
                    format!(
                        "C# line {}: unsupported TryParse output; use out type name or out name",
                        self.line(parse.start)
                    )
                })?;
                let name = &m[2];
                let ty = m
                    .get(1)
                    .map(|m| m.as_str())
                    .filter(|t| *t != "var")
                    .unwrap_or_else(|| parse.function.rsplit_once('.').unwrap().0);
                inputs.push(op(
                    "input",
                    format!("Ievade:\n{name} ({ty})"),
                    self.line(read.start),
                ));
                let negated = parse.start > a && self.token(parse.start - 1) == "!";
                let start = if negated {
                    parse.start - 1
                } else {
                    parse.start
                };
                replacements.push((
                    self.tokens[start].start,
                    self.tokens[parse.end - 1].end,
                    format!("{name} {} {ty}", if negated { "nav" } else { "ir" }),
                ));
            } else {
                inputs.push(op(
                    "input",
                    "Ievade:\nteksts (string)",
                    self.line(read.start),
                ));
                replacements.push((
                    self.tokens[read.start].start,
                    self.tokens[read.end - 1].end,
                    "ievadītais teksts".into(),
                ));
            }
        }
        for i in a..b {
            if matches!(self.token(i), "&&" | "||")
                && !replacements.iter().any(|(start, end, _)| {
                    *start <= self.tokens[i].start && self.tokens[i].start < *end
                })
            {
                replacements.push((
                    self.tokens[i].start,
                    self.tokens[i].end,
                    if self.token(i) == "&&" { "un" } else { "vai" }.into(),
                ));
            }
        }
        if a == b {
            return Ok((inputs, "true?".into()));
        }
        let offset = self.tokens[a].start;
        let mut value = self.source[offset..self.tokens[b - 1].end].to_owned();
        replacements.sort();
        replacements.dedup();
        for (start, end, replacement) in replacements.into_iter().rev() {
            value.replace_range(start - offset..end - offset, &replacement);
        }
        Ok((group(inputs), format!("{}?", compact(&value))))
    }
    fn declaration(&self, a: usize, b: usize) -> Result<Option<Vec<Block>>> {
        let mut type_start = a;
        while matches!(
            self.token(type_start),
            "const" | "ref" | "readonly" | "scoped" | "using"
        ) {
            type_start += 1;
        }
        if matches!(self.token(type_start), "new" | "await" | "return" | "throw") {
            return Ok(None);
        }
        let Some(name_index) = self.type_end(type_start, b) else {
            return Ok(None);
        };
        if self.token(type_start) == "var" && self.token(name_index) == "(" {
            return self.fail(
                name_index,
                "unsupported deconstruction; assign each variable separately",
            );
        }
        if !identifier(self.token(name_index))
            || !matches!(self.token(name_index + 1), "=" | ",") && name_index + 1 != b
        {
            return Ok(None);
        }
        let ty = self.raw(type_start, name_index);
        let pieces = self.split(name_index, b, ",");
        let mut result = Vec::new();
        for (index, (start, end)) in pieces.into_iter().enumerate() {
            let start = if index == 0 { name_index } else { start };
            if start >= end || !identifier(self.token(start)) {
                return self.fail(start, "invalid or unsupported C# syntax");
            }
            let name = self.token(start);
            if !self.reads(start, end).is_empty() {
                if self.token(start + 1) != "=" {
                    return self.fail(start, "assign each console input to a separate variable");
                }
                result.push(self.input_value(
                    start + 2,
                    end,
                    name,
                    ty.clone(),
                    self.line(start),
                )?);
            } else {
                let value = self.raw(start, end);
                result.push(op(
                    "process",
                    if value.contains('=') {
                        value
                    } else {
                        format!("{ty} {value}")
                    },
                    self.line(start),
                ));
            }
        }
        Ok(Some(result))
    }
    fn expression(&self, a: usize, b: usize) -> Result<Vec<Block>> {
        if a == b {
            return Ok(Vec::new());
        }
        if let Some(declaration) = self.declaration(a, b)? {
            return Ok(declaration);
        }
        let calls = self.calls(a, b);
        if let Some(write) = calls.iter().find(|c| {
            c.start == a
                && c.end == b
                && CONSOLE
                    .captures(&c.function)
                    .is_some_and(|m| matches!(&m[1], "Write" | "WriteLine"))
        }) {
            if !self.reads(a, b).is_empty() {
                return self.fail(a, "assign console input to a variable before writing it");
            }
            if write.open + 1 == write.end - 1 {
                return Ok(Vec::new());
            }
            let arguments = self.split(write.open + 1, write.end - 1, ",");
            let value = if arguments.len() == 1 {
                literal(&self.raw(arguments[0].0, arguments[0].1))
            } else {
                self.raw(a, b)
            };
            return Ok(vec![op(
                "output",
                format!("Izvade:\n{value}"),
                self.line(a),
            )]);
        }
        if !self.reads(a, b).is_empty() {
            let assignment = self.split(a, b, "=");
            if assignment.len() > 1 {
                let (start, end) = assignment[0];
                return Ok(vec![self.input_value(
                    end + 1,
                    b,
                    &self.raw(start, end),
                    String::new(),
                    self.line(a),
                )?]);
            }
            if (a..b).any(|i| matches!(self.token(i), "+=" | "-=" | "*=" | "/=" | "%=" | "??=")) {
                return self.fail(a, "assign the input before a compound assignment");
            }
            return Ok(vec![self.input_value(
                a,
                b,
                "teksts",
                String::new(),
                self.line(a),
            )?]);
        }
        Ok(vec![op("process", self.raw(a, b), self.line(a))])
    }
    fn body(&mut self, end: usize, prompts: bool, depth: usize) -> Result<Vec<Block>> {
        if depth > 256 {
            return self.fail(self.position, "C# nesting is too deep");
        }
        let mut chunks: Vec<(Vec<Block>, Option<String>)> = Vec::new();
        while self.position < end {
            let start = self.position;
            if self.token(start) == ";" {
                self.position += 1;
                continue;
            }
            let terminating = matches!(self.token(start), "return" | "break" | "continue");
            let mut values = self.statement(depth + 1)?;
            let reads = !self.reads(start, self.position).is_empty();
            if prompts
                && reads
                && values.first().is_some_and(|v| kind(v) == "input")
                && chunks.last().is_some_and(|(_, p)| p.is_some())
            {
                let (_, prompt) = chunks.pop().unwrap();
                values[0].prompt = prompt;
            }
            let prompt = if prompts {
                self.calls(start, self.position)
                    .into_iter()
                    .find(|c| {
                        c.start == start
                            && self.token(c.end) == ";"
                            && CONSOLE
                                .captures(&c.function)
                                .is_some_and(|m| matches!(&m[1], "Write" | "WriteLine"))
                    })
                    .map(|c| {
                        let args = self.split(c.open + 1, c.end - 1, ",");
                        if c.open + 1 == c.end - 1 {
                            String::new()
                        } else {
                            literal(&self.raw(args[0].0, args[0].1))
                        }
                    })
            } else {
                None
            };
            chunks.push((values, prompt));
            if prompts && terminating {
                self.position = end;
                break;
            }
        }
        Ok(group(chunks.into_iter().flat_map(|(v, _)| v).collect()))
    }
    fn nested_body(&mut self, depth: usize) -> Result<Vec<Block>> {
        self.statement(depth + 1)
    }
    fn parentheses(&mut self) -> Result<(usize, usize)> {
        self.expect("(")?;
        let a = self.position;
        let b = self.tokens[a - 1].mate.unwrap();
        self.position = b + 1;
        Ok((a, b))
    }
    fn statement(&mut self, depth: usize) -> Result<Vec<Block>> {
        if depth > 256 {
            return self.fail(self.position, "C# nesting is too deep");
        }
        let start = self.position;
        let line = self.line(start);
        let keyword = self.token(start).to_owned();
        self.position += 1;
        match keyword.as_str() {
            ";" => Ok(Vec::new()),
            "{" => {
                let end = self.tokens[start].mate.unwrap();
                let body = self.body(end, true, depth + 1)?;
                self.expect("}")?;
                Ok(body)
            }
            "if" => {
                let (a, b) = self.parentheses()?;
                let (mut inputs, condition) = self.condition(a, b)?;
                let yes = self.nested_body(depth)?;
                let no = if self.token(self.position) == "else" {
                    self.position += 1;
                    self.nested_body(depth)?
                } else { Vec::new() };
                let mut item = op("if", condition, line);
                item.yes = yes;
                item.no = no;
                inputs.push(item);
                Ok(inputs)
            }
            "while" | "for" | "foreach" | "do" => self.loop_statement(&keyword, line, depth),
            "switch" => self.switch_statement(line, depth),
            "break" | "continue" => {
                self.expect(";")?;
                Ok(vec![op(&keyword, "", line)])
            }
            "return" => {
                let end = self.semicolon()?;
                let value = self.raw(start + 1, end);
                let label = if value.is_empty() { "Beigas".into() } else { format!("Beigas\nreturn {value}") };
                Ok(vec![op("end", label, line)])
            }
            "try" | "throw" | "goto" | "lock" | "using" | "yield" | "checked" | "unchecked" | "fixed" | "unsafe" => {
                self.fail(start, &format!("unsupported {keyword}_statement; use the editable HTML to describe this construct explicitly"))
            }
            _ => {
                self.position = start;
                let end = self.semicolon()?;
                self.expression(start, end)
            }
        }
    }
    fn loop_statement(&mut self, keyword: &str, line: usize, depth: usize) -> Result<Vec<Block>> {
        let mut init = Vec::new();
        let mut step = Vec::new();
        let mut before = Vec::new();
        let condition;
        let mut body;
        if keyword == "do" {
            body = self.nested_body(depth)?;
            self.expect("while")?;
            let (a, b) = self.parentheses()?;
            (before, condition) = self.condition(a, b)?;
            self.expect(";")?;
        } else {
            let (a, b) = self.parentheses()?;
            if keyword == "foreach" {
                let in_index = (a..b)
                    .find(|&i| self.token(i) == "in")
                    .ok_or_else(|| format!("C# line {line}: invalid foreach"))?;
                if in_index <= a || in_index + 1 == b {
                    return self.fail(a, "invalid foreach");
                }
                let name_start = if self.token(in_index - 1) == ")" {
                    self.tokens[in_index - 1].mate.unwrap()
                } else {
                    in_index - 1
                };
                let name = self.raw(name_start, in_index);
                let collection = self.raw(in_index + 1, b);
                condition = format!("Ir nākamais {name} no {collection}?");
                body = vec![op("process", format!("{name} = nākamais elements"), line)];
                body.extend(self.nested_body(depth)?);
            } else {
                if keyword == "for" {
                    let parts = self.split(a, b, ";");
                    if parts.len() != 3 {
                        return self.fail(a, "invalid for statement");
                    }
                    if parts[0].0 < parts[0].1 {
                        init = self
                            .declaration(parts[0].0, parts[0].1)?
                            .unwrap_or_else(|| self.expressions(parts[0].0, parts[0].1, line));
                    }
                    (before, condition) = self.condition(parts[1].0, parts[1].1)?;
                    step = self.expressions(parts[2].0, parts[2].1, line);
                } else {
                    (before, condition) = self.condition(a, b)?;
                }
                body = self.nested_body(depth)?;
            }
        }
        let mut item = op("loop", condition, line);
        item.body = body;
        item.step = group(step);
        item.before_test = before;
        item.test_first = keyword != "do";
        init.push(item);
        Ok(init)
    }
    fn switch_statement(&mut self, line: usize, depth: usize) -> Result<Vec<Block>> {
        let (a, b) = self.parentheses()?;
        let selector = self.raw(a, b);
        self.expect("{")?;
        let mut cases = Vec::new();
        let mut labels = Vec::new();
        while self.token(self.position) != "}" {
            let label = self.position;
            let default = self.token(label) == "default";
            if !default && self.token(label) != "case" {
                return self.fail(
                    label,
                    "switch patterns/guards are unsupported; use constant case labels",
                );
            }
            self.position += 1;
            let value_start = self.position;
            while self.position < self.tokens.len() && self.token(self.position) != ":" {
                self.position += 1;
            }
            if !default {
                if (value_start..self.position).any(|i| self.token(i) == "?") {
                    return self.fail(value_start, "unsupported conditional_expression; use the editable HTML to describe this construct explicitly");
                }
                if (value_start..self.position).any(|i| {
                    matches!(
                        self.token(i),
                        "when" | "var" | ">" | "<" | ">=" | "<=" | "and" | "or" | "not"
                    )
                }) || (self.position == value_start + 2
                    && identifier(self.token(value_start))
                    && identifier(self.token(value_start + 1)))
                {
                    return self.fail(
                        label,
                        "switch patterns/guards are unsupported; use constant case labels",
                    );
                }
                labels.push(Some(self.raw(value_start, self.position)));
            } else {
                labels.push(None);
            }
            self.expect(":")?;
            let mut body = Vec::new();
            while !matches!(self.token(self.position), "case" | "default" | "}" | "") {
                let terminates = matches!(self.token(self.position), "break" | "return");
                body.extend(self.statement(depth + 1)?);
                if terminates {
                    while !matches!(self.token(self.position), "case" | "default" | "}" | "") {
                        if matches!(self.token(self.position), "(" | "[" | "{") {
                            self.position = self.tokens[self.position].mate.unwrap() + 1;
                        } else {
                            self.position += 1;
                        }
                    }
                }
            }
            if !body.is_empty() {
                cases.push(Case {
                    values: labels,
                    body: group(body),
                });
                labels = Vec::new();
            }
        }
        self.expect("}")?;
        let mut item = op("switch", selector, line);
        item.cases = cases;
        Ok(vec![item])
    }
    fn semicolon(&mut self) -> Result<usize> {
        let mut i = self.position;
        while i < self.tokens.len() {
            if self.token(i) == ";" {
                self.position = i + 1;
                return Ok(i);
            }
            if self.token(i) == "}" {
                break;
            }
            if matches!(self.token(i), "(" | "[" | "{") {
                i = self.tokens[i].mate.unwrap() + 1;
            } else {
                i += 1;
            }
        }
        self.fail(self.position, "invalid or unsupported C# syntax")
    }
}

fn identifier(s: &str) -> bool {
    s.chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '@')
        && s.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '@')
}

pub(crate) fn read(path: &Path, method: &str) -> Result<Document> {
    let source = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Document {
        title: Some(
            path.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        ),
        source: Some(
            path.canonicalize()
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .into_owned(),
        ),
        body: parse(&source, method)?,
        ..Document::default()
    })
}

pub(crate) fn parse(source: &str, method: &str) -> Result<Vec<Block>> {
    let source = source
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let tokens = lex(&source)?;
    let mut reader = Reader {
        source: &source,
        tokens,
        position: 0,
    };
    reader.validate_syntax()?;
    let mut methods = Vec::new();
    for i in 1..reader.tokens.len() {
        if reader.token(i) == method
            && (identifier(reader.token(i - 1))
                || matches!(reader.token(i - 1), ">" | ">>" | "]" | "?"))
            && !matches!(
                reader.token(i - 1),
                "return" | "await" | "throw" | "new" | "else" | "yield"
            )
        {
            let parameters = if reader.token(i + 1) == "<" {
                reader
                    .generic_end(i + 1, reader.tokens.len())
                    .unwrap_or(i + 1)
            } else {
                i + 1
            };
            if reader.token(parameters) != "(" {
                continue;
            }
            let mut after = reader.tokens[parameters].mate.unwrap() + 1;
            if reader.token(after) == "where" {
                while after < reader.tokens.len()
                    && !matches!(reader.token(after), "{" | "=>" | ";")
                {
                    after += 1;
                }
            }
            if matches!(reader.token(after), "{" | "=>" | ";") {
                methods.push((i, after));
            }
        }
    }
    if methods.len() > 1 {
        return Err(format!(
            "Multiple methods named {method}; provide a file with one entry method"
        ));
    }
    if let Some(&(name, body)) = methods.first() {
        if reader.token(body) != "{" {
            return reader.fail(name, "entry method must have a statement body");
        }
        reader.position = body;
        reader.statement(0)
    } else {
        if method != "Main" {
            return Err(format!("No {method} method or top-level statements found"));
        }
        let mut operations = Vec::new();
        let mut globals = false;
        while reader.position < reader.tokens.len() {
            let start = reader.position;
            if matches!(reader.token(start), "using" | "extern") && reader.token(start + 1) != "("
                || reader.token(start) == "global" && reader.token(start + 1) == "using"
            {
                reader.semicolon()?;
                continue;
            }
            let mut declaration = start;
            while matches!(
                reader.token(declaration),
                "public"
                    | "internal"
                    | "private"
                    | "protected"
                    | "static"
                    | "partial"
                    | "abstract"
                    | "sealed"
                    | "readonly"
                    | "ref"
                    | "file"
            ) || reader.token(declaration) == "["
            {
                if reader.token(declaration) == "[" {
                    declaration = reader.tokens[declaration].mate.unwrap() + 1;
                } else {
                    declaration += 1;
                }
            }
            if matches!(
                reader.token(declaration),
                "namespace" | "class" | "struct" | "interface" | "record" | "enum" | "delegate"
            ) {
                while reader.position < reader.tokens.len()
                    && !matches!(reader.token(reader.position), "{" | ";")
                {
                    reader.position += 1;
                }
                if reader.token(reader.position) == "{" {
                    reader.position = reader.tokens[reader.position].mate.unwrap() + 1;
                } else {
                    reader.position += 1;
                }
                if reader.token(declaration) == "namespace" {
                    break;
                }
                continue;
            }
            globals = true;
            operations.extend(reader.statement(0)?);
        }
        if !globals {
            return Err(format!("No {method} method or top-level statements found"));
        }
        Ok(group(operations))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_prompt_parse_check_and_loop_control() {
        let source = r#"{
            Console.Write("N: ");
            if (!int.TryParse(Console.ReadLine(), out int n) || n < 0) return;
            for (int i = 0; i < n; i++) {
                if (i == 1) continue;
                Console.WriteLine($"{i:F2}");
            }
        }"#;
        let mut reader = Reader {
            source,
            tokens: lex(source).unwrap(),
            position: 0,
        };
        let body = reader.statement(0).unwrap();
        assert_eq!(body[0].text, "Ievade:\nn (int)");
        assert_eq!(body[0].prompt.as_deref().unwrap(), "N: ");
        assert_eq!(body[1].text, "n nav int vai n < 0?");
        assert_eq!(body[1].yes[0].kind, "end");
        assert_eq!(body[2].text, "i = 0");
        assert_eq!(body[3].kind, "loop");
        assert_eq!(body[3].step[0].text, "i++");
        assert_eq!(body[3].body[0].yes[0].kind, "continue");
        assert_eq!(body[3].body[1].text, "Izvade:\n{i:F2}");
    }

    #[test]
    fn selects_entry_methods_without_confusing_calls_for_declarations() {
        for source in [
            "class X { static int Main() { return Main(); } }",
            "class X { static Task<int> Main() { return Main(); } }",
            "class X { static int Main<T>() where T:class { return Main<T>(); } }",
        ] {
            let body = parse(source, "Main").unwrap();
            assert_eq!(body.len(), 1);
            assert_eq!(body[0].kind, "end");
        }
        let source = "class X { static void Main() {} static void Calculate() { Console.WriteLine(\"ok\"); } }";
        assert_eq!(parse(source, "Calculate").unwrap()[0].text, "Izvade:\nok");
        assert!(parse(source, "Missing").is_err());
    }

    #[test]
    fn preserves_generic_tuple_and_top_level_declarations() {
        let source = "global using System; Dictionary<string,List<int>> xs=new(); (int n,string s) pair=(1,\"a\"); Console.WriteLine(pair); class Helper{}";
        let body = parse(source, "Main").unwrap();
        assert_eq!(body[0].text, "xs=new()\npair=(1,\"a\")");
        assert_eq!(body[1].text, "Izvade:\npair");
    }

    #[test]
    fn ignores_unreachable_constructs_and_empty_statements_between_prompts() {
        let source = "class X { static void Main() { Console.Write(\"N: \"); ; int n=int.Parse(Console.ReadLine()); return; throw new Exception(); } }";
        let body = parse(source, "Main").unwrap();
        assert_eq!(body.len(), 2);
        assert_eq!(body[0].prompt.as_deref().unwrap(), "N: ");
        assert_eq!(body[1].kind, "end");
    }

    #[test]
    fn rejects_invalid_or_conditional_input_without_panicking() {
        for source in [
            "int n=;",
            "int =1;",
            "int x=1+;",
            "if() return;",
            "if(true){",
            "Console.WriteLine(,);",
            "switch(x){case :break;}",
            "Console.WriteLine(\"x);",
            "/* missing terminator",
            "if(ok && int.TryParse(Console.ReadLine(),out int n)) return;",
            "int n=int.Parse(Console.ReadLine())+1;",
            "string s=(Console.ReadLine());",
        ] {
            assert!(parse(source, "Main").is_err(), "accepted {source}");
        }
        assert!(
            parse(
                "if(Check(ok && ready, int.TryParse(Console.ReadLine(),out int n))) return;",
                "Main"
            )
            .is_ok()
        );
    }
}
