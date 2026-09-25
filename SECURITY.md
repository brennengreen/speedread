# Security policy

## Reporting a vulnerability

Please report vulnerabilities privately through GitHub: **Security → Report a vulnerability** ([direct link](https://github.com/brennengreen/speedread/security/advisories/new)). Please don't open a public issue. This is a one-person project, so allow a few days for a reply.

Fixes go into the latest release and `main`; older versions aren't patched.

## What counts

speedread is meant to be read-only by construction: it has no write tools, paths are canonicalized and must lie inside a workspace root, and symlinks are resolved before that check. These are in scope:

- reading a file outside the workspace roots and the allowed dependency caches without `--unrestricted`, by any route: `..`, symlinks, globs, `#Symbol` lookups, `path@etag`, MCP roots or `file://` URIs;
- any way to write, delete or execute something through the MCP tools or the CLI;
- a crafted file or request that crashes the server or makes it use unbounded memory or CPU.

These are not vulnerabilities:

- anything reachable with `--unrestricted`, which lifts the path limits by design;
- reading dependency caches (`~/.cargo/registry`, `~/go/pkg/mod`, SwiftPM checkouts, SDKs), which is allowed by default; `--no-deps` turns it off;
- prompt injection through file contents. speedread shows code to the model verbatim, so text in a repository can try to instruct the agent; that is inherent to reading code. Reports where speedread makes it worse, for example by surfacing content the user wouldn't see, are still welcome.
