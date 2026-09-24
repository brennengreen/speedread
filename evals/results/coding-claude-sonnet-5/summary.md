# Coding eval — claude-sonnet-5 via GitHub Copilot CLI 1.0.89-1

8 SWE-style bug-fix tasks × 2 trials per condition, run 2026-09-24. Pass = the repository's full test suite passes with tests unmodified.

| Metric | baseline | available | preferred | exclusive |
|---|---:|---:|---:|---:|
| Pass rate (pass@1) | 100% | 100% | 100% | 100% |
| Mean input tokens / task | 166,147 | 243,091 (+46%) | 160,357 (−3%) | 165,307 (−1%) |
| Median input tokens / task | 124,460 | 174,428 (+40%) | 118,361 (−5%) | 102,896 (−17%) |
| Claude Code-adjusted input tokens (upper bound) | 166,147 | 253,043 (+52%) | 182,872 (+10%) | 196,947 (+19%) |
| Mean output tokens / task | 1,714 | 2,424 (+41%) | 1,685 (−2%) | 1,636 (−5%) |
| Mean cost / task (AI units) | 7.84 | 10.15 (+30%) | 7.50 (−4%) | 7.37 (−6%) |
| Median model (API) time, s | 20.5 | 21.5 (+5%) | 15.6 (−24%) | 14.9 (−27%) |
| Median session time, s | 33.8 | 28.2 (−17%) | 24.1 (−29%) | 22.7 (−33%) |
| Mean model calls (turns) | 7.8 | 9.4 (+21%) | 6.5 (−16%) | 7.2 (−6%) |
| Mean tool calls | 6.9 | 8.4 (+22%) | 5.5 (−20%) | 6.2 (−9%) |
| Mean read/search calls | 4.5 | 5.2 (+17%) | 3.1 (−32%) | 3.8 (−17%) |
| Read-result tokens / task | 1,604 | 1,671 (+4%) | 1,906 (+19%) | 1,622 (+1%) |
| Source bytes shown / task | 5,150 | 5,486 (+7%) | 5,475 (+6%) | 4,619 (−10%) |
| Repeated source bytes / task | 594 | 1,034 (+74%) | 722 (+21%) | 551 (−7%) |
| Used speedread (adoption) | 0% | 0% | 100% | 100% |
| Share of reads via speedread | 0% | 0% | 78% | 83% |
| Mean trace calls | 0.0 | 0.0 | 0.0 | 0.0 |
| Decisive line shown | 100% | 100% | 100% | 100% |

Consistency: baseline pass@2 100%, pass^2 100%; available pass@2 100%, pass^2 100%; preferred pass@2 100%, pass^2 100%; exclusive pass@2 100%, pass^2 100%

Compression safety (speedread conditions): available: decisive line first hidden in a skeleton in 0 trials (never expanded 0, failed 0); preferred: decisive line first hidden in a skeleton in 0 trials (never expanded 0, failed 0); exclusive: decisive line first hidden in a skeleton in 0 trials (never expanded 0, failed 0)

## Per task (means)

| Task | Difficulty | baseline pass / tokens | available pass / tokens | preferred pass / tokens | exclusive pass / tokens |
|---|---|---:|---:|---:|---:|
| gin-client-ip | medium | 100% / 131,080 | 100% / 187,070 | 100% / 172,629 | 100% / 155,604 |
| gin-catchall-param | hard | 100% / 483,034 | 100% / 775,370 | 100% / 456,459 | 100% / 559,913 |
| gin-json-charset | easy | 100% / 109,792 | 100% / 109,920 | 100% / 85,220 | 100% / 80,322 |
| gin-is-aborted | easy | 100% / 84,010 | 100% / 115,026 | 100% / 84,824 | 100% / 97,879 |
| flask-prefixed-env | medium | 100% / 94,683 | 100% / 139,234 | 100% / 89,940 | 100% / 81,912 |
| flask-blueprint-url-prefix | medium | 100% / 137,438 | 100% / 270,054 | 100% / 142,219 | 100% / 119,186 |
| flask-response-tuple | hard | 100% / 144,914 | 100% / 192,646 | 100% / 107,999 | 100% / 99,278 |
| flask-json-decimal | easy | 100% / 144,224 | 100% / 155,409 | 100% / 143,562 | 100% / 128,364 |
