// What `comline-lsp` says about itself: `--version`, and `serverInfo` in the
// `initialize` result. Both carry the `comline-core` commit the build links.

#![cfg(feature = "server")]

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};

use comline_language_server::version::{server_version, CORE_REV};

const BIN: &str = env!("CARGO_BIN_EXE_comline-lsp");

#[test]
fn test_version_flag_prints_server_and_core_rev() {
    let output = Command::new(BIN).arg("--version").output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("comline-lsp {}\n", server_version())
    );
    assert!(server_version().contains(&format!("core {CORE_REV}")));
}

fn frame(message: &serde_json::Value) -> Vec<u8> {
    let body = message.to_string();
    format!("Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
}

/// The next `Content-Length`-framed message on `stdout`.
fn read_message(stdout: &mut impl BufRead) -> serde_json::Value {
    let mut length = None;
    loop {
        let mut line = String::new();
        assert_ne!(
            stdout.read_line(&mut line).unwrap(),
            0,
            "the server closed stdout"
        );
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length: ") {
            length = Some(value.parse::<usize>().unwrap());
        }
    }
    let mut body = vec![0; length.expect("a Content-Length header")];
    stdout.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[test]
fn test_initialize_reports_server_info_with_core_rev() {
    let mut child = Command::new(BIN)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    stdin
        .write_all(&frame(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"processId": null, "rootUri": null, "capabilities": {}}
        })))
        .unwrap();
    let initialize = read_message(&mut stdout);

    // Let the server exit rather than leave it for the test runner to reap.
    stdin
        .write_all(&frame(
            &serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "shutdown"}),
        ))
        .unwrap();
    stdin
        .write_all(&frame(
            &serde_json::json!({"jsonrpc": "2.0", "method": "exit"}),
        ))
        .unwrap();
    drop(stdin);
    child.wait().unwrap();

    let info = &initialize["result"]["serverInfo"];
    assert_eq!(info["name"], "Comline Language Server", "{initialize}");
    assert_eq!(info["version"], server_version().as_str());
}
