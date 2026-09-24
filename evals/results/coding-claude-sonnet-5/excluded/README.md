# Excluded trials

`flask-blueprint-prefix` failed task verification after these trials ran: with the
injected bug (`rule = self.url_prefix + rule`), flask's test suite still passes,
because werkzeug merges repeated slashes (`/bar//foo` matches `/bar/foo`). The
bug report's symptom was therefore false and the grader could not tell a fix
from no fix. Per "fix broken tasks", the task was replaced by
`flask-blueprint-url-prefix` (verified with `coding_eval.py --verify`) and these
two baseline trials are excluded from all results. They are kept for audit.
