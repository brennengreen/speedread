//! Drives the real binary over MCP stdio: handshake, tool listing, and the
//! read → edit → `path@etag` diff / append flow within one session.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next: u64,
}

impl Client {
    fn start(root: &std::path::Path) -> Client {
        Self::spawn(&["--root", root.to_str().unwrap(), "mcp"], None)
    }

    fn spawn(args: &[&str], cwd: Option<&std::path::Path>) -> Client {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_speedread"));
        if let Some(d) = cwd {
            cmd.current_dir(d);
        }
        let mut child = cmd
            .args(args)
            .env_remove("CLAUDE_PROJECT_DIR")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn speedread");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Client {
            child,
            stdin,
            stdout,
            next: 0,
        }
    }

    fn send(&mut self, v: Value) {
        writeln!(self.stdin, "{v}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.rpc_with_roots(method, params, None)
    }

    /// Like `rpc`, answering any server `roots/list` request with `roots`.
    fn rpc_with_roots(&mut self, method: &str, params: Value, roots: Option<&Value>) -> Value {
        self.next += 1;
        let id = self.next;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let mut line = String::new();
            assert!(
                self.stdout.read_line(&mut line).unwrap() > 0,
                "server closed stdout"
            );
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["method"] == "roots/list" {
                let result = json!({"roots": roots.cloned().unwrap_or(json!([]))});
                self.send(json!({"jsonrpc": "2.0", "id": v["id"], "result": result}));
                continue;
            }
            if v["id"] == json!(id) && v.get("method").is_none() {
                return v;
            }
        }
    }

    fn call(&mut self, tool: &str, args: Value) -> String {
        let r = self.rpc("tools/call", json!({"name": tool, "arguments": args}));
        assert!(r.get("error").is_none(), "tool error: {r}");
        r["result"]["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c["text"].as_str())
            .collect()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn etag_of(out: &str, file: &str) -> String {
    let i = out
        .find(file)
        .unwrap_or_else(|| panic!("{file} not in {out}"));
    let at = out[i..].find('@').unwrap() + i + 1;
    out[at..at + 8].to_string()
}

#[test]
fn mcp_session_end_to_end() {
    let dir = std::env::temp_dir().join(format!("speedread-mcp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let mut lib = String::from(
        "/// Adds.\npub fn add(a: i32, b: i32) -> i32 {\n    let c = a + b;\n    let d = c;\n    let e = d;\n    e\n}\n\npub struct Point {\n    x: i32,\n}\n",
    );
    for i in 0..30 {
        lib.push_str(&format!("\npub fn f{i}() -> u32 {{\n    {i}\n}}\n"));
    }
    std::fs::write(dir.join("src/lib.rs"), &lib).unwrap();
    std::fs::write(dir.join("app.log"), "boot\n").unwrap();

    let mut c = Client::start(&dir);
    let init = c.rpc(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
    );
    assert_eq!(init["result"]["serverInfo"]["name"], "speedread");
    assert!(
        init["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("read")
    );
    c.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    let tools = c.rpc("tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["map", "read", "search"]);
    for t in tools["result"]["tools"].as_array().unwrap() {
        assert_eq!(t["annotations"]["readOnlyHint"], json!(true));
    }

    // Symbol read, including its doc comment.
    let out = c.call("read", json!({"targets": ["src/lib.rs#add", "app.log"]}));
    assert!(out.contains("==> src/lib.rs:1-7"), "{out}");
    assert!(out.contains("1\t/// Adds."), "{out}");
    let lib_tag = etag_of(&out, "src/lib.rs");
    let log_tag = etag_of(&out, "app.log");

    // Edit + append, then re-read by etag: diff and appended lines only.
    let src = std::fs::read_to_string(dir.join("src/lib.rs")).unwrap();
    std::fs::write(
        dir.join("src/lib.rs"),
        src.replace("let d = c;", "let d = c * 2;"),
    )
    .unwrap();
    std::fs::write(dir.join("app.log"), "boot\nready\n").unwrap();
    let out = c.call(
        "read",
        json!({"targets": [format!("src/lib.rs@{lib_tag}"), format!("app.log@{log_tag}")]}),
    );
    assert!(
        out.contains(&format!("(was @{lib_tag}): 1 hunk, +1 -1")),
        "{out}"
    );
    assert!(
        out.contains("-    let d = c;\n+    let d = c * 2;"),
        "{out}"
    );
    assert!(out.contains("+1 line appended"), "{out}");
    assert!(out.contains("2\tready"), "{out}");

    // Unchanged file → one line.
    let new_tag = etag_of(&out, "src/lib.rs");
    let out = c.call("read", json!({"targets": format!("src/lib.rs@{new_tag}")}));
    assert_eq!(
        out.trim_end(),
        format!("==> src/lib.rs @{new_tag} unchanged (131 lines)")
    );

    // Search groups hits under the enclosing symbol.
    let out = c.call("search", json!({"pattern": "let d"}));
    assert!(
        out.contains("[1-7] pub fn add(a: i32, b: i32) -> i32"),
        "{out}"
    );

    // Map lists files with line counts and symbols.
    let out = c.call("map", json!({"symbols": true}));
    assert!(out.contains("lib.rs 131L: add, Point, f0, f1"), "{out}");

    // Lenient arguments: a bare string target and a string budget.
    let out = c.call("read", json!({"path": "src/lib.rs:9-11", "budget": "500"}));
    assert!(out.contains("9\tpub struct Point {"), "{out}");

    // Typos get suggestions rather than a bare failure.
    let out = c.call("read", json!({"targets": ["src/lob.rs"]}));
    assert!(out.contains("Did you mean: src/lib.rs"), "{out}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn client_roots_become_the_workspace() {
    let dir = std::env::temp_dir().join(format!("speedread-roots-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("My App/src")).unwrap();
    std::fs::write(
        dir.join("My App/src/main.rs"),
        "fn main() {\n    println!(\"hi\");\n}\n",
    )
    .unwrap();
    // Launched elsewhere (like a GUI client), with no --root.
    let mut c = Client::spawn(&["mcp"], Some(&std::env::temp_dir()));
    c.rpc(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {"roots": {"listChanged": true}},
               "clientInfo": {"name": "test", "version": "0"}}),
    );
    c.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    let root = dir.canonicalize().unwrap().join("My App");
    let uri = format!("file://{}", root.to_str().unwrap().replace(' ', "%20"));
    let roots = json!([{"uri": uri, "name": "My App"}]);
    let r = c.rpc_with_roots(
        "tools/call",
        json!({"name": "read", "arguments": {"targets": ["src/main.rs#main"]}}),
        Some(&roots),
    );
    let text = r["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(text.starts_with("==> src/main.rs:1-3 @"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}
