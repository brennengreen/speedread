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
    out[at..at + 16].to_string()
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
    assert_eq!(names, vec!["map", "read", "search", "trace"]);
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
    // Symbol-aware: the changed function is named, with its signature status,
    // and the hunk carries git-style function context.
    assert!(
        out.contains("symbols:\n  add [1-7]: body changed, signature unchanged\n"),
        "{out}"
    );
    assert!(out.contains("@@ add\n"), "{out}");
    assert!(
        out.contains("-    let d = c;\n+    let d = c * 2;"),
        "{out}"
    );
    assert!(out.contains("+1 line appended"), "{out}");
    assert!(out.contains("2\tready"), "{out}");

    // Signature change + added function, summarised without hunks (mode=outline).
    let tag = etag_of(&out, "src/lib.rs");
    let src = std::fs::read_to_string(dir.join("src/lib.rs")).unwrap();
    std::fs::write(
        dir.join("src/lib.rs"),
        src.replace("pub fn f3() -> u32 {", "pub fn f3(k: u32) -> u32 {")
            + "\npub fn g() -> u8 {\n    1\n}\n",
    )
    .unwrap();
    let out = c.call(
        "read",
        json!({"targets": [format!("src/lib.rs@{tag}")], "mode": "outline"}),
    );
    assert!(
        out.contains(
            "f3 [25-27]: signature changed: `pub fn f3() -> u32` → `pub fn f3(k: u32) -> u32`"
        ),
        "{out}"
    );
    assert!(
        out.contains("g [133-135]: added `pub fn g() -> u8`"),
        "{out}"
    );
    assert!(!out.contains("@@"), "{out}");
    let src = std::fs::read_to_string(dir.join("src/lib.rs")).unwrap();
    std::fs::write(
        dir.join("src/lib.rs"),
        src.replace("pub fn f3(k: u32) -> u32 {", "pub fn f3() -> u32 {")
            .replace("\npub fn g() -> u8 {\n    1\n}\n", ""),
    )
    .unwrap();
    let out = c.call("read", json!({"targets": [format!("src/lib.rs@{tag}")]}));
    assert!(out.contains("unchanged (131 lines)"), "{out}");

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

    // Trace: callers grouped by enclosing function, callees resolved.
    std::fs::write(
        dir.join("src/use.rs"),
        "use crate::add;\n\npub fn twice(x: i32) -> i32 {\n    // add(1, 2) in a comment is not a call\n    let s = \"add(\";\n    add(x, x)\n}\n",
    )
    .unwrap();
    let out = c.call("trace", json!({"target": "#add"}));
    assert!(out.contains("==> callers of add (src/lib.rs:1-7)"), "{out}");
    assert!(out.contains("1 call site in 1 function"), "{out}");
    assert!(
        out.contains("[3-7] pub fn twice(x: i32) -> i32\n    6\tadd(x, x)"),
        "{out}"
    );
    let out = c.call(
        "trace",
        json!({"target": "src/use.rs#twice", "direction": "callees"}),
    );
    assert!(
        out.contains("→ src/lib.rs:1-7 pub fn add(a: i32, b: i32) -> i32"),
        "{out}"
    );
    let out = c.call("trace", json!({"target": "add", "direction": "references"}));
    assert!(out.contains("1 import"), "{out}");

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

