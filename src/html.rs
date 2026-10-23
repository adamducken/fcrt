//! Editable HTML is the on-disk algorithm format; labels are plain HTML text.
use crate::{Block, Case, Document, Result};
use html5ever::{parse_document, tendril::TendrilSink};
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use std::collections::BTreeMap;
use std::fmt::Write;

const STYLE: &str = "body{max-width:64rem;margin:2rem auto;padding:0 1rem;font:11pt 'Times New Roman',serif;color:#222;background:#fafafa}\nh1{font-size:18pt;font-weight:normal;white-space:pre-wrap}\narticle{border:1px solid #aaa;background:white;padding:.7rem;margin:.7rem 0}\narticle::before{content:attr(data-kind) ' · ' attr(data-line);font:10pt system-ui;color:#666}\nsection[aria-label]{border-left:2px solid #ccc;padding-left:1rem;margin:.7rem 0}\nsection[aria-label]::before{content:attr(aria-label);font:10pt system-ui;color:#555}\npre{font:inherit;white-space:pre-wrap;overflow-wrap:anywhere;margin:.5rem 0}\n[data-role=prompt]{color:#666}\n[data-role=prompt]::before{content:'Prompt: ';font-style:italic}";

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\r', "&#13;")
}

fn attribute(output: &mut String, name: &str, value: &str) {
    write!(
        output,
        " {name}=\"{}\"",
        escape(value).replace('\n', "&#10;")
    )
    .unwrap();
}

fn label(output: &mut String, role: &str, value: &str, indent: usize) {
    // HTML consumes the first newline after <pre>; always supply that newline.
    writeln!(
        output,
        "{:indent$}<pre data-role=\"{role}\">\n{}</pre>",
        "",
        escape(value)
    )
    .unwrap();
}

fn section(output: &mut String, role: &str, body: &[Block], indent: usize) {
    let caption = match role {
        "yes" => "jā",
        "no" => "nē",
        "body" => "body",
        "step" => "step",
        "before-test" => "before test",
        _ => role,
    };
    writeln!(
        output,
        "{:indent$}<section data-role=\"{role}\" aria-label=\"{caption}\">",
        ""
    )
    .unwrap();
    blocks(output, body, indent + 2);
    writeln!(output, "{:indent$}</section>", "").unwrap();
}

fn blocks(output: &mut String, body: &[Block], indent: usize) {
    for item in body {
        write!(output, "{:indent$}<article", "").unwrap();
        attribute(output, "data-kind", &item.kind);
        if let Some(line) = item.line {
            attribute(output, "data-line", &line.to_string());
        }
        if item.kind == "loop" {
            attribute(
                output,
                "data-test-first",
                if item.test_first { "true" } else { "false" },
            );
        }
        output.push_str(">\n");
        label(output, "text", &item.text, indent + 2);
        if let Some(prompt) = &item.prompt {
            label(output, "prompt", prompt, indent + 2);
        }
        match item.kind.as_str() {
            "if" => {
                section(output, "yes", &item.yes, indent + 2);
                section(output, "no", &item.no, indent + 2);
            }
            "loop" => {
                section(output, "before-test", &item.before_test, indent + 2);
                section(output, "body", &item.body, indent + 2);
                section(output, "step", &item.step, indent + 2);
            }
            "switch" => {
                writeln!(
                    output,
                    "{:width$}<section data-role=\"cases\" aria-label=\"cases\">",
                    "",
                    width = indent + 2
                )
                .unwrap();
                for case in &item.cases {
                    writeln!(
                        output,
                        "{:width$}<section data-role=\"case\" aria-label=\"case\">",
                        "",
                        width = indent + 4
                    )
                    .unwrap();
                    for value in &case.values {
                        label(
                            output,
                            if value.is_some() { "value" } else { "default" },
                            value.as_deref().unwrap_or("default"),
                            indent + 6,
                        );
                    }
                    section(output, "body", &case.body, indent + 6);
                    writeln!(output, "{:width$}</section>", "", width = indent + 4).unwrap();
                }
                writeln!(output, "{:width$}</section>", "", width = indent + 2).unwrap();
            }
            _ => (),
        }
        writeln!(output, "{:indent$}</article>", "").unwrap();
    }
}

