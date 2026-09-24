# Agent A/B eval — claude-sonnet-5 via GitHub Copilot CLI 1.0.89-1

4 tasks × 2 trials per condition, run 2026-09-24.

| Metric | baseline | speedread | speedread vs baseline |
|---|---:|---:|---:|
| Pass rate (pass@1) | 100% | 100% | |
| Mean input tokens / task | 104,797 | 44,636 | 57% lower |
| Mean output tokens / task | 1,266 | 449 | 65% lower |
| Mean cost / task (AI units) | 6.24 | 3.04 | 51% lower |
| Mean session time (s) | 21.6 | 11.3 | 48% lower |
| Mean model calls | 5.00 | 2.25 | 55% lower |
| Mean tool calls | 4.38 | 1.25 | 71% lower |
| Tool-result tokens / task | 1,258 | 702 | 44% lower |
| Used speedread | 0% | 100% | |

Consistency: baseline pass@2 100%, pass^2 100%; speedread pass@2 100%, pass^2 100%

## Per task (means)

| Task | Category | baseline cost | speedread cost | baseline s | speedread s | baseline pass | speedread pass |
|---|---|---:|---:|---:|---:|---:|---:|
| gin-render-impls | Go interface implementations (structural) | 2.90 | 3.13 | 10.0 | 12.2 | 100% | 100% |
| ripgrep-sink-impls | Rust trait implementations | 3.14 | 2.65 | 10.9 | 8.8 | 100% | 100% |
| gin-abort-two-hop | multi-hop callers | 13.07 | 3.90 | 48.2 | 15.7 | 100% | 100% |
| flask-dispatch-callees | resolved callees | 5.87 | 2.50 | 17.3 | 8.6 | 100% | 100% |
