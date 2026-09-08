//! Focused tests: capfile fail-closed validation, argument parsing, image
//! budget fitting, and the result → ToolOutput mapping.

use super::*;

use tidebreak_core::computer_session::ComputerUseImage;

fn temp_dir() -> tempfile::TempDir {
    tempfile::tempdir().expect("temp dir")
}

fn valid_capfile_json() -> String {
    format!(
        r#"{{"version":1,"endpoint":"http://127.0.0.1:4567/code/native","token":"tbreak_nt_{}"}}"#,
        Uuid::new_v4()
    )
}

#[test]
fn capfile_roundtrips_a_valid_file() {
    let dir = temp_dir();
    let path = dir.path().join("cap.json");
    std::fs::write(&path, valid_capfile_json()).unwrap();
    let cap = NativeCapfile::load(&path).unwrap();
    assert_eq!(cap.endpoint, "http://127.0.0.1:4567/code/native");
    assert!(cap.token.starts_with("tbreak_nt_"));
}

#[test]
fn capfile_rejects_bad_endpoints_and_tokens() {
    let dir = temp_dir();
    let cases = [
        // https scheme
        r#"{"version":1,"endpoint":"https://127.0.0.1:4567/code/native","token":"tbreak_nt_00000000-0000-4000-8000-000000000000"}"#.to_string(),
        // non-loopback host
        r#"{"version":1,"endpoint":"http://10.0.0.5:4567/code/native","token":"tbreak_nt_00000000-0000-4000-8000-000000000000"}"#.to_string(),
        // wrong path (the browser channel's)
        r#"{"version":1,"endpoint":"http://127.0.0.1:4567/code/browser","token":"tbreak_nt_00000000-0000-4000-8000-000000000000"}"#.to_string(),
        // missing port
        r#"{"version":1,"endpoint":"http://127.0.0.1/code/native","token":"tbreak_nt_00000000-0000-4000-8000-000000000000"}"#.to_string(),
        // browser-prefixed token
        r#"{"version":1,"endpoint":"http://127.0.0.1:4567/code/native","token":"tbreak_bt_00000000-0000-4000-8000-000000000000"}"#.to_string(),
        // unsupported version
        r#"{"version":2,"endpoint":"http://127.0.0.1:4567/code/native","token":"tbreak_nt_00000000-0000-4000-8000-000000000000"}"#.to_string(),
        // unknown field (deny_unknown_fields)
        r#"{"version":1,"endpoint":"http://127.0.0.1:4567/code/native","token":"tbreak_nt_00000000-0000-4000-8000-000000000000","owner":"local"}"#.to_string(),
    ];
    for (index, case) in cases.iter().enumerate() {
        let path = dir.path().join(format!("cap-{index}.json"));
        std::fs::write(&path, case).unwrap();
        let error = match NativeCapfile::load(&path) {
            Err(error) => error,
            Ok(_) => panic!("case must fail"),
        };
        let text = error.to_string();
        assert!(
            !text.contains("tbreak_nt_") && !text.contains("tbreak_bt_"),
            "error text must not carry token material: {text}"
        );
    }
}

#[test]
fn parse_requires_a_known_tool_and_json() {
    assert!(parse_computer(vec![]).is_err());
    assert!(parse_computer(vec!["computer_wait".into()]).is_err());
    assert!(parse_computer(vec![
        "computer_not_a_tool".into(),
        "--json".into(),
        "{}".into()
    ])
    .is_err());
    let parsed = parse_computer(vec![
        "computer_wait".into(),
        "--json".into(),
        r#"{"seconds":2}"#.into(),
    ])
    .unwrap();
    assert_eq!(parsed.tool, "computer_wait");
    assert_eq!(parsed.arguments, serde_json::json!({"seconds": 2}));
    assert!(parsed.output.is_none());
}

#[test]
fn parse_accepts_output_and_list_tools() {
    let parsed = parse_computer(vec![
        "computer_capture_screen".into(),
        "--json".into(),
        "{}".into(),
        "--output".into(),
        "/tmp/shot.png".into(),
    ])
    .unwrap();
    assert_eq!(parsed.output, Some(PathBuf::from("/tmp/shot.png")));
    let listed = parse_computer(vec!["list-tools".into()]).unwrap();
    assert_eq!(listed.tool, "list-tools");
}

#[test]
fn every_canonical_tool_parses_by_name() {
    // The CLI accepts the dynamic core list, so a primitive added there is
    // driveable here without a bridge change.
    for spec in computer_use_tool_specs() {
        parse_computer(vec![spec.name.clone(), "--json".into(), "{}".into()])
            .unwrap_or_else(|error| panic!("{} must parse: {error}", spec.name));
    }
}

#[test]
fn control_tools_are_sensitive_and_observation_tools_read_only() {
    let cap = NativeCapfile {
        endpoint: "http://127.0.0.1:1/code/native".into(),
        token: format!("tbreak_nt_{}", Uuid::new_v4()),
    };
    let client = NativeClient::new(&cap).unwrap();
    for spec in computer_use_tool_specs() {
        let name = spec.name.clone();
        let tool = NativeTool {
            spec,
            client: client.clone(),
        };
        let expected = if is_computer_use_control_tool(&name) {
            ApprovalClass::Sensitive
        } else {
            ApprovalClass::ReadOnly
        };
        assert_eq!(tool.approval_class(), expected, "{name}");
    }
}