pub(crate) fn write(document: &Document) -> String {
    let mut output = format!(
        "<!doctype html>\n<html lang=\"lv\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<title>{}</title>\n<style>\n{STYLE}\n</style>\n</head>\n<body>\n<main data-flow-version=\"1\"",
        escape(document.title.as_deref().unwrap_or("Flowchart"))
    );
    let settings = &document.layout;
    for (key, value) in [
        ("data-dpi", settings.dpi.to_string()),
        ("data-columns", settings.columns.clone()),
        ("data-orientation", settings.orientation.clone()),
        ("data-padding-pt", settings.padding_pt.to_string()),
        ("data-gap-pt", settings.gap_pt.to_string()),
    ] {
        attribute(&mut output, key, &value);
    }
    if let Some(font) = &settings.font {
        attribute(&mut output, "data-font", font);
    }
    if let Some(source) = &document.source {
        attribute(&mut output, "data-source", source);
    }
    output.push_str(">\n");
    if let Some(title) = &document.title {
        writeln!(output, "  <h1 data-role=\"title\">{}</h1>", escape(title)).unwrap();
    }
    section(&mut output, "body", &document.body, 2);
    output.push_str("</main>\n</body>\n</html>\n");
    output
}

fn attr(node: &Handle, key: &str) -> Option<String> {
    if let NodeData::Element { attrs, .. } = &node.data {
        return attrs
            .borrow()
            .iter()
            .find(|a| a.name.local.as_ref() == key)
            .map(|a| a.value.to_string());
    }
    None
}

fn tag(node: &Handle, expected: &str) -> bool {
    matches!(&node.data, NodeData::Element { name, .. } if name.local.as_ref() == expected && name.ns.as_ref() == "http://www.w3.org/1999/xhtml")
}

fn check_attrs(node: &Handle, allowed: &[&str]) -> Result<()> {
    if let NodeData::Element { attrs, .. } = &node.data {
        for a in attrs.borrow().iter() {
            let name = a.name.local.as_ref();
            if name.starts_with("data-") && !allowed.contains(&name) {
                return Err(format!("Unknown HTML attribute: {name}"));
            }
        }
    }
    Ok(())
}

fn children(node: &Handle) -> Result<Vec<Handle>> {
    let mut result = Vec::new();
    for child in node.children.borrow().iter() {
        match &child.data {
            NodeData::Element { .. } => result.push(child.clone()),
            NodeData::Text { contents } if !contents.borrow().trim().is_empty() => {
                return Err("Put algorithm text in <pre data-role=\"text\">".into());
            }
            _ => (),
        }
    }
    Ok(result)
}

fn plain_text(node: &Handle) -> Result<String> {
    let mut result = String::new();
    for child in node.children.borrow().iter() {
        match &child.data {
            NodeData::Text { contents } => result.push_str(&contents.borrow()),
            NodeData::Comment { .. } => (),
            _ if tag(child, "br") => result.push('\n'),
            _ => {
                return Err(
                    "Labels must contain plain text or <br>; escape < and & as &lt; and &amp;"
                        .into(),
                );
            }
        }
    }
    Ok(result)
}

fn fields(node: &Handle) -> Result<BTreeMap<String, Handle>> {
    let mut result = BTreeMap::new();
    for child in children(node)? {
        let role = attr(&child, "data-role").ok_or("Each algorithm field needs data-role")?;
        check_attrs(&child, &["data-role"])?;
        let expected = match role.as_str() {
            "title" => "h1",
            "text" | "prompt" => "pre",
            _ => "section",
        };
        if !tag(&child, expected) {
            return Err(format!("{role} must use <{expected}>"));
        }
        if result.insert(role.clone(), child).is_some() {
            return Err(format!("Duplicate HTML field: {role}"));
        }
    }
    Ok(result)
}

fn take(fields: &mut BTreeMap<String, Handle>, role: &str) -> Result<Handle> {
    fields
        .remove(role)
        .ok_or_else(|| format!("Missing HTML field: {role}"))
}

fn no_fields(fields: BTreeMap<String, Handle>) -> Result<()> {
    if let Some(role) = fields.keys().next() {
        return Err(format!("Unexpected HTML field: {role}"));
    }
    Ok(())
}

