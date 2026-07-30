use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SCRIPTS: [&str; 4] = [
    "render_pdf.py",
    "extract_pdf_figures.py",
    "render_office.py",
    "analyze_xlsx.py",
];

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
    for script in SCRIPTS {
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
        assert!(stdout.contains("--preview-dir"));
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
}
