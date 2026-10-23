//! Native algorithm data shared by the C# reader, HTML reader, and renderer.
use crate::Result;

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Document {
    pub title: Option<String>,
    pub source: Option<String>,
    pub layout: Settings,
    pub body: Vec<Block>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Settings {
    pub dpi: f64,
    pub columns: String,
    pub orientation: String,
    pub padding_pt: f64,
    pub gap_pt: f64,
    pub font: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            dpi: 300.,
            columns: "auto".into(),
            orientation: "portrait".into(),
            padding_pt: 10.,
            gap_pt: 26.,
            font: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Block {
    pub kind: String,
    pub text: String,
    pub line: Option<usize>,
    pub prompt: Option<String>,
    pub yes: Vec<Block>,
    pub no: Vec<Block>,
    pub body: Vec<Block>,
    pub step: Vec<Block>,
    pub before_test: Vec<Block>,
    pub test_first: bool,
    pub cases: Vec<Case>,
}

impl Block {
    pub fn new(kind: &str, text: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            text: text.into(),
            line: None,
            prompt: None,
            yes: Vec::new(),
            no: Vec::new(),
            body: Vec::new(),
            step: Vec::new(),
            before_test: Vec::new(),
            test_first: true,
            cases: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Case {
    // None is the default label; Some("null") is the C# null constant.
    pub values: Vec<Option<String>>,
    pub body: Vec<Block>,
}

pub(crate) fn validate(document: &Document) -> Result<()> {
    let settings = &document.layout;
    for (key, value, low, high) in [
        ("dpi", settings.dpi, 72., 600.),
        ("padding_pt", settings.padding_pt, 6., 24.),
        ("gap_pt", settings.gap_pt, 20., 60.),
    ] {
        if !(low..=high).contains(&value) {
            return Err(format!("{key} must be between {low} and {high}"));
        }
    }
    if !matches!(settings.columns.as_str(), "auto" | "1" | "2")
        || !matches!(settings.orientation.as_str(), "portrait" | "landscape")
    {
        return Err(
            "columns must be auto, 1, or 2; orientation must be portrait or landscape".into(),
        );
    }
    validate_body(&document.body, 0)
}

fn validate_body(body: &[Block], depth: usize) -> Result<()> {
    if depth > 64 {
        return Err("Algorithm nesting must be below 64 levels".into());
    }
    for item in body {
        match item.kind.as_str() {
            "process" | "input" | "output" | "end" | "break" | "continue" => (),
            "if" => {
                validate_body(&item.yes, depth + 1)?;
                validate_body(&item.no, depth + 1)?;
            }
            "loop" => {
                validate_body(&item.body, depth + 1)?;
                validate_body(&item.step, depth + 1)?;
                validate_body(&item.before_test, depth + 1)?;
            }
            "switch" => {
                if item.cases.is_empty() || item.cases.iter().any(|c| c.values.is_empty()) {
                    return Err("A switch needs cases with at least one value each".into());
                }
                if item
                    .cases
                    .iter()
                    .flat_map(|c| &c.values)
                    .filter(|v| v.is_none())
                    .count()
                    > 1
                {
                    return Err("Only one default case is allowed".into());
                }
                for case in &item.cases {
                    validate_body(&case.body, depth + 1)?;
                }
            }
            _ => return Err(format!("Unknown block kind: {}", item.kind)),
        }
    }
    Ok(())
}