fn read_body(node: &Handle, depth: usize) -> Result<Vec<Block>> {
    if depth > 64 {
        return Err("Algorithm nesting must be below 64 levels".into());
    }
    children(node)?
        .iter()
        .map(|node| {
            if !tag(node, "article") {
                return Err("An algorithm block must use <article data-kind=\"…\">".into());
            }
            check_attrs(node, &["data-kind", "data-line", "data-test-first"])?;
            let kind = attr(node, "data-kind").ok_or("A block needs data-kind")?;
            let mut fields = fields(node)?;
            let label = fields.remove("text");
            if label.is_none() && !matches!(kind.as_str(), "break" | "continue") {
                return Err("A block needs <pre data-role=\"text\">".into());
            }
            let mut item = Block::new(
                &kind,
                label
                    .map(|n| plain_text(&n))
                    .transpose()?
                    .unwrap_or_default(),
            );
            if let Some(value) = attr(node, "data-line") {
                let value = value.trim();
                item.line = Some(
                    value
                        .parse::<usize>()
                        .ok()
                        .filter(|&v| v > 0 && value.bytes().all(|c| c.is_ascii_digit()))
                        .ok_or("data-line must be a positive integer")?,
                );
            }
            item.prompt = fields
                .remove("prompt")
                .map(|n| plain_text(&n))
                .transpose()?;
            if let Some(value) = attr(node, "data-test-first") {
                if kind != "loop" {
                    return Err("data-test-first only applies to loop blocks".into());
                }
                item.test_first = match value.as_str() {
                    "true" => true,
                    "false" => false,
                    _ => return Err("data-test-first must be true or false".into()),
                };
            }
            match kind.as_str() {
                "if" => {
                    item.yes = read_body(&take(&mut fields, "yes")?, depth + 1)?;
                    item.no = read_body(&take(&mut fields, "no")?, depth + 1)?;
                }
                "loop" => {
                    item.body = read_body(&take(&mut fields, "body")?, depth + 1)?;
                    if let Some(node) = fields.remove("step") {
                        item.step = read_body(&node, depth + 1)?;
                    }
                    if let Some(node) = fields.remove("before-test") {
                        item.before_test = read_body(&node, depth + 1)?;
                    }
                }
                "switch" => item.cases = read_cases(&take(&mut fields, "cases")?, depth + 1)?,
                "process" | "input" | "output" | "end" | "break" | "continue" => (),
                _ => return Err(format!("Unknown block kind: {kind}")),
            }
            no_fields(fields)?;
            Ok(item)
        })
        .collect()
}

fn read_cases(node: &Handle, depth: usize) -> Result<Vec<Case>> {
    children(node)?
        .iter()
        .map(|node| {
            check_attrs(node, &["data-role"])?;
            if !tag(node, "section") || attr(node, "data-role").as_deref() != Some("case") {
                return Err("Switch cases must use <section data-role=\"case\">".into());
            }
            let mut values = Vec::new();
            let mut body = None;
            for child in children(node)? {
                check_attrs(&child, &["data-role"])?;
                match attr(&child, "data-role").as_deref() {
                    Some("value") if tag(&child, "pre") => values.push(Some(plain_text(&child)?)),
                    Some("default") if tag(&child, "pre") => {
                        plain_text(&child)?;
                        values.push(None);
                    }
                    Some("body") if tag(&child, "section") && body.is_none() => {
                        body = Some(read_body(&child, depth)?)
                    }
                    _ => {
                        return Err(
                            "A case needs value/default labels and exactly one body section".into(),
                        );
                    }
                }
            }
            Ok(Case {
                values,
                body: body.ok_or("A case needs a body section")?,
            })
        })
        .collect()
}

