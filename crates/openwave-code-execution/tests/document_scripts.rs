use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Helpers that turn a document into bounded images under `preview/`; every
/// one of them takes `--preview-dir`.
const PREVIEW_SCRIPTS: [&str; 4] = [
    "render_pdf.py",
    "extract_pdf_figures.py",
    "render_office.py",
    "analyze_xlsx.py",
];

/// Helpers for the in-place OOXML editing pipeline. They read and write
/// package trees and emit no previews.
const PACKAGE_SCRIPTS: [&str; 3] = ["office_unpack.py", "office_pack.py", "pptx_clean.py"];
/// The Calc helpers produce no preview images, so they are held to the
/// weaker contract the sandbox image's smoke check relies on: they import
/// and answer `--help` on a machine with no UNO bridge at all.
const CALC_SCRIPTS: [&str; 2] = ["calc_uno.py", "xlsx_recalc.py"];

fn python() -> Option<PathBuf> {
    ["python3", "python"].into_iter().find_map(|candidate| {
        Command::new(candidate)
            .args(["-c", "import sys; print(sys.executable)"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| {
                let executable = String::from_utf8(output.stdout).ok()?;
                let executable = PathBuf::from(executable.trim());
                executable.is_absolute().then_some(executable)
            })
    })
}

fn scripts_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/exec-documents")
        .canonicalize()
        .expect("document scripts directory exists")
}

fn run_missing_tool_case(python: &Path, script: &Path, input: &Path, needle: &str) -> Output {
    let output = Command::new(python)
        .arg("-S")
        .arg(script)
        .arg(input)
        .env("PATH", "")
        .output()
        .expect("Python starts");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with("error: ") && stderr.contains(needle),
        "expected an actionable missing-tool error containing {needle:?}, got {stderr:?}"
    );
    output
}

#[test]
fn document_scripts_compile_and_expose_argparse_help() {
    let Some(python) = python() else {
        eprintln!("skipping document script contracts: Python is unavailable");
        return;
    };
    let directory = scripts_dir();
    let cache = tempfile::tempdir().unwrap();
    for script in PREVIEW_SCRIPTS.iter().chain(PACKAGE_SCRIPTS.iter()) {
        let path = directory.join(script);
        let compiled = Command::new(&python)
            .args(["-m", "py_compile"])
            .arg(&path)
            .env("PYTHONPYCACHEPREFIX", cache.path())
            .output()
            .expect("Python starts");
        assert!(
            compiled.status.success(),
            "{script} must compile: {}",
            String::from_utf8_lossy(&compiled.stderr)
        );

        let help = Command::new(&python)
            .arg(&path)
            .arg("--help")
            .output()
            .expect("Python starts");
        assert!(
            help.status.success(),
            "{script} --help must succeed: {}",
            String::from_utf8_lossy(&help.stderr)
        );
        let stdout = String::from_utf8_lossy(&help.stdout);
        assert!(stdout.starts_with("usage:"), "{script} uses argparse");
        assert!(
            !PREVIEW_SCRIPTS.contains(script) || stdout.contains("--preview-dir"),
            "{script} must expose --preview-dir"
        );
    }
}

#[test]
fn calc_scripts_expose_help_without_a_uno_bridge() {
    let Some(python) = python() else {
        eprintln!("skipping Calc helper contracts: Python is unavailable");
        return;
    };
    let directory = scripts_dir();
    for script in CALC_SCRIPTS {
        let help = Command::new(&python)
            .arg(directory.join(script))
            .arg("--help")
            .output()
            .expect("Python starts");
        assert!(
            help.status.success(),
            "{script} --help must succeed: {}",
            String::from_utf8_lossy(&help.stderr)
        );
        assert!(
            String::from_utf8_lossy(&help.stdout).starts_with("usage:"),
            "{script} uses argparse"
        );
    }
}

#[test]
fn missing_document_tooling_fails_concisely() {
    let Some(python) = python() else {
        eprintln!("skipping document script diagnostics: Python is unavailable");
        return;
    };
    let directory = scripts_dir();
    let inputs = tempfile::tempdir().unwrap();
    let pdf = inputs.path().join("sample.pdf");
    let docx = inputs.path().join("sample.docx");
    let xlsx = inputs.path().join("sample.xlsx");
    std::fs::write(&pdf, b"not needed: dependency discovery fails first").unwrap();
    std::fs::write(&docx, b"not needed: dependency discovery fails first").unwrap();
    std::fs::write(&xlsx, b"not needed: dependency discovery fails first").unwrap();

    run_missing_tool_case(
        &python,
        &directory.join("render_pdf.py"),
        &pdf,
        "pypdfium2 or pdf2image",
    );
    run_missing_tool_case(
        &python,
        &directory.join("extract_pdf_figures.py"),
        &pdf,
        "pdfimages",
    );
    run_missing_tool_case(
        &python,
        &directory.join("render_office.py"),
        &docx,
        "LibreOffice",
    );
    run_missing_tool_case(
        &python,
        &directory.join("analyze_xlsx.py"),
        &xlsx,
        "openpyxl",
    );
    // Without a UNO bridge the recalculation helper must name the sandbox
    // rather than leave the model looking for an openpyxl workaround.
    run_missing_tool_case(
        &python,
        &directory.join("xlsx_recalc.py"),
        &xlsx,
        "python3-uno",
    );
}
