mod csharp;
mod drawing;
mod html;
mod ir;

use ir::{Block, Case, Document, Settings, validate};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::ExitCode,
};

type Result<T> = std::result::Result<T, String>;

fn text(item: &Block) -> &str {
    &item.text
}
fn kind(item: &Block) -> &str {
    &item.kind
}

const HELP: &str = "C# console algorithms as Latvian A4 flowcharts (11 pt Times New Roman).\n\nUsage: fcrt [OPTIONS] <INPUT.cs|INPUT.flow.html>\n\n  -o, --output PATH  First PNG path (default: beside input)\n      --fn NAME      C# function to chart, case-sensitive (default: Main)\n      --method NAME  Alias for --fn\n      --title TEXT   Chart title (default: filename, or NAME with --fn)\n      --columns auto|1|2\n      --landscape    Landscape A4\n      --dpi NUMBER   Resolution, 72–600 (default: 300)\n      --font PATH    Times New Roman Regular .ttf\n      --html-only    Extract editable HTML without rendering\n  -h, --help         Show help\n  -V, --version      Show version";

fn run() -> Result<()> {
    let mut input = None;
    let mut output = None;
    let mut method = None;
    let mut title = None;
    let mut columns = None;
    let mut dpi = None;
    let mut font = None;
    let mut landscape = false;
    let mut html_only = false;
    let mut positional = false;
    let mut args = env::args_os().skip(1);
    while let Some(arg) = args.next() {
        let arg_text = arg.to_string_lossy();
        if !positional && arg_text == "--" {
            positional = true;
            continue;
        }
        if !positional && arg_text.starts_with('-') {
            let (flag, inline) = arg_text
                .split_once('=')
                .map_or((arg_text.as_ref(), None), |(a, b)| (a, Some(b)));
            let (flag, inline) =
                if flag.starts_with("-o") && !flag.starts_with("--") && flag.len() > 2 {
                    ("-o", Some(&arg_text[2..]))
                } else {
                    (flag, inline)
                };
            if inline.is_some()
                && matches!(
                    flag,
                    "-h" | "--help" | "-V" | "--version" | "--html-only" | "--landscape"
                )
            {
                return Err(format!("{flag} does not take a value"));
            }
            if matches!(flag, "-h" | "--help") {
                println!("{HELP}");
                return Ok(());
            }
            if matches!(flag, "-V" | "--version") {
                println!("fcrt {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            match flag {
                "--html-only" => html_only = true,
                "--landscape" => landscape = true,
                "-o" | "--output" | "--fn" | "--method" | "--title" | "--columns" | "--dpi"
                | "--font" => {
                    let value = inline
                        .map(std::ffi::OsString::from)
                        .or_else(|| args.next())
                        .ok_or_else(|| format!("{flag} requires a value"))?;
                    if inline.is_none()
                        && value.to_string_lossy().starts_with('-')
                        && value != "-"
                        && value.to_string_lossy().parse::<f64>().is_err()
                    {
                        return Err(format!("{flag} requires a value"));
                    }
                    match flag {
                        "-o" | "--output" => output = Some(PathBuf::from(value)),
                        "--fn" | "--method" => method = Some(value.to_string_lossy().into_owned()),
                        "--title" => title = Some(value.to_string_lossy().into_owned()),
                        "--columns" => {
                            if !matches!(value.to_str(), Some("auto" | "1" | "2")) {
                                return Err("--columns must be auto, 1, or 2".into());
                            }
                            columns = Some(value.to_string_lossy().into_owned());
                        }
                        "--dpi" => {
                            dpi = Some(
                                value
                                    .to_string_lossy()
                                    .parse::<i64>()
                                    .map_err(|_| "--dpi must be an integer")?
                                    as f64,
                            )
                        }
                        "--font" => {
                            let path = PathBuf::from(value);
                            let absolute = if path.is_absolute() {
                                path
                            } else {
                                env::current_dir().map_err(|e| e.to_string())?.join(path)
                            };
                            font = Some(
                                absolute
                                    .canonicalize()
                                    .unwrap_or(absolute)
                                    .to_string_lossy()
                                    .into_owned(),
                            );
                        }
                        _ => unreachable!(),
                    }
                }
                _ => return Err(format!("unrecognized argument: {arg_text}")),
            }
        } else if input.replace(PathBuf::from(arg)).is_some() {
            return Err("expected one input file".into());
        }
    }
    let input = input.ok_or("an input .cs or .html file is required; see fcrt --help")?;
    let extension = input
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mut document = match extension.as_str() {
        "cs" => csharp::read(&input, method.as_deref().unwrap_or("Main"))?,
        "html" | "htm" => html::read(
            &fs::read_to_string(&input).map_err(|e| format!("{}: {e}", input.display()))?,
        )?,
        _ => return Err("Input must be a .cs or .html file".into()),
    };
    let mut base = input.with_extension("");
    if extension != "cs" && base.extension().is_some_and(|s| s == "flow") {
        base.set_extension("");
    }
    let output = output.unwrap_or_else(|| {
        base.with_file_name(format!(
            "{}.png",
            base.file_name().unwrap_or_default().to_string_lossy()
        ))
    });
    if !output
        .extension()
        .is_some_and(|s| s.eq_ignore_ascii_case("png"))
    {
        return Err("--output must end in .png".into());
    }
    if let Some(title) = title.or(method.filter(|_| extension == "cs")) {
        document.title = Some(title);
    }
    if let Some(columns) = columns {
        document.layout.columns = columns;
    }
    if let Some(dpi) = dpi {
        document.layout.dpi = dpi;
    }
    if let Some(font) = font {
        document.layout.font = Some(font);
    }
    if landscape {
        document.layout.orientation = "landscape".into();
    }
    validate(&document)?;
    let sidecar = output.with_extension("flow.html");
    create_parent(&sidecar)?;
    fs::write(&sidecar, html::write(&document)).map_err(|e| e.to_string())?;
    println!("{}", sidecar.display());
    if !html_only {
        for path in drawing::render(&document, &output)? {
            println!("{}", path.display());
        }
    }
    Ok(())
}

fn create_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("fcrt: error: {error}");
            ExitCode::from(2)
        }
    }
}
