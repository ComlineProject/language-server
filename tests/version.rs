// What `comline-lsp` says about itself: `--version`, and `serverInfo` in the
// `initialize` result. Both carry the `comline-core` commit the build links.

#![cfg(feature = "server")]

use std::io::Write;
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

/// The JSON bodies of the `Content-Length`-framed messages in `stdout`.
fn messages(stdout: &[u8]) -> Vec<serde_json::Value> {
    let mut rest = stdout;
    let mut found = Vec::new();
    while let Some(at) = rest.windows(4).position(|w| w == b"\r\n\r\n") {
        let header = std::str::from_utf8(&rest[..at]).unwrap();
        let length: usize = header
            .lines()
            .find_map(|l| l.strip_prefix("Content-Length: "))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let body = &rest[at + 4..at + 4 + length];
        found.push(serde_json::from_slice(body).unwrap());
        rest = &rest[at + 4 + length..];
    }
    found
}

#[test]
fn test_initialize_reports_server_info_with_core_rev() {
    let mut child = Command::new(BIN)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        let requests = [
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {"processId": null, "rootUri": null, "capabilities": {}}}),
            serde_json::json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}),
            serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "shutdown"}),
            serde_json::json!({"jsonrpc": "2.0", "method": "exit"}),
        ];
        for request in &requests {
            stdin.write_all(&frame(request)).unwrap();
        }
    }
    let output = child.wait_with_output().unwrap();

    let initialize = messages(&output.stdout)
        .into_iter()
        .find(|m| m["id"] == 1)
        .expect("a response to initialize");
    let info = &initialize["result"]["serverInfo"];
    assert_eq!(info["name"], "Comline Language Server", "{initialize}");
    assert_eq!(info["version"], server_version().as_str());
}
