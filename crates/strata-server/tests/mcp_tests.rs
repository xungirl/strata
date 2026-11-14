//! End-to-end test of the `strata-mcp` binary over its real transport:
//! newline-delimited JSON-RPC 2.0 on stdin/stdout (MCP stdio transport).
//! Exercises the handshake, every tool, tag filtering, durability across a
//! restart, and that nothing but protocol messages appears on stdout.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{json, Value};

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Mcp {
    fn start(dir: &PathBuf) -> Mcp {
        let mut child = Command::new(env!("CARGO_BIN_EXE_strata-mcp"))
            .arg("--dir")
            .arg(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start strata-mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut m = Mcp {
            child,
            stdin,
            stdout,
            next_id: 1,
        };
        let init = m.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "strata-test", "version": "0" }
            }),
        );
        assert_eq!(init["serverInfo"]["name"], "strata-mcp");
        assert!(init["capabilities"]["tools"].is_object());
        m.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        m
    }

    fn send(&mut self, msg: Value) {
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        loop {
            let mut line = String::new();
            assert!(
                self.stdout.read_line(&mut line).unwrap() > 0,
                "server closed stdout"
            );
            // Every stdout line must be a JSON-RPC message (logs go to stderr).
            let msg: Value = serde_json::from_str(&line).expect("stdout line is JSON");
            assert_eq!(msg["jsonrpc"], "2.0");
            if msg["id"] == id {
                assert!(msg.get("error").is_none(), "{method}: {msg}");
                return msg["result"].clone();
            }
        }
    }

    fn call(&mut self, tool: &str, args: Value) -> Value {
        let r = self.request("tools/call", json!({ "name": tool, "arguments": args }));
        assert_ne!(r["isError"], true, "{tool}: {r}");
        r["structuredContent"].clone()
    }

    fn ids(v: &Value) -> Vec<String> {
        v["memories"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap().to_string())
            .collect()
    }

    fn stop(mut self) {
        drop(self.stdin);
        self.child.wait().unwrap();
    }
}

#[test]
fn mcp_tools_end_to_end_and_durable() {
    let dir = std::env::temp_dir().join(format!("strata-mcp-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    let mut m = Mcp::start(&dir);
    let tools = m.request("tools/list", json!({}));
    let mut names: Vec<&str> = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "forget",
            "memory_stats",
            "recall",
            "remember",
            "remember_many",
            "search"
        ]
    );

    m.call("remember", json!({ "id": "a1", "text": "Alice's favourite food is ramen", "tags": { "user": "alice" } }));
    m.call(
        "remember_many",
        json!({ "memories": [
            { "id": "b1", "text": "Bob's favourite food is pizza", "tags": { "user": "bob" } },
            { "id": "a2", "text": "Alice is learning Rust this summer", "tags": { "user": "alice" } },
            { "text": "an untagged note about ramen shops" }
        ]}),
    );
    assert_eq!(m.call("memory_stats", json!({}))["memories"], 4);

    // Natural-language recall (plain bag of words, punctuation is fine).
    let r = m.call(
        "recall",
        json!({ "query": "What is Alice's favourite food?", "k": 3 }),
    );
    assert_eq!(Mcp::ids(&r)[0], "a1");
    assert!(r["memories"][0]["text"].as_str().unwrap().contains("ramen"));
    // The tag filter restricts recall to one user.
    let r = m.call(
        "recall",
        json!({ "query": "favourite food", "k": 5, "tag_key": "user", "tag_value": "bob" }),
    );
    assert_eq!(Mcp::ids(&r), ["b1"]);
    // Query language: phrase and must-not.
    let r = m.call("search", json!({ "query": "ramen -shops" }));
    assert_eq!(Mcp::ids(&r), ["a1"]);

    m.call("forget", json!({ "id": "a1" }));
    let r = m.call("recall", json!({ "query": "ramen", "k": 5 }));
    assert!(!Mcp::ids(&r).contains(&"a1".to_string()));
    m.stop();

    // Restart: memories (and the deletion) survive; auto ids keep counting.
    let mut m = Mcp::start(&dir);
    assert_eq!(m.call("memory_stats", json!({}))["memories"], 3);
    let r = m.call("recall", json!({ "query": "learning Rust", "k": 1 }));
    assert_eq!(Mcp::ids(&r), ["a2"]);
    let ack = m.call("remember", json!({ "text": "another note" }));
    assert_eq!(ack["ids"][0], "m2");
    m.stop();
    let _ = std::fs::remove_dir_all(&dir);
}
