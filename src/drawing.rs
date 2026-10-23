//! A4 layout and routing, ported from drawing.py; native font shaping and rasterization.
use crate::{Block, Document, Result, Settings, create_parent, kind, text};
use regex::Regex;
use rustybuzz::{
    Face, UnicodeBuffer,
    ttf_parser::{GlyphId, OutlineBuilder},
};
use std::{
    fs,
    io::BufWriter,
    path::{Path, PathBuf},
    process::Command,
    sync::LazyLock,
};
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};

const FONT_PT: f64 = 11.;
const LINE_HEIGHT: f64 = FONT_PT * 1.3;
type Point = (f64, f64);
static WORDS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s*\S+|\s+$").unwrap());

fn find_font(explicit: Option<&str>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        let path = if let Some(rest) = path.strip_prefix("~/") {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(rest)
        } else {
            PathBuf::from(path)
        };
        return if path.is_file() {
            Ok(path)
        } else {
            Err(format!("Font file not found: {}", path.display()))
        };
    }
    if let Ok(output) = Command::new("fc-match")
        .args([
            "-f",
            "%{family}|%{style}|%{file}",
            "Times New Roman:style=Regular",
        ])
        .output()
    {
        let result = String::from_utf8_lossy(&output.stdout);
        let parts: Vec<_> = result.split('|').collect();
        if output.status.success()
            && parts.len() == 3
            && parts[0].contains("Times New Roman")
            && !parts[1].to_lowercase().contains("bold")
            && Path::new(parts[2]).is_file()
        {
            return Ok(parts[2].into());
        }
    }
    for path in [
        "/usr/share/fonts/truetype/msttcorefonts/Times_New_Roman.ttf",
        "/usr/share/fonts/truetype/msttcorefonts/times.ttf",
        "/Library/Fonts/Times New Roman.ttf",
        "/System/Library/Fonts/Supplemental/Times New Roman.ttf",
        "C:/Windows/Fonts/times.ttf",
    ] {
        if Path::new(path).is_file() {
            return Ok(path.into());
        }
    }
    Err(
        "Times New Roman Regular not found. Install the font or pass --font /path/to/times.ttf"
            .into(),
    )
}

struct Font<'a> {
    face: Face<'a>,
}
impl<'a> Font<'a> {
    fn new(data: &'a [u8]) -> Result<Self> {
        let face = Face::from_slice(data, 0).ok_or("Invalid TrueType font")?;
        let family = face
            .names()
            .into_iter()
            .filter(|n| n.name_id == 1)
            .filter_map(|n| n.to_string())
            .any(|n| n.contains("Times New Roman"));
        if !family || face.is_bold() || face.is_italic() || face.is_oblique() {
            return Err("The font must be Times New Roman Regular (not bold or italic)".into());
        }
        Ok(Self { face })
    }
    fn shape(&self, value: &str) -> rustybuzz::GlyphBuffer {
        let mut buffer = UnicodeBuffer::new();
        buffer.push_str(value);
        buffer.guess_segment_properties();
        rustybuzz::shape(&self.face, &[], buffer)
    }
    fn width(&self, value: &str) -> f64 {
        self.shape(value)
            .glyph_positions()
            .iter()
            .map(|p| p.x_advance as f64)
            .sum::<f64>()
            * FONT_PT
            / self.face.units_per_em() as f64
    }
}

#[derive(Clone, Debug)]
struct Node {
    kind: String,
    #[cfg(test)]
    text: String,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    lines: Vec<String>,
}
impl Node {
    fn top(&self) -> f64 {
        self.y - self.h / 2.
    }
    fn bottom(&self) -> f64 {
        self.y + self.h / 2.
    }
    fn port(&self, side: &str) -> Point {
        match side {
            "top" => (self.x, self.top()),
            "left" => (self.x - self.w / 2., self.y),
            "right" => (self.x + self.w / 2., self.y),
            _ => (self.x, self.bottom()),
        }
    }
}
#[derive(Debug)]
struct Edge {
    points: Vec<Point>,
    arrow: bool,
    label: String,
    label_position: Option<Point>,
    #[cfg(test)]
    source: usize,
    #[cfg(test)]
    target: usize,
}
struct Fragment {
    entry: usize,
    exit: Option<usize>,
    bottom: f64,
    breaks: Vec<usize>,
    continues: Vec<usize>,
}
impl Fragment {
    fn new(entry: usize, exit: Option<usize>, bottom: f64) -> Self {
        Self {
            entry,
            exit,
            bottom,
            breaks: Vec::new(),
            continues: Vec::new(),
        }
    }
}

fn linear(items: &[Block]) -> bool {
    items.iter().all(|i| {
        matches!(
            kind(i),
            "start" | "process" | "input" | "output" | "end" | "break" | "continue"
        )
    })
}