pub(crate) fn read(source: &str) -> Result<Document> {
    let dom = parse_document(RcDom::default(), Default::default()).one(source);
    if dom.errors.iter().any(|e| e == "Duplicate attribute") {
        return Err("Duplicate HTML attribute".into());
    }
    let mut pending = vec![dom.document.clone()];
    let mut roots = Vec::new();
    while let Some(node) = pending.pop() {
        if attr(&node, "data-flow-version").is_some() {
            roots.push(node.clone());
        }
        pending.extend(node.children.borrow().iter().cloned());
    }
    if roots.len() != 1
        || !tag(&roots[0], "main")
        || attr(&roots[0], "data-flow-version").as_deref() != Some("1")
    {
        return Err("HTML needs exactly one <main data-flow-version=\"1\">".into());
    }
    let root = &roots[0];
    check_attrs(
        root,
        &[
            "data-flow-version",
            "data-source",
            "data-dpi",
            "data-columns",
            "data-orientation",
            "data-padding-pt",
            "data-gap-pt",
            "data-font",
        ],
    )?;
    let mut fields = fields(root)?;
    let mut document = Document {
        title: fields.remove("title").map(|n| plain_text(&n)).transpose()?,
        source: attr(root, "data-source"),
        body: read_body(&take(&mut fields, "body")?, 0)?,
        ..Document::default()
    };
    no_fields(fields)?;
    let settings = &mut document.layout;
    for (key, value) in [
        ("data-dpi", &mut settings.dpi),
        ("data-padding-pt", &mut settings.padding_pt),
        ("data-gap-pt", &mut settings.gap_pt),
    ] {
        if let Some(text) = attr(root, key) {
            *value = text
                .trim()
                .parse()
                .map_err(|_| format!("{key} must be a number"))?;
        }
    }
    if let Some(value) = attr(root, "data-columns") {
        settings.columns = value;
    }
    if let Some(value) = attr(root, "data-orientation") {
        settings.orientation = value;
    }
    settings.font = attr(root, "data-font");
    Ok(document)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_control_flow_metadata_and_literal_text() {
        let source = "class X { static void Main() { Console.Write(\"N: \"); int n=int.Parse(Console.ReadLine()); for(int i=0;i<n;i++) { switch(i) { case 0: case 1: Console.WriteLine(\"<b>&amp;\\\"\"); break; default: continue; } } do { n--; } while(n>0); } }";
        let mut document = Document {
            title: Some("\nĀ < & \"\r\n".into()),
            source: Some("a\n\"<&.cs".into()),
            body: crate::csharp::parse(source, "Main").unwrap(),
            ..Document::default()
        };
        document.body.push(Block::new(
            "process",
            "\n\n  x < 3 && y > 0\r\n</pre><script>bad</script>\t",
        ));
        document.layout.dpi = 96.5;
        document.layout.font = Some("/a & b/times.ttf".into());
        let html = write(&document);
        let parsed = read(&html).unwrap();
        assert_eq!(parsed, document);
        assert_eq!(write(&parsed), html);
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn rejects_missing_duplicate_unknown_or_misplaced_fields() {
        for body in [
            "<article data-kind=\"unknown\"><pre data-role=\"text\">x</pre></article>",
            "<article data-kind=\"process\"></article>",
            "<article data-kind=\"process\"><pre data-role=\"text\">x</pre><pre data-role=\"text\">y</pre></article>",
            "<article data-kind=\"if\"><pre data-role=\"text\">x</pre><section data-role=\"yes\"></section></article>",
            "<article data-kind=\"process\" data-line=\"0\"><pre data-role=\"text\">x</pre></article>",
            "<article data-kind=\"process\"><pre data-role=\"text\"><script>bad</script></pre></article>",
            "<article data-kind=\"loop\" data-test-first=\"yes\"><pre data-role=\"text\">x</pre><section data-role=\"body\"></section></article>",
            "<article data-kind=\"switch\"><pre data-role=\"text\">x</pre><section data-role=\"cases\"></section></article>",
            "<section data-role=\"yes\"></section>",
        ] {
            assert!(read(&format!("<main data-flow-version=\"1\"><section data-role=\"body\">{body}</section></main>")).and_then(|document| crate::validate(&document)).is_err(), "accepted {body}");
        }
        for source in [
            "<main></main>",
            "<main data-flow-version=\"2\"></main>",
            "<main data-flow-version=\"1\" data-dpi=\"72\" data-dpi=\"600\"><section data-role=\"body\"></section></main>",
            "<main data-flow-version=\"1\" data-dpi=\"601\"><section data-role=\"body\"></section></main>",
        ] {
            assert!(
                read(source)
                    .and_then(|document| crate::validate(&document))
                    .is_err()
            );
        }
    }

    #[test]
    fn reads_handwritten_html_without_a_generated_wrapper() {
        let document = read(r#"<main data-flow-version="1" data-dpi=" 96.5 ">
          <h1 data-role="title">Pārbaude &amp; izvade</h1>
          <section data-role="body">
            <!-- The no branch is deliberately empty. -->
            <article data-kind="if" data-line=" 3 ">
              <pre data-role="text">n &lt; 5 &amp;&amp; ok?</pre>
              <section data-role="yes">
                <article data-kind="output"><pre data-role="text">Izvade:<br>ā &lt; &gt; &amp; &quot;</pre></article>
              </section>
              <section data-role="no"></section>
            </article>
          </section>
        </main>"#).unwrap();
        assert_eq!(document.title.as_deref(), Some("Pārbaude & izvade"));
        assert_eq!(document.layout.dpi, 96.5);
        assert_eq!(document.body[0].line, Some(3));
        assert_eq!(document.body[0].text, "n < 5 && ok?");
        assert_eq!(document.body[0].yes[0].text, "Izvade:\nā < > & \"");
        assert!(document.body[0].no.is_empty());
    }
}
