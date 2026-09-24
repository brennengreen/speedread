# Coding eval — claude-sonnet-5 via GitHub Copilot CLI 1.0.89-1

8 SWE-style bug-fix tasks × 2 trials per condition, run 2026-09-24. Pass = the repository's full test suite passes with tests unmodified.

| Metric | baseline |
|---|---:|
| Pass rate (pass@1) | 100% |
| Mean input tokens / task | 166,147 |
| Median input tokens / task | 124,460 |
| Mean output tokens / task | 1,714 |
| Mean cost / task (AI units) | 7.84 |
| Median model (API) time, s | 20.5 |
| Median session time, s | 33.8 |
| Mean model calls (turns) | 7.8 |
| Mean tool calls | 6.9 |
| Mean read/search calls | 4.5 |
| Read-result tokens / task | 1,604 |
| Source bytes shown / task | 5,150 |
| Repeated source bytes / task | 594 |
| Used speedread (adoption) | 0% |
| Share of reads via speedread | 0% |
| Decisive line shown | 100% |

Consistency: baseline pass@2 100%, pass^2 100%

Compression safety (speedread conditions): 

## Per task (means)

| Task | Difficulty | baseline pass / tokens |
|---|---|---:|
| gin-client-ip | medium | 100% / 131,080 |
| gin-catchall-param | hard | 100% / 483,034 |
| gin-json-charset | easy | 100% / 109,792 |
| gin-is-aborted | easy | 100% / 84,010 |
| flask-prefixed-env | medium | 100% / 94,683 |
| flask-blueprint-url-prefix | medium | 100% / 137,438 |
| flask-response-tuple | hard | 100% / 144,914 |
| flask-json-decimal | easy | 100% / 144,224 |
