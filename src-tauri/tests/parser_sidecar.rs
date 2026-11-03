use std::{
    io::{Read, Write},
    process::{Command, Stdio},
};

use private_ai_connector_workspace_lib::{models::ParseRequest, parser_ipc::ParserResponse};

#[test]
fn parses_a_length_delimited_request_out_of_process() {
    let request = ParseRequest {
        file_name: "notes.md".into(),
        extension: "md".into(),
        bytes: b"# Intake\n\nConfirm the signed engagement letter.".to_vec(),
    };
    let payload = serde_json::to_vec(&request).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_workspace-parser"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(&(payload.len() as u32).to_be_bytes())
        .unwrap();
    stdin.write_all(&payload).unwrap();
    drop(stdin);

    let mut output = Vec::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut output)
        .unwrap();
    assert!(child.wait().unwrap().success());
    assert!(output.len() >= 4);
    let length = u32::from_be_bytes(output[..4].try_into().unwrap()) as usize;
    assert_eq!(output.len(), length + 4);
    let response: ParserResponse = serde_json::from_slice(&output[4..]).unwrap();
    let result = response.result.expect("parser should return a result");
    assert!(response.ok);
    assert_eq!(result.blocks[0].location.heading.as_deref(), Some("Intake"));
}
