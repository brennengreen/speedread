# Agent A/B eval — claude-sonnet-5 via GitHub Copilot CLI 1.0.89-1

10 tasks × 3 trials per condition, run 2026-09-23.

| Metric | baseline | speedread | dropin | speedread vs baseline |
|---|---:|---:|---:|---:|
| Pass rate (pass@1) | 97% | 100% | 90% | |
| Mean input tokens / task | 75,511 | 48,984 | 99,290 | 35% lower |
| Mean output tokens / task | 386 | 177 | 509 | 54% lower |
| Mean cost / task (AI units) | 3.90 | 3.07 | 5.17 | 21% lower |
| Mean session time (s) | 12.1 | 14.5 | 14.3 | 19% higher |
| Mean model calls | 3.37 | 2.17 | 4.10 | 36% lower |
| Mean tool calls | 2.37 | 1.20 | 3.20 | 49% lower |
| Tool-result tokens / task | 198 | 220 | 209 | 11% higher |
| Used speedread | 0% | 90% | 0% | |

Consistency: baseline pass@3 100%, pass^3 90%; speedread pass@3 100%, pass^3 100%; dropin pass@1 90%, pass^1 90%

## Per task (means)

| Task | Category | baseline cost | speedread cost | baseline s | speedread s | baseline pass | speedread pass |
|---|---|---:|---:|---:|---:|---:|---:|
| flask-shell-context | control: small symbol | 3.56 | 2.87 | 10.6 | 7.2 | 100% | 100% |
| gin-go-version | control: tiny file | 2.55 | 2.53 | 5.7 | 6.5 | 100% | 100% |
| ripgrep-fixed-strings | large file (8,161 lines) | 3.38 | 2.78 | 10.0 | 6.1 | 100% | 100% |
| ripgrep-walk-run | large file (2,740 lines) | 3.74 | 2.62 | 27.0 | 28.2 | 100% | 100% |
| zod-ip-kinds | large file (5,138 lines) | 4.90 | 3.07 | 14.1 | 43.7 | 67% | 100% |
| flask-dispatch-lines | large file, several symbols | 3.32 | 2.65 | 9.4 | 6.2 | 100% | 100% |
| gin-abort-callers | cross-file search | 2.78 | 2.71 | 6.4 | 6.4 | 100% | 100% |
| vscode-setvalue | huge repo (19k files) | 7.55 | 6.11 | 16.8 | 13.5 | 100% | 100% |
| alamofire-start-immediately | Swift, large file (1,441 lines) | 3.15 | 2.69 | 8.9 | 20.6 | 100% | 100% |
| sap-configuration | Swift, protocol requirement | 4.04 | 2.63 | 12.5 | 6.2 | 100% | 100% |