fn png_bytes(width: u32, height: u32, noisy: bool) -> Vec<u8> {
    let mut seed = 0x2545_F491u64;
    let image = image::RgbImage::from_fn(width, height, |x, y| {
        if noisy {
            // Deterministic noise compresses poorly, so the encoded PNG
            // stays near its raw size and exceeds the budget.
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let v = (seed >> 33) as u32 ^ (x * 31 + y * 17);
            image::Rgb([
                (v & 0xff) as u8,
                ((v >> 8) & 0xff) as u8,
                ((v >> 16) & 0xff) as u8,
            ])
        } else {
            image::Rgb([40, 90, 200])
        }
    });
    let mut out = Vec::new();
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

#[test]
fn small_images_pass_the_budget_untouched() {
    let bytes = png_bytes(64, 64, false);
    let fitted = fit_png_to_budget(bytes.clone()).unwrap();
    assert_eq!(fitted, bytes);
}

#[test]
fn oversized_images_are_recompressed_within_the_budget() {
    let bytes = png_bytes(1400, 1400, true);
    assert!(
        bytes.len() > NATIVE_IMAGE_MAX_BYTES,
        "fixture must exceed the budget ({} bytes)",
        bytes.len()
    );
    let fitted = fit_png_to_budget(bytes).unwrap();
    assert!(fitted.len() <= NATIVE_IMAGE_MAX_BYTES);
    let (width, height) = png_dimensions(&fitted).unwrap();
    assert!(width < 1400 && height < 1400);
    assert!(width > 0 && height > 0);
}

#[test]
fn non_png_results_are_refused() {
    use base64::Engine as _;
    let result = ComputerUseResult {
        request_id: Uuid::new_v4(),
        outcome: ComputerUseOutcome::Completed,
        text: "captured".into(),
        data: serde_json::json!({}),
        error_code: None,
        images: vec![ComputerUseImage {
            mime_type: "image/jpeg".into(),
            base64: base64::engine::general_purpose::STANDARD.encode(b"not png"),
        }],
    };
    let failure = match decode_result_images(&result) {
        Err(failure) => failure,
        Ok(_) => panic!("jpeg must be refused"),
    };
    assert!(failure.redacted_text().contains("image/png"));
}

#[test]
fn completed_results_keep_pixels_out_of_text_and_data() {
    use base64::Engine as _;
    let png = png_bytes(32, 32, false);
    let encoded = base64::engine::general_purpose::STANDARD.encode(&png);
    let result = ComputerUseResult {
        request_id: Uuid::new_v4(),
        outcome: ComputerUseOutcome::Completed,
        text: "Captured the screen.".into(),
        data: serde_json::json!({"windows": 2}),
        error_code: None,
        images: vec![ComputerUseImage {
            mime_type: "image/png".into(),
            base64: encoded.clone(),
        }],
    };
    let output = result_tool_output(&result).unwrap();
    assert_eq!(output.images.len(), 1);
    assert!(!output.content.contains(&encoded));
    assert!(!serde_json::to_string(&output.data)
        .unwrap()
        .contains(&encoded));
}

#[test]
fn unknown_outcomes_instruct_inspection_before_acting() {
    let result = ComputerUseResult {
        request_id: Uuid::new_v4(),
        outcome: ComputerUseOutcome::Unknown,
        text: "The click may not have landed.".into(),
        data: serde_json::json!({}),
        error_code: Some("unknown_outcome".into()),
        images: vec![],
    };
    let output = result_tool_output(&result).unwrap();
    assert!(output.content.contains("unknown"));
    assert!(output.content.contains("before acting again"));
}

#[test]
fn rejected_results_become_failures() {
    let result = ComputerUseResult {
        request_id: Uuid::new_v4(),
        outcome: ComputerUseOutcome::Rejected,
        text: "the user declined".into(),
        data: serde_json::json!({}),
        error_code: Some("consent_denied".into()),
        images: vec![],
    };
    let failure = result_tool_output(&result).expect_err("rejected must fail");
    assert!(failure.redacted_text().contains("declined"));
}

#[test]
fn image_files_are_private_and_symlink_safe() {
    let dir = temp_dir();
    let path = dir.path().join("shot.png");
    write_image_private(&path, b"png-bytes").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"png-bytes");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let link = dir.path().join("link.png");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(write_image_private(&link, b"other").is_err());
    }
    // Overwrite replaces content atomically.
    write_image_private(&path, b"second").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"second");
}

#[test]
fn server_messages_are_scrubbed() {
    assert_eq!(scrub_server_message("bad Bearer abc"), "[redacted]");
    let with_uuid = format!("token {} leaked", Uuid::new_v4());
    assert!(scrub_server_message(&with_uuid).contains("[redacted]"));
    assert_eq!(scrub_server_message("plain detail"), "plain detail");
}