struct Layout<'a> {
    font: Font<'a>,
    settings: Settings,
    dpi: f64,
    scale: f64,
    width: f64,
    height: f64,
    center: f64,
    column_width: f64,
    margin: f64,
    left: f64,
    right: f64,
    single_width: f64,
    padding: f64,
    gap: f64,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    title: String,
}
impl<'a> Layout<'a> {
    fn new(document: &Document, data: &'a [u8]) -> Result<Self> {
        let settings = document.layout.clone();
        let dpi = settings.dpi;
        let (mut width, mut height) = (210. * 72. / 25.4, 297. * 72. / 25.4);
        if settings.orientation == "landscape" {
            std::mem::swap(&mut width, &mut height);
        }
        let column_width = 200_f64.min(width * 0.34);
        let margin = (width - 2. * column_width) / 3.;
        let left = margin + column_width / 2.;
        let mut layout = Self {
            font: Font::new(data)?,
            dpi,
            scale: dpi / 72.,
            width,
            height,
            center: width / 2.,
            column_width,
            margin,
            left,
            right: width - left,
            single_width: 340_f64.min(width - 2. * margin),
            padding: settings.padding_pt,
            gap: settings.gap_pt,
            settings,
            nodes: Vec::new(),
            edges: Vec::new(),
            title: document.title.clone().unwrap_or_default(),
        };
        let mut body = vec![Block::new("start", "Sākums")];
        body.extend(document.body.as_slice().iter().cloned());
        body.push(Block::new("end", "Beigas"));
        let part = if linear(&body) && body.len() >= 4 && layout.fits_lane(&body) {
            layout.fold(&body)?
        } else {
            layout.sequence(&body, 0., None, None, 0)?
        };
        if !part.breaks.is_empty() || !part.continues.is_empty() {
            return Err("break/continue outside a matching switch/loop".into());
        }
        Ok(layout)
    }
    fn wrap(&self, text: &str, width: f64) -> Vec<String> {
        let mut lines = Vec::new();
        for paragraph in text.split('\n') {
            let mut line = String::new();
            for found in WORDS.find_iter(paragraph) {
                let mut word = found.as_str();
                while self.font.width(word) > width {
                    if !line.is_empty() {
                        lines.push(std::mem::take(&mut line));
                    }
                    let mut end = word.chars().next().unwrap().len_utf8();
                    for (offset, c) in word.char_indices().skip(1) {
                        if self.font.width(&word[..offset + c.len_utf8()]) > width {
                            break;
                        }
                        end = offset + c.len_utf8();
                    }
                    lines.push(word[..end].to_owned());
                    word = &word[end..];
                }
                let candidate = format!("{line}{word}");
                if !line.is_empty() && self.font.width(&candidate) > width {
                    lines.push(line);
                    line = word.trim_start().to_owned();
                } else {
                    line = candidate;
                }
            }
            lines.push(line);
        }
        lines
    }
    fn measure(&self, item: &Block, mut width: f64) -> (f64, f64, Vec<String>) {
        let k = kind(item);
        let value = text(item);
        if matches!(k, "start" | "end") {
            width = (value
                .split('\n')
                .map(|l| self.font.width(l))
                .fold(0., f64::max)
                + 42.)
                .min(width)
                .max(88.);
        }
        let inset = if k == "input" { 10. } else { 0. };
        let lines = self.wrap(
            value,
            if matches!(k, "if" | "loop") {
                width * 0.64
            } else {
                width - 2. * self.padding - inset
            },
        );
        let text_height = lines.len() as f64 * LINE_HEIGHT;
        let height = if matches!(k, "if" | "loop") {
            lines
                .iter()
                .enumerate()
                .fold(62_f64, |height, (index, line)| {
                    let dy = ((index as f64 - (lines.len() - 1) as f64 / 2.) * LINE_HEIGHT).abs();
                    height.max(
                        2. * (dy + FONT_PT / 2. + self.padding)
                            / (1. - (self.font.width(line) + 2. * self.padding) / width),
                    )
                })
        } else if matches!(k, "start" | "end") {
            32_f64.max((text_height + self.padding) * 1.45)
        } else {
            44_f64.max(text_height + 2. * self.padding + if k == "output" { 8. } else { 0. })
        };
        (width, height, lines)
    }
    fn node(&mut self, item: &Block, top: f64, x: Option<f64>, width: Option<f64>) -> usize {
        let (w, h, lines) = self.measure(item, width.unwrap_or(self.single_width));
        let id = self.nodes.len();
        self.nodes.push(Node {
            kind: kind(item).into(),
            #[cfg(test)]
            text: text(item).into(),
            x: x.unwrap_or(self.center),
            y: top + h / 2.,
            w,
            h,
            lines,
        });
        id
    }
    fn joint(&mut self, x: f64, y: f64) -> usize {
        let id = self.nodes.len();
        self.nodes.push(Node {
            kind: "joint".into(),
            #[cfg(test)]
            text: String::new(),
            x,
            y,
            w: 0.,
            h: 0.,
            lines: Vec::new(),
        });
        id
    }
    fn rails(&self, depth: usize) -> Result<Point> {
        if depth > 7 {
            return Err("Nesting is too deep for clear A4 routing; simplify the algorithm in the editable HTML".into());
        }
        let inset = (self.margin - 12.).min(self.margin / 4. + depth as f64 * 5.);
        Ok((inset, self.width - inset))
    }
    fn connect(
        &mut self,
        ends: (usize, usize),
        ports: (&str, &str),
        via: Option<Vec<Point>>,
        label: &str,
    ) -> Result<()> {
        let (source, target) = ends;
        let a = self.nodes[source].port(ports.0);
        let b = self.nodes[target].port(ports.1);
        let via = via.unwrap_or_else(|| {
            if a.0 == b.0 || a.1 == b.1 {
                Vec::new()
            } else {
                vec![(a.0, (a.1 + b.1) / 2.), (b.0, (a.1 + b.1) / 2.)]
            }
        });
        let mut points = vec![a];
        points.extend(via);
        points.push(b);
        points.dedup();
        if points
            .windows(2)
            .any(|p| p[0].0 != p[1].0 && p[0].1 != p[1].1)
        {
            return Err("Internal layout error: diagonal connector".into());
        }
        let label_position = if label.is_empty() || points.len() < 2 {
            None
        } else {
            Some(
                points
                    .windows(2)
                    .find(|p| p[0].1 == p[1].1 && (p[0].0 - p[1].0).abs() >= 22.)
                    .map_or((points[0].0 + 21., points[0].1 + FONT_PT), |p| {
                        ((p[0].0 + p[1].0) / 2., p[0].1 - FONT_PT)
                    }),
            )
        };
        let arrow = points.len() >= 2 && self.nodes[target].kind != "joint";
        self.edges.push(Edge {
            points,
            arrow,
            label: label.into(),
            label_position,
            #[cfg(test)]
            source,
            #[cfg(test)]
            target,
        });
        Ok(())
    }
    fn link(&mut self, a: usize, b: usize) -> Result<()> {
        self.connect((a, b), ("bottom", "top"), None, "")
    }
    fn sequence(
        &mut self,
        items: &[Block],
        mut top: f64,
        x: Option<f64>,
        width: Option<f64>,
        depth: usize,
    ) -> Result<Fragment> {
        let (x, width) = (x.unwrap_or(self.center), width.unwrap_or(self.single_width));
        let (mut first, mut previous, mut bottom) = (None, None, top);
        let mut breaks = Vec::new();
        let mut continues = Vec::new();
        for item in items {
            let mut part = match kind(item) {
                "if" => self.branch(item, top, depth)?,
                "switch" => self.switch(item, top, depth)?,
                "loop" => self.loop_block(item, top, depth)?,
                "break" | "continue" => {
                    let joint = self.joint(x, top);
                    let mut part = Fragment::new(joint, None, top);
                    if kind(item) == "break" {
                        part.breaks.push(joint);
                    } else {
                        part.continues.push(joint);
                    }
                    part
                }
                _ => {
                    let node = self.node(item, top, Some(x), Some(width));
                    Fragment::new(
                        node,
                        if kind(item) == "end" {
                            None
                        } else {
                            Some(node)
                        },
                        self.nodes[node].bottom(),
                    )
                }
            };
            if let Some(previous) = previous {
                self.link(previous, part.entry)?;
            }
            first = first.or(Some(part.entry));
            breaks.append(&mut part.breaks);
            continues.append(&mut part.continues);
            previous = part.exit;
            bottom = part.bottom;
            top = bottom + self.gap;
            if previous.is_none() {
                break;
            }
        }
        let entry = first.unwrap_or_else(|| {
            let joint = self.joint(x, top);
            previous = Some(joint);
            joint
        });
        Ok(Fragment {
            entry,
            exit: previous,
            bottom,
            breaks,
            continues,
        })
    }
    fn fits_lane(&self, items: &[Block]) -> bool {
        if self.settings.columns == "1" {
            return false;
        }
        if self.settings.columns == "2" {
            return true;
        }
        items.iter().all(|item| {
            matches!(kind(item), "break" | "continue")
                || (self.measure(item, self.column_width).1 <= 260.
                    && text(item)
                        .split_whitespace()
                        .all(|word| self.font.width(word) <= self.column_width - 2. * self.padding))
        })
    }
    fn fold(&mut self, items: &[Block]) -> Result<Fragment> {
        let items = &items[..=items.iter().position(|i| kind(i) == "end").unwrap()];
        let heights: Vec<f64> = items
            .iter()
            .map(|i| self.measure(i, self.column_width).1 + self.gap)
            .collect();
        let split = (1..items.len())
            .min_by(|&a, &b| {
                let height = |i| {
                    heights[..i]
                        .iter()
                        .sum::<f64>()
                        .max(heights[i..].iter().sum::<f64>())
                };
                height(a).total_cmp(&height(b))
            })
            .unwrap();
        let mut left = self.sequence(
            &items[..split],
            0.,
            Some(self.left),
            Some(self.column_width),
            0,
        )?;
        let Some(exit) = left.exit else {
            return Ok(left);
        };
        let mut right = self.sequence(
            &items[split..],
            0.,
            Some(self.right),
            Some(self.column_width),
            0,
        )?;
        self.connect(
            (exit, right.entry),
            ("bottom", "top"),
            Some(vec![
                (self.left, left.bottom + self.gap / 2.),
                (self.center, left.bottom + self.gap / 2.),
                (self.center, -self.gap / 2.),
                (self.right, -self.gap / 2.),
            ]),
            "",
        )?;
        left.breaks.append(&mut right.breaks);
        left.continues.append(&mut right.continues);
        Ok(Fragment {
            entry: left.entry,
            exit: right.exit,
            bottom: left.bottom.max(right.bottom),
            breaks: left.breaks,
            continues: left.continues,
        })
    }
    fn branch(&mut self, item: &Block, top: f64, depth: usize) -> Result<Fragment> {
        let mut chain = Vec::new();
        let mut current = item;
        while kind(current) == "if"
            && linear(current.yes.as_slice())
            && self.fits_lane(current.yes.as_slice())
        {
            chain.push(current);
            let alternative = current.no.as_slice();
            if alternative.len() == 1 && kind(&alternative[0]) == "if" {
                current = &alternative[0];
            } else {
                break;
            }
        }
        if chain.len() > 1
            && linear(chain.last().unwrap().no.as_slice())
            && self.fits_lane(chain.last().unwrap().no.as_slice())
        {
            return self.ladder(&chain, top, depth);
        }
        let both: Vec<Block> = item
            .yes
            .as_slice()
            .iter()
            .chain(item.no.as_slice())
            .cloned()
            .collect();
        if linear(&both) && self.fits_lane(&both) {
            self.parallel(item, top, depth)
        } else {
            self.stacked(item, top, depth)
        }
    }
    fn parallel(&mut self, item: &Block, top: f64, depth: usize) -> Result<Fragment> {
        let decision = self.node(item, top, None, Some(self.column_width));
        let branch_top = self.nodes[decision].bottom() + self.gap;
        let mut yes = self.sequence(
            item.yes.as_slice(),
            branch_top,
            Some(self.left),
            Some(self.column_width),
            depth + 1,
        )?;
        let mut no = self.sequence(
            item.no.as_slice(),
            branch_top,
            Some(self.right),
            Some(self.column_width),
            depth + 1,
        )?;
        for (part, port, label, column) in [
            (&yes, "left", "jā", self.left),
            (&no, "right", "nē", self.right),
        ] {
            let source = self.nodes[decision].port(port);
            self.connect(
                (decision, part.entry),
                (port, "top"),
                Some(vec![
                    (column, source.1),
                    (column, self.nodes[part.entry].top()),
                ]),
                label,
            )?;
        }
        let bottom = yes.bottom.max(no.bottom) + self.gap / 2.;
        let joint = if yes.exit.is_some() || no.exit.is_some() {
            Some(self.joint(self.center, bottom))
        } else {
            None
        };
        for node in [yes.exit, no.exit].into_iter().flatten() {
            self.connect(
                (node, joint.unwrap()),
                ("bottom", "top"),
                Some(vec![(self.nodes[node].x, bottom)]),
                "",
            )?;
        }
        yes.breaks.append(&mut no.breaks);
        yes.continues.append(&mut no.continues);
        Ok(Fragment {
            entry: decision,
            exit: joint,
            bottom,
            breaks: yes.breaks,
            continues: yes.continues,
        })
    }
    fn ladder(&mut self, chain: &[&Block], mut top: f64, depth: usize) -> Result<Fragment> {
        let (mut first, mut previous) = (None, None);
        let mut exits = Vec::new();
        let mut breaks = Vec::new();
        let mut continues = Vec::new();
        let (left_rail, _) = self.rails(depth)?;
        for item in chain {
            let decision_height = self.measure(item, self.column_width).1;
            let first_height = item
                .yes
                .as_slice()
                .iter()
                .find(|n| !matches!(kind(n), "break" | "continue"))
                .map_or(0., |n| self.measure(n, self.column_width).1);
            let cy = top + decision_height.max(first_height) / 2.;
            let decision = self.node(
                item,
                cy - decision_height / 2.,
                Some(self.right),
                Some(self.column_width),
            );
            let mut yes = self.sequence(
                item.yes.as_slice(),
                cy - first_height / 2.,
                Some(self.left),
                Some(self.column_width),
                depth + 1,
            )?;
            self.connect((decision, yes.entry), ("left", "right"), None, "jā")?;
            if let Some(previous) = previous {
                self.connect((previous, decision), ("bottom", "top"), None, "nē")?;
            }
            first = first.or(Some(decision));
            previous = Some(decision);
            if let Some(exit) = yes.exit {
                exits.push(exit);
            }
            breaks.append(&mut yes.breaks);
            continues.append(&mut yes.continues);
            top = self.nodes[decision].bottom().max(yes.bottom) + self.gap;
        }
        let mut no = self.sequence(
            chain.last().unwrap().no.as_slice(),
            top,
            Some(self.right),
            Some(self.column_width),
            depth + 1,
        )?;
        self.connect((previous.unwrap(), no.entry), ("bottom", "top"), None, "nē")?;
        let bottom = no.bottom + self.gap / 2.;
        let joint = if !exits.is_empty() || no.exit.is_some() {
            Some(self.joint(self.right, bottom))
        } else {
            None
        };
        if let Some(exit) = no.exit {
            self.link(exit, joint.unwrap())?;
        }
        for node in exits {
            let n = &self.nodes[node];
            self.connect(
                (node, joint.unwrap()),
                ("bottom", "top"),
                Some(vec![
                    (n.x, n.bottom() + self.gap / 2.),
                    (left_rail, n.bottom() + self.gap / 2.),
                    (left_rail, bottom),
                ]),
                "",
            )?;
        }
        breaks.append(&mut no.breaks);
        continues.append(&mut no.continues);
        Ok(Fragment {
            entry: first.unwrap(),
            exit: joint,
            bottom,
            breaks,
            continues,
        })
    }
    fn stacked(&mut self, item: &Block, top: f64, depth: usize) -> Result<Fragment> {
        let decision = self.node(item, top, None, None);
        let (left_rail, right_rail) = self.rails(depth)?;
        let mut yes = self.sequence(
            item.yes.as_slice(),
            self.nodes[decision].bottom() + self.gap,
            None,
            None,
            depth + 1,
        )?;
        let mut no = self.sequence(
            item.no.as_slice(),
            yes.bottom + self.gap,
            None,
            None,
            depth + 1,
        )?;
        let d = &self.nodes[decision];
        let entry = &self.nodes[yes.entry];
        self.connect(
            (decision, yes.entry),
            ("left", "top"),
            Some(vec![
                (left_rail, d.y),
                (left_rail, entry.top() - self.gap / 2.),
                (entry.x, entry.top() - self.gap / 2.),
            ]),
            "jā",
        )?;
        let d = &self.nodes[decision];
        let entry = &self.nodes[no.entry];
        self.connect(
            (decision, no.entry),
            ("right", "top"),
            Some(vec![
                (right_rail, d.y),
                (right_rail, entry.top() - self.gap / 3.),
                (entry.x, entry.top() - self.gap / 3.),
            ]),
            "nē",
        )?;
        let bottom = no.bottom + self.gap / 2.;
        let joint = if yes.exit.is_some() || no.exit.is_some() {
            Some(self.joint(self.center, bottom))
        } else {
            None
        };
        if let Some(exit) = yes.exit {
            let n = &self.nodes[exit];
            self.connect(
                (exit, joint.unwrap()),
                ("bottom", "top"),
                Some(vec![
                    (n.x, yes.bottom + self.gap / 3.),
                    (left_rail, yes.bottom + self.gap / 3.),
                    (left_rail, bottom),
                ]),
                "",
            )?;
        }
        if let Some(exit) = no.exit {
            self.link(exit, joint.unwrap())?;
        }
        yes.breaks.append(&mut no.breaks);
        yes.continues.append(&mut no.continues);
        Ok(Fragment {
            entry: decision,
            exit: joint,
            bottom,
            breaks: yes.breaks,
            continues: yes.continues,
        })
    }
    fn switch(&mut self, item: &Block, top: f64, depth: usize) -> Result<Fragment> {
        let mut alternative = Vec::new();
        let mut cases = Vec::new();
        for case in &item.cases {
            let mut body = case.body.as_slice().to_vec();
            if body.last().is_some_and(|n| kind(n) == "break") {
                body.pop();
            }
            if case.values.iter().any(Option::is_none) {
                alternative = body;
            } else {
                cases.push((case.values.as_slice(), body));
            }
        }
        for (values, body) in cases.into_iter().rev() {
            let condition = values
                .iter()
                .map(|v| format!("{} == {}", text(item), v.as_deref().unwrap()))
                .collect::<Vec<_>>()
                .join(" vai ")
                + "?";
            alternative = vec![Block {
                yes: body,
                no: alternative,
                ..Block::new("if", condition)
            }];
        }
        let mut part = self.sequence(&alternative, top, None, None, depth)?;
        if part.breaks.is_empty() {
            return Ok(part);
        }
        let bottom = part.bottom + self.gap / 2.;
        let joint = self.joint(self.center, bottom);
        let (left_rail, _) = self.rails(depth)?;
        if let Some(exit) = part.exit {
            self.link(exit, joint)?;
        }
        for node in part.breaks.drain(..) {
            self.connect(
                (node, joint),
                ("bottom", "top"),
                Some(vec![(left_rail, self.nodes[node].y), (left_rail, bottom)]),
                "",
            )?;
        }
        Ok(Fragment {
            entry: part.entry,
            exit: Some(joint),
            bottom,
            breaks: Vec::new(),
            continues: part.continues,
        })
    }
    fn loop_block(&mut self, item: &Block, top: f64, depth: usize) -> Result<Fragment> {
        let (left_rail, right_rail) = self.rails(depth)?;
        let has_before = !item.before_test.as_slice().is_empty();
        if !item.test_first {
            let body = self.sequence(item.body.as_slice(), top, None, None, depth + 1)?;
            if body.exit.is_none() && body.continues.is_empty() {
                if body.breaks.is_empty() {
                    return Ok(body);
                }
                let joint = self.joint(self.center, body.bottom + self.gap);
                for node in body.breaks {
                    self.link(node, joint)?;
                }
                return Ok(Fragment::new(body.entry, Some(joint), self.nodes[joint].y));
            }
            let before = self.sequence(
                item.before_test.as_slice(),
                body.bottom + self.gap,
                None,
                None,
                depth + 1,
            )?;
            let decision = self.node(
                item,
                before.bottom + if has_before { self.gap } else { 0. },
                None,
                None,
            );
            let test_entry = if has_before { before.entry } else { decision };
            if let Some(exit) = body.exit {
                self.link(exit, test_entry)?;
            }
            for node in body.continues {
                let entry = &self.nodes[test_entry];
                self.connect(
                    (node, test_entry),
                    ("bottom", "top"),
                    Some(vec![
                        (right_rail, self.nodes[node].y),
                        (right_rail, entry.top() - self.gap / 2.),
                        (entry.x, entry.top() - self.gap / 2.),
                    ]),
                    "",
                )?;
            }
            if has_before {
                self.link(before.exit.unwrap(), decision)?;
            }
            let entry = &self.nodes[body.entry];
            self.connect(
                (decision, body.entry),
                ("left", "top"),
                Some(vec![
                    (left_rail, self.nodes[decision].y),
                    (left_rail, entry.top() - self.gap / 2.),
                    (entry.x, entry.top() - self.gap / 2.),
                ]),
                "jā",
            )?;
            let bottom = self.nodes[decision].bottom() + self.gap;
            let joint = self.joint(self.center, bottom);
            self.connect(
                (decision, joint),
                ("right", "top"),
                Some(vec![
                    (right_rail, self.nodes[decision].y),
                    (right_rail, bottom),
                ]),
                "nē",
            )?;
            for node in body.breaks {
                self.connect(
                    (node, joint),
                    ("bottom", "top"),
                    Some(vec![(right_rail, self.nodes[node].y), (right_rail, bottom)]),
                    "",
                )?;
            }
            return Ok(Fragment::new(body.entry, Some(joint), bottom));
        }
        let before = self.sequence(item.before_test.as_slice(), top, None, None, depth + 1)?;
        let decision_top = before.bottom + if has_before { self.gap } else { 0. };
        let both: Vec<Block> = item
            .body
            .as_slice()
            .iter()
            .chain(item.step.as_slice())
            .cloned()
            .collect();
        let use_lanes = linear(&both) && self.fits_lane(&both);
        let (decision, body) = if use_lanes {
            let decision_height = self.measure(item, self.column_width).1;
            let first_height = item
                .body
                .as_slice()
                .iter()
                .find(|n| !matches!(kind(n), "break" | "continue"))
                .map_or(0., |n| self.measure(n, self.column_width).1);
            let cy = decision_top + decision_height.max(first_height) / 2.;
            let decision = self.node(
                item,
                cy - decision_height / 2.,
                Some(self.right),
                Some(self.column_width),
            );
            let body = self.sequence(
                item.body.as_slice(),
                cy - first_height / 2.,
                Some(self.left),
                Some(self.column_width),
                depth + 1,
            )?;
            self.connect((decision, body.entry), ("left", "right"), None, "jā")?;
            (decision, body)
        } else {
            let decision = self.node(item, decision_top, None, None);
            let body = self.sequence(
                item.body.as_slice(),
                self.nodes[decision].bottom() + self.gap,
                None,
                None,
                depth + 1,
            )?;
            let entry = &self.nodes[body.entry];
            self.connect(
                (decision, body.entry),
                ("left", "top"),
                Some(vec![
                    (left_rail, self.nodes[decision].y),
                    (left_rail, entry.top() - self.gap / 2.),
                    (entry.x, entry.top() - self.gap / 2.),
                ]),
                "jā",
            )?;
            (decision, body)
        };
        let entry = if has_before { before.entry } else { decision };
        if has_before {
            self.link(before.exit.unwrap(), decision)?;
        }
        let live_step = body.exit.is_some() || !body.continues.is_empty();
        let step = self.sequence(
            if live_step { item.step.as_slice() } else { &[] },
            body.bottom.max(self.nodes[decision].bottom()) + self.gap,
            Some(if use_lanes { self.left } else { self.center }),
            Some(if use_lanes {
                self.column_width
            } else {
                self.single_width
            }),
            depth + 1,
        )?;
        if let Some(exit) = body.exit {
            self.link(exit, step.entry)?;
        }
        for node in body.continues {
            let n = &self.nodes[step.entry];
            self.connect(
                (node, step.entry),
                ("bottom", "top"),
                Some(vec![
                    (left_rail + 5., self.nodes[node].y),
                    (left_rail + 5., n.top() - self.gap / 2.),
                    (n.x, n.top() - self.gap / 2.),
                ]),
                "",
            )?;
        }
        if live_step {
            self.connect(
                (step.exit.unwrap(), entry),
                ("bottom", "top"),
                Some(vec![
                    (
                        self.nodes[step.exit.unwrap()].x,
                        step.bottom + self.gap / 2.,
                    ),
                    (left_rail, step.bottom + self.gap / 2.),
                    (left_rail, top - self.gap / 2.),
                    (self.nodes[entry].x, top - self.gap / 2.),
                ]),
                "",
            )?;
        }
        let bottom = step.bottom + self.gap;
        let joint = self.joint(self.center, bottom);
        self.connect(
            (decision, joint),
            ("right", "top"),
            Some(vec![
                (right_rail, self.nodes[decision].y),
                (right_rail, bottom),
            ]),
            "nē",
        )?;
        for node in body.breaks {
            self.connect(
                (node, joint),
                ("bottom", "top"),
                Some(vec![(right_rail, self.nodes[node].y), (right_rail, bottom)]),
                "",
            )?;
        }
        Ok(Fragment::new(entry, Some(joint), bottom))
    }
    fn pages(&self) -> Result<Vec<Point>> {
        let mut visible: Vec<_> = self.nodes.iter().filter(|n| n.kind != "joint").collect();
        visible.sort_by(|a, b| a.top().total_cmp(&b.top()));
        let mut bands: Vec<Point> = Vec::new();
        for n in visible {
            if let Some(last) = bands.last_mut().filter(|last| n.top() < last.1) {
                last.1 = last.1.max(n.bottom());
            } else {
                bands.push((n.top(), n.bottom()));
            }
        }
        let capacity = self.height - 110.;
        let horizontal: Vec<_> = self
            .edges
            .iter()
            .flat_map(|e| e.points.windows(2))
            .filter(|p| p[0].1 == p[1].1)
            .map(|p| p[0].1)
            .collect();
        let mut ranges = Vec::new();
        let (mut start, mut bottom) = (-16., -16.);
        for (top, end) in bands {
            if end - top > capacity - 32. {
                return Err(
                    "A block is taller than A4: split its text into smaller blocks in the HTML"
                        .into(),
                );
            }
            if end - start > capacity - 16. {
                let mut boundary = ((bottom + top) / 2.).min(start + capacity);
                if horizontal.iter().any(|y| (boundary - y).abs() < 0.1) {
                    boundary += if boundary + 3. < (top - 6.).min(start + capacity) {
                        3.
                    } else {
                        -3.
                    };
                }
                ranges.push((start, boundary));
                start = boundary;
            }
            bottom = end;
        }
        ranges.push((start, bottom + 16.));
        Ok(ranges)
    }
}

