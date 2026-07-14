use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

use openwave_host_broker::{
    ConsentMethod, ControlEnvelope, ControlRequest, ExecutionContext, GrantSubject,
    OperationEnvelope, OperationId, OperationRequest, RegisterRootRequest, RequestId,
    RevokeRootRequest, RootId, PROTOCOL_VERSION,
};
use serde::Serialize;
use tempfile::TempDir;
use uuid::Uuid;

fn spawn(temp: &TempDir, home: &Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_openwave-host-broker"))
        .args([
            "--data-dir",
            temp.path().join("app-data").to_str().unwrap(),
            "--home",
            home.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap()
}

fn exchange<T: Serialize>(
    input: &mut ChildStdin,
    output: &mut BufReader<ChildStdout>,
    channel: &str,
    envelope: &T,
) -> serde_json::Value {
    serde_json::to_writer(
        &mut *input,
        &serde_json::json!({ "channel": channel, "envelope": envelope }),
    )
    .unwrap();
    input.write_all(b"\n").unwrap();
    input.flush().unwrap();
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    assert!(!line.is_empty(), "sidecar exited before responding");
    serde_json::from_str(&line).unwrap()
}

#[test]
fn stdio_sidecar_persists_authority_and_audit_across_restart() {
    #[cfg(unix)]
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    #[cfg(windows)]
    let home = PathBuf::from(std::env::var_os("USERPROFILE").unwrap());
    let home = home.canonicalize().unwrap();
    let temp = tempfile::Builder::new()
        .prefix(".openwave-sidecar-test-")
        .tempdir_in(&home)
        .unwrap();
    let root = temp.path().join("Documents/project");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("note.txt"), "sidecar read").unwrap();
    let conversation_id = Uuid::new_v4();
    let subject = GrantSubject::conversation(conversation_id).unwrap();

    let root_id = {
        let mut child = spawn(&temp, &home);
        let mut input = child.stdin.take().unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let response = exchange(
            &mut input,
            &mut output,
            "control",
            &ControlEnvelope {
                protocol_version: PROTOCOL_VERSION,
                request_id: RequestId::new(),
                request: ControlRequest::RegisterRoot(RegisterRootRequest {
                    operation_id: OperationId::new(),
                    subject,
                    conversation_id,
                    path: root.clone(),
                    consent_method: ConsentMethod::FolderPicker,
                }),
            },
        );
        assert_eq!(response["channel"], "control");
        assert_eq!(
            response["envelope"]["response"]["payload"]["result"], "register_root",
            "{response}"
        );
        let root_id = response["envelope"]["response"]["payload"]["root"]["root_id"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(!response.to_string().contains(root.to_str().unwrap()));
        drop(input);
        assert!(child.wait().unwrap().success());
        root_id
    };

    let mut child = spawn(&temp, &home);
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let response = exchange(
        &mut input,
        &mut output,
        "operation",
        &OperationEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(),
            context: ExecutionContext::standalone(conversation_id).unwrap(),
            request: OperationRequest::ListRoots,
        },
    );
    assert_eq!(response["channel"], "operation");
    assert_eq!(
        response["envelope"]["response"]["payload"]["roots"][0]["root_id"],
        root_id
    );
    let private_response = exchange(
        &mut input,
        &mut output,
        "control",
        &ControlEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(),
            request: ControlRequest::RegisterRoot(RegisterRootRequest {
                operation_id: OperationId::new(),
                subject,
                conversation_id,
                path: temp.path().join("app-data"),
                consent_method: ConsentMethod::FolderPicker,
            }),
        },
    );
    assert_eq!(
        private_response["envelope"]["response"]["payload"]["code"],
        "invalid_root"
    );
    let revoke = exchange(
        &mut input,
        &mut output,
        "control",
        &ControlEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(),
            request: ControlRequest::RevokeRoot(RevokeRootRequest {
                operation_id: OperationId::new(),
                subject,
                root_id: root_id.parse::<RootId>().unwrap(),
            }),
        },
    );
    assert_eq!(revoke["envelope"]["response"]["payload"]["revoked"], true);
    let after_revoke = exchange(
        &mut input,
        &mut output,
        "operation",
        &OperationEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(),
            context: ExecutionContext::standalone(conversation_id).unwrap(),
            request: OperationRequest::ListRoots,
        },
    );
    assert_eq!(
        after_revoke["envelope"]["response"]["payload"]["roots"],
        serde_json::json!([])
    );
    drop(input);
    assert!(child.wait().unwrap().success());

    let audit =
        std::fs::read_to_string(temp.path().join("app-data/host-broker-audit.jsonl")).unwrap();
    assert!(audit.contains("register_root"));
    assert!(audit.contains("list_roots"));
    assert!(audit.contains("revoke_root"));
    assert!(!audit.contains(root.to_str().unwrap()));
    assert!(!audit.contains("sidecar read"));
}