#[test]
fn luau_symbols_and_trace() {
    let dir = std::env::temp_dir().join(format!("speedread-luau-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/Account.luau"),
        "--!strict\nlocal Account = {}\nAccount.__index = Account\n\nfunction Account.new(owner: string)\n\tlocal self = setmetatable({}, Account)\n\tself.balance = 0\n\treturn self\nend\n\n@native\nfunction Account:deposit(amount: number)\n\tself.balance += amount\nend\n\nreturn Account\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/Bank.luau"),
        "local Account = require(script.Parent.Account)\n\nlocal Bank = {}\n\nfunction Bank.open(name: string)\n\tlocal acct = Account.new(name)\n\tacct:deposit(5)\n\treturn acct\nend\n\ngame.Players.PlayerAdded:Connect(function(player)\n\tlocal acct = Bank.open(player.Name)\n\tacct:deposit(1)\nend)\n\nfunction Bank.spawn(name: string)\n\treturn Bank.open(name)\nend\n\ntask.spawn(function()\n\tBank.spawn(\"b\")\nend)\n\nreturn Bank\n",
    )
    .unwrap();
    // Roblox globals called next to a type annotation naming them, and in a
    // `.lua` file that only Luau parses (`+=`).
    std::fs::write(
        dir.join("src/Place.luau"),
        "local function place()\n\tlocal pos: Vector3 = Vector3.new(0, 1, 0)\n\treturn pos\nend\n\nreturn place\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/Tick.lua"),
        "local count = 0\n\nlocal function tick()\n\tcount += 1\n\ttask.spawn(function() end)\nend\n\nreturn tick\n",
    )
    .unwrap();
    let mut c = Client::start(&dir);
    c.rpc(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
    );
    c.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    // A Luau method by `#Class:method`, `path#Class.method` or `#method`, with its attribute.
    for t in [
        "#Account:deposit",
        "src/Account.luau#Account.deposit",
        "#deposit",
    ] {
        let out = c.call("read", json!({"targets": [t]}));
        assert!(
            out.starts_with("==> src/Account.luau:11-14 @") && out.contains("11\t@native\n"),
            "{t}: {out}"
        );
    }

    // `obj:method()` calls resolve to the method, across files and at top level.
    let out = c.call("trace", json!({"target": "#Account:deposit"}));
    assert!(out.contains("2 call sites"), "{out}");
    assert!(
        out.contains("[5-9] function Bank.open(name: string)\n    7\tacct:deposit(5)"),
        "{out}"
    );
    let out = c.call("trace", json!({"target": "#Account.new"}));
    assert!(out.contains("1 call site"), "{out}");
    assert!(!out.contains("Vector3"), "{out}");
    // `task.spawn(...)` is Luau's task library, not a call to `Bank.spawn`.
    let out = c.call("trace", json!({"target": "#Bank.spawn"}));
    assert!(
        out.contains("1 call site") && out.contains("21\tBank.spawn(\"b\")"),
        "{out}"
    );
    assert!(!out.contains("task.spawn"), "{out}");
    let out = c.call(
        "trace",
        json!({"target": "src/Bank.luau#open", "direction": "callees"}),
    );
    assert!(
        out.contains("→ src/Account.luau:5-9 function Account.new(owner: string)"),
        "{out}"
    );
    assert!(
        out.contains("→ src/Account.luau:11-14 function Account:deposit(amount: number)"),
        "{out}"
    );

    // Search hits are grouped under their function.
    let out = c.call("search", json!({"pattern": "balance"}));
    assert!(
        out.contains("[11-14] function Account:deposit(amount: number)\n13\t"),
        "{out}"
    );

    // Lua modules are mostly all `M`: a file's own `M.new()` is not a call to
    // another file's, and a local `vector` module is not a library.
    std::fs::create_dir_all(dir.join("lua")).unwrap();
    for f in ["a", "b"] {
        std::fs::write(
            dir.join(format!("lua/{f}.lua")),
            "local M = {}\n\nfunction M.new()\n  return {}\nend\n\nfunction M.copy()\n  return M.new()\nend\n\nreturn M\n",
        )
        .unwrap();
    }
    let out = c.call("trace", json!({"target": "lua/a.lua#new"}));
    assert!(
        out.contains("1 call site") && out.contains("lua/a.lua\n  [7-9] function M.copy()"),
        "{out}"
    );
    std::fs::write(
        dir.join("lua/vector.lua"),
        "local function create(x, y)\n  return { x = x, y = y }\nend\n\nreturn { create = create }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("lua/main.lua"),
        "local vector = require(\"vector\")\n\nfunction spawn()\n  return vector.create(1, 2)\nend\n",
    )
    .unwrap();
    let out = c.call("trace", json!({"target": "lua/vector.lua#create"}));
    assert!(
        out.contains("[3-5] function spawn()\n    4\treturn vector.create(1, 2)"),
        "{out}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn keys_with_colons_are_found_in_large_workspaces() {
    let dir = std::env::temp_dir().join(format!("speedread-keys-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("package.json"),
        "{\n  \"scripts\": {\n    \"test:unit\": \"vitest run\"\n  }\n}\n",
    )
    .unwrap();
    // Over 200 files mention the name, so definitions are prefiltered.
    for i in 0..210 {
        std::fs::write(
            dir.join(format!("src/f{i}.js")),
            format!("// unit test helper\nexport const value{i} = 1;\n"),
        )
        .unwrap();
    }
    let mut c = Client::start(&dir);
    c.rpc(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
    );
    c.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    let out = c.call("read", json!({"targets": ["#test:unit"]}));
    assert!(out.starts_with("==> package.json:3-3 @"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}