fn clipped(points: &[Point], lower: f64, upper: f64) -> Vec<Vec<Point>> {
    let mut pieces = Vec::new();
    let mut current = Vec::new();
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let segment = if a.1 == b.1 {
            if (lower..=upper).contains(&a.1) {
                Some((a, b))
            } else {
                None
            }
        } else if a.1.max(b.1) < lower || a.1.min(b.1) > upper {
            None
        } else {
            Some((
                (a.0, a.1.clamp(lower, upper)),
                (b.0, b.1.clamp(lower, upper)),
            ))
        };
        if let Some((a, b)) = segment.filter(|(a, b)| a != b) {
            if current.last().is_some_and(|p| *p != a) {
                pieces.push(std::mem::take(&mut current));
            }
            if current.is_empty() {
                current.push(a);
            }
            current.push(b);
        } else if !current.is_empty() {
            pieces.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        pieces.push(current);
    }
    pieces
}

struct Outline {
    builder: PathBuilder,
}
impl OutlineBuilder for Outline {
    fn move_to(&mut self, x: f32, y: f32) {
        self.builder.move_to(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.builder.line_to(x, y);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.builder.quad_to(x1, y1, x, y);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.builder.cubic_to(x1, y1, x2, y2, x, y);
    }
    fn close(&mut self) {
        self.builder.close();
    }
}

struct Canvas<'a, 'b> {
    pixmap: Pixmap,
    layout: &'a Layout<'b>,
    offset: f64,
    stroke: f32,
}
impl Canvas<'_, '_> {
    fn point(&self, p: Point) -> (f32, f32) {
        (
            (p.0 * self.layout.scale).round_ties_even() as f32,
            ((p.1 + self.offset) * self.layout.scale).round_ties_even() as f32,
        )
    }
    fn paint(white: bool, antialias: bool) -> Paint<'static> {
        let mut paint = Paint {
            anti_alias: antialias,
            ..Paint::default()
        };
        paint.set_color(if white { Color::WHITE } else { Color::BLACK });
        paint
    }
    fn draw_path(&mut self, path: &tiny_skia::Path, fill: Option<bool>, outline: bool) {
        if let Some(white) = fill {
            self.pixmap.fill_path(
                path,
                &Self::paint(white, false),
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
        if outline {
            self.pixmap.stroke_path(
                path,
                &Self::paint(false, false),
                &Stroke {
                    width: self.stroke,
                    ..Stroke::default()
                },
                Transform::identity(),
                None,
            );
        }
    }
    fn polyline(&mut self, points: &[Point], fill: Option<bool>, closed: bool, outline: bool) {
        if points.len() < 2 {
            return;
        }
        let mut builder = PathBuilder::new();
        let (x, y) = self.point(points[0]);
        builder.move_to(x, y);
        for p in &points[1..] {
            let (x, y) = self.point(*p);
            builder.line_to(x, y);
        }
        if closed {
            builder.close();
        }
        if let Some(path) = builder.finish() {
            self.draw_path(&path, fill, outline);
        }
    }
    fn ellipse(&mut self, left: f64, top: f64, right: f64, bottom: f64) {
        let (l, t) = self.point((left, top));
        let (r, b) = self.point((right, bottom));
        if let Some(path) = Rect::from_ltrb(l, t, r, b).and_then(PathBuilder::from_oval) {
            self.draw_path(&path, Some(true), true);
        }
    }
    fn text(&mut self, x: f64, y: f64, value: &str, background: bool) {
        let scale = self.layout.scale;
        let shaped = self.layout.font.shape(value);
        let factor = FONT_PT * scale / self.layout.font.face.units_per_em() as f64;
        let width = shaped
            .glyph_positions()
            .iter()
            .map(|p| p.x_advance as f64)
            .sum::<f64>()
            * factor;
        let (px, py) = self.point((x, y));
        let hhea = self.layout.font.face.tables().hhea;
        let baseline = py as f64
            + ((hhea.ascender as f64 * factor).ceil() + (hhea.descender as f64 * factor).floor())
                / 2.;
        if background
            && let Some(rect) = Rect::from_ltrb(
                (px as f64 - width / 2. - 2. * scale) as f32,
                (py as f64 - FONT_PT * scale / 2. - scale) as f32,
                (px as f64 + width / 2. + 2. * scale) as f32,
                (py as f64 + FONT_PT * scale / 2. + scale) as f32,
            )
        {
            self.pixmap
                .fill_rect(rect, &Self::paint(true, false), Transform::identity(), None);
        }
        let mut cursor = px as f64 - width / 2.;
        for (info, position) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
            let mut outline = Outline {
                builder: PathBuilder::new(),
            };
            self.layout
                .font
                .face
                .outline_glyph(GlyphId(info.glyph_id as u16), &mut outline);
            if let Some(path) = outline.builder.finish() {
                let transform = Transform::from_row(
                    factor as f32,
                    0.,
                    0.,
                    -factor as f32,
                    (cursor + position.x_offset as f64 * factor) as f32,
                    (baseline - position.y_offset as f64 * factor) as f32,
                );
                self.pixmap.fill_path(
                    &path,
                    &Self::paint(false, true),
                    FillRule::Winding,
                    transform,
                    None,
                );
            }
            cursor += position.x_advance as f64 * factor;
        }
    }
    fn arrow(&mut self, points: &[Point]) {
        if points.len() < 2 {
            return;
        }
        let a = points[points.len() - 2];
        let b = points[points.len() - 1];
        let distance = (b.0 - a.0).hypot(b.1 - a.1);
        if distance == 0. {
            return;
        }
        let (ux, uy) = ((b.0 - a.0) / distance, (b.1 - a.1) / distance);
        let length = 7_f64.min(distance);
        let base = (b.0 - length * ux, b.1 - length * uy);
        self.polyline(
            &[
                b,
                (base.0 - 2.2 * uy, base.1 + 2.2 * ux),
                (base.0 + 2.2 * uy, base.1 - 2.2 * ux),
            ],
            Some(false),
            true,
            false,
        );
    }
    fn node(&mut self, node: &Node) {
        let (left, right, top, bottom) = (
            node.x - node.w / 2.,
            node.x + node.w / 2.,
            node.top(),
            node.bottom(),
        );
        match node.kind.as_str() {
            "start" | "end" => self.ellipse(left, top, right, bottom),
            "if" | "loop" => self.polyline(
                &[
                    (node.x, top),
                    (right, node.y),
                    (node.x, bottom),
                    (left, node.y),
                ],
                Some(true),
                true,
                true,
            ),
            "input" => self.polyline(
                &[
                    (left + 12., top),
                    (right, top),
                    (right, bottom),
                    (left, bottom),
                    (left, top + 12.),
                ],
                Some(true),
                true,
                true,
            ),
            "output" => {
                let mut vertices = vec![(left, top), (right, top), (right, bottom)];
                for i in 1..=60 {
                    let t = i as f64 / 60.;
                    let u = 1. - t;
                    vertices.push((
                        u.powi(3) * right
                            + 3. * u * u * t * (node.x + node.w / 4.)
                            + 3. * u * t * t * (node.x - node.w / 4.)
                            + t.powi(3) * left,
                        u.powi(3) * bottom
                            + 3. * u * u * t * (bottom - 12.)
                            + 3. * u * t * t * (bottom + 12.)
                            + t.powi(3) * bottom,
                    ));
                }
                self.polyline(&vertices, Some(true), true, true);
            }
            _ => self.polyline(
                &[(left, top), (right, top), (right, bottom), (left, bottom)],
                Some(true),
                true,
                true,
            ),
        }
        for (number, value) in node.lines.iter().enumerate() {
            let y = node.y + (number as f64 - (node.lines.len() - 1) as f64 / 2.) * LINE_HEIGHT;
            self.text(
                node.x,
                y - if node.kind == "output" { 3. } else { 0. },
                value,
                false,
            );
        }
    }
}

struct Crossing {
    point: Point,
    label: String,
    offset: f64,
}

pub(crate) fn render(document: &Document, output: &Path) -> Result<Vec<PathBuf>> {
    let font_path = find_font(document.layout.font.as_deref())?;
    let data = fs::read(&font_path).map_err(|e| format!("{}: {e}", font_path.display()))?;
    let layout = Layout::new(document, &data)?;
    let ranges = layout.pages()?;
    let mut crossings: Vec<Crossing> = Vec::new();
    for &(_, boundary) in &ranges[..ranges.len() - 1] {
        for edge in &layout.edges {
            for pair in edge.points.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                if a.0 == b.0
                    && a.1.min(b.1) < boundary
                    && boundary < a.1.max(b.1)
                    && !crossings.iter().any(|c| c.point == (a.0, boundary))
                {
                    crossings.push(Crossing {
                        point: (a.0, boundary),
                        label: (crossings.len() + 1).to_string(),
                        offset: 0.,
                    });
                }
            }
        }
    }
    for &(_, boundary) in &ranges[..ranges.len() - 1] {
        let mut indices: Vec<usize> = (0..crossings.len())
            .filter(|&i| crossings[i].point.1 == boundary)
            .collect();
        indices.sort_by(|&a, &b| crossings[a].point.0.total_cmp(&crossings[b].point.0));
        let mut groups: Vec<Vec<usize>> = Vec::new();
        for index in indices {
            if let Some(last) = groups
                .last_mut()
                .filter(|g| crossings[index].point.0 - crossings[*g.last().unwrap()].point.0 < 19.)
            {
                last.push(index);
            } else {
                groups.push(vec![index]);
            }
        }
        for group in groups {
            if group.len() > 5 {
                return Err(
                    "Too many parallel continuation tracks for A4; simplify the HTML".into(),
                );
            }
            for (number, &index) in group.iter().enumerate() {
                crossings[index].offset = (number as f64 - (group.len() - 1) as f64 / 2.) * 19.;
            }
        }
    }
    let mut images = Vec::new();
    for (index, &(lower, upper)) in ranges.iter().enumerate() {
        let mut pixmap = Pixmap::new(
            (layout.width * layout.scale).round_ties_even() as u32,
            (layout.height * layout.scale).round_ties_even() as u32,
        )
        .ok_or("Cannot allocate A4 image")?;
        pixmap.fill(Color::WHITE);
        let mut canvas = Canvas {
            pixmap,
            layout: &layout,
            offset: 55. - lower,
            stroke: (0.7 * layout.scale).round_ties_even().max(1.) as f32,
        };
        for edge in &layout.edges {
            for mut piece in clipped(&edge.points, lower, upper) {
                let original_end = *piece.last().unwrap();
                for p in &mut piece {
                    if (p.1 == lower || p.1 == upper)
                        && let Some(crossing) = crossings.iter().find(|c| c.point == *p)
                    {
                        p.1 += crossing.offset;
                    }
                }
                canvas.polyline(&piece, None, false, true);
                if edge.arrow && edge.points.last() == Some(&original_end) {
                    canvas.arrow(&piece);
                }
            }
        }
        for node in &layout.nodes {
            if node.kind != "joint" && node.top() >= lower - 0.01 && node.bottom() <= upper + 0.01 {
                canvas.node(node);
            }
        }
        for edge in &layout.edges {
            if let Some((x, y)) = edge
                .label_position
                .filter(|p| (lower..=upper).contains(&p.1))
            {
                canvas.text(x, y, &edge.label, true);
            }
        }
        for crossing in &crossings {
            if crossing.point.1 == lower || crossing.point.1 == upper {
                let (x, y) = (crossing.point.0, crossing.point.1 + crossing.offset);
                let radius = 8_f64.max(layout.font.width(&crossing.label) / 2. + 3.);
                canvas.ellipse(x - radius, y - radius, x + radius, y + radius);
                canvas.text(x, y, &crossing.label, false);
            }
        }
        let title = if ranges.len() > 1 {
            format!("{} ({}/{})", layout.title, index + 1, ranges.len())
        } else {
            layout.title.clone()
        };
        let title_lines = layout.wrap(&title, layout.width - 2. * layout.margin);
        if title_lines.len() > 2 {
            return Err(
                "Title is too long for A4; shorten title in the HTML or pass --title".into(),
            );
        }
        for (number, value) in title_lines.iter().enumerate() {
            canvas.text(
                layout.center,
                25. - canvas.offset + number as f64 * LINE_HEIGHT,
                value,
                false,
            );
        }
        if ranges.len() > 1 {
            canvas.text(
                layout.center,
                layout.height - 22. - canvas.offset,
                &format!("{} / {}", index + 1, ranges.len()),
                false,
            );
        }
        let path = if index == 0 {
            output.to_owned()
        } else {
            output.with_file_name(format!(
                "{}-{:02}.png",
                output.file_stem().unwrap_or_default().to_string_lossy(),
                index + 1
            ))
        };
        create_parent(&path)?;
        let file = fs::File::create(&path).map_err(|e| e.to_string())?;
        let mut encoder = png::Encoder::new(
            BufWriter::new(file),
            canvas.pixmap.width(),
            canvas.pixmap.height(),
        );
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let pixels_per_meter = (layout.dpi / 0.0254).round() as u32;
        encoder.set_pixel_dims(Some(png::PixelDimensions {
            xppu: pixels_per_meter,
            yppu: pixels_per_meter,
            unit: png::Unit::Meter,
        }));
        encoder
            .add_text_chunk(
                "Description".into(),
                "A4; Times New Roman Regular; 11 pt".into(),
            )
            .map_err(|e| e.to_string())?;
        let rgb: Vec<u8> = canvas
            .pixmap
            .data()
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| p[..3].iter().copied())
            .collect();
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(&rgb).map_err(|e| e.to_string())?;
        writer.finish().map_err(|e| e.to_string())?;
        images.push(path);
    }
    Ok(images)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, VecDeque};

    fn geometry(chart: &Layout<'_>) {
        let pages = chart.pages().unwrap();
        let visible: Vec<_> = chart.nodes.iter().filter(|n| n.kind != "joint").collect();
        for (index, node) in visible.iter().enumerate() {
            assert!(node.x - node.w / 2. >= 0. && node.x + node.w / 2. <= chart.width);
            assert_eq!(
                pages
                    .iter()
                    .filter(|(lo, hi)| *lo <= node.top() && node.bottom() <= *hi)
                    .count(),
                1,
                "{} straddles pages",
                node.text
            );
            assert!(
                visible
                    .iter()
                    .filter(|n| n.top() <= node.top() && node.top() < n.bottom())
                    .count()
                    <= 2
            );
            for other in &visible[index + 1..] {
                assert!(
                    !((node.x - other.x).abs() < (node.w + other.w) / 2. - 0.1
                        && (node.y - other.y).abs() < (node.h + other.h) / 2. - 0.1),
                    "overlap: {} / {}",
                    node.text,
                    other.text
                );
            }
        }
        for edge in &chart.edges {
            for pair in edge.points.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                assert!(a.0 == b.0 || a.1 == b.1, "diagonal connector");
                for node in &visible {
                    if matches!(node.kind.as_str(), "if" | "loop" | "start" | "end") {
                        continue;
                    }
                    let (left, right) = (node.x - node.w / 2. + 0.5, node.x + node.w / 2. - 0.5);
                    let (top, bottom) = (node.top() + 0.5, node.bottom() - 0.5);
                    let vertical = a.0 == b.0
                        && left < a.0
                        && a.0 < right
                        && a.1.min(b.1).max(top) < a.1.max(b.1).min(bottom);
                    let horizontal = a.1 == b.1
                        && top < a.1
                        && a.1 < bottom
                        && a.0.min(b.0).max(left) < a.0.max(b.0).min(right);
                    assert!(
                        !vertical && !horizontal,
                        "connector crosses {}: {a:?} -> {b:?}",
                        node.text
                    );
                }
                for &(_, boundary) in &pages[..pages.len() - 1] {
                    assert!(a.1 != boundary || b.1 != boundary);
                }
            }
        }
    }

    fn with_document(document: Document, check: impl FnOnce(&Layout<'_>)) {
        crate::validate(&document).unwrap();
        let data = fs::read(find_font(None).unwrap()).unwrap();
        let chart = Layout::new(&document, &data).unwrap();
        geometry(&chart);
        check(&chart);
    }

    fn with_source(source: &str, check: impl FnOnce(&Layout<'_>)) {
        let body = crate::csharp::parse(source, "Main").unwrap();
        with_document(
            Document {
                body,
                ..Document::default()
            },
            check,
        );
    }

    fn trace<'a>(chart: &'a Layout<'_>, choices: &[(&str, &[&str])]) -> Vec<&'a str> {
        let mut choices: HashMap<_, VecDeque<_>> = choices
            .iter()
            .map(|(condition, answers)| (*condition, answers.iter().copied().collect()))
            .collect();
        let mut current = chart.nodes.iter().position(|n| n.kind == "start").unwrap();
        let mut visited = Vec::new();
        for _ in 0..500 {
            let node = &chart.nodes[current];
            if node.kind != "joint" {
                visited.push(node.text.as_str());
            }
            if node.kind == "end" {
                return visited;
            }
            let answer = if matches!(node.kind.as_str(), "if" | "loop") {
                Some(
                    choices
                        .get_mut(node.text.as_str())
                        .expect("missing condition choices")
                        .pop_front()
                        .expect("not enough choices"),
                )
            } else {
                None
            };
            let outgoing: Vec<_> = chart
                .edges
                .iter()
                .filter(|e| e.source == current && answer.is_none_or(|label| e.label == label))
                .collect();
            assert_eq!(
                outgoing.len(),
                1,
                "{} has wrong outgoing edge count",
                node.text
            );
            current = outgoing[0].target;
        }
        panic!("control flow failed to terminate");
    }

    #[test]
    fn nested_branches_keep_returns_and_source_order() {
        with_source(
            "if(a) { if(b) Console.WriteLine(\"A\"); else Console.WriteLine(\"B\"); } else { Console.WriteLine(\"C\"); return; } Console.WriteLine(\"D\");",
            |chart| {
                for (choices, messages) in [
                    (
                        vec![("a?", &["jā"][..]), ("b?", &["jā"][..])],
                        vec!["A", "D"],
                    ),
                    (
                        vec![("a?", &["jā"][..]), ("b?", &["nē"][..])],
                        vec!["B", "D"],
                    ),
                    (vec![("a?", &["nē"][..])], vec!["C"]),
                ] {
                    let visited = trace(chart, &choices);
                    assert_eq!(
                        visited
                            .iter()
                            .filter_map(|s| s.strip_prefix("Izvade:\n"))
                            .collect::<Vec<_>>(),
                        messages
                    );
                }
            },
        );
    }

    #[test]
    fn switch_break_and_continue_target_the_right_construct() {
        with_source(
            "int x=1; for(int i=0;i<3;i++) { switch(i) { case 0: if(x>0) break; Console.WriteLine(\"zero\"); break; default: continue; } Console.WriteLine(\"after switch\"); } Console.WriteLine(\"done\");",
            |chart| {
                let visited = trace(
                    chart,
                    &[
                        ("i<3?", &["jā", "nē"]),
                        ("i == 0?", &["jā"]),
                        ("x>0?", &["jā"]),
                    ],
                );
                assert!(visited.contains(&"Izvade:\nafter switch"));
                assert!(!visited.contains(&"Izvade:\nzero"));
                assert!(visited.contains(&"i++"));
                let visited = trace(chart, &[("i<3?", &["jā", "nē"]), ("i == 0?", &["nē"])]);
                assert!(!visited.contains(&"Izvade:\nafter switch"));
                assert!(visited.contains(&"i++"));
                assert!(visited.contains(&"Izvade:\ndone"));
            },
        );
    }

    #[test]
    fn loop_break_skips_increment() {
        with_source(
            "for(int i=0;i<3;i++) { break; } Console.WriteLine(\"done\");",
            |chart| {
                let visited = trace(chart, &[("i<3?", &["jā"])]);
                assert!(!visited.contains(&"i++"));
                assert!(visited.contains(&"Izvade:\ndone"));
            },
        );
    }

    #[test]
    fn do_continue_reaches_test_and_while_rereads_input() {
        with_source(
            "int i=0; do { i++; continue; } while(i<1); Console.WriteLine(\"done\");",
            |chart| {
                let visited = trace(chart, &[("i<1?", &["nē"])]);
                assert_eq!(visited.iter().filter(|s| **s == "i++").count(), 1);
                assert!(visited.contains(&"Izvade:\ndone"));
            },
        );
        with_source(
            "while(int.TryParse(Console.ReadLine(),out int x)) Console.WriteLine(x);",
            |chart| {
                let visited = trace(chart, &[("x ir int?", &["jā", "nē"])]);
                assert_eq!(
                    visited.iter().filter(|s| **s == "Ievade:\nx (int)").count(),
                    2
                );
            },
        );
    }

    #[test]
    fn grouped_cases_and_empty_default_rejoin() {
        with_source(
            "switch(x){case 1:case 2:Console.WriteLine(\"ok\");break;default:break;} Console.WriteLine(\"done\");",
            |chart| {
                let visited = trace(chart, &[("x == 1 vai x == 2?", &["jā"])]);
                assert!(visited.contains(&"Izvade:\nok"));
                assert!(visited.contains(&"Izvade:\ndone"));
                let visited = trace(chart, &[("x == 1 vai x == 2?", &["nē"])]);
                assert!(!visited.contains(&"Izvade:\nok"));
                assert!(visited.contains(&"Izvade:\ndone"));
            },
        );
    }

    #[test]
    fn folds_and_paginates_with_equal_column_spacing() {
        let body: Vec<_> = (0..28)
            .map(|i| Block::new("process", format!("x = {i}")))
            .collect();
        let expected: Vec<_> = std::iter::once("Sākums")
            .chain(body.iter().map(text))
            .chain(std::iter::once("Beigas"))
            .collect();
        with_document(
            Document {
                body: body.clone(),
                ..Document::default()
            },
            |chart| {
                assert!(chart.pages().unwrap().len() > 1);
                assert_eq!(trace(chart, &[]), expected);
                assert!((chart.margin - (chart.left - chart.column_width / 2.)).abs() < 1e-9);
                assert!(
                    (chart.margin - (chart.right - chart.left - chart.column_width)).abs() < 1e-9
                );
                assert!(
                    (chart.margin - (chart.width - chart.right - chart.column_width / 2.)).abs()
                        < 1e-9
                );
            },
        );
    }

    #[test]
    fn preserves_spacing_when_wrapping_and_clips_orthogonally() {
        with_source("Console.WriteLine(\"a  &&  b\");", |chart| {
            assert_eq!(chart.wrap("\"a  &&  b\"", 340.), ["\"a  &&  b\""]);
        });
        let pieces = clipped(&[(10., -20.), (10., 40.), (20., 40.), (20., 80.)], 0., 60.);
        assert_eq!(
            pieces,
            [vec![(10., 0.), (10., 40.), (20., 40.), (20., 60.)]]
        );
    }
}
