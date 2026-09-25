## What and why

<!-- What does this change, and what problem does it solve? Link the issue if there is one. -->

## Checklist

- [ ] `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test --all-targets` pass
- [ ] Outline changes: snapshots regenerated with `UPDATE_SNAPSHOTS=1 cargo test --test outlines`, and the diff reviewed
- [ ] README and `skills/speedread/SKILL.md` updated if flags, targets or output formats changed
- [ ] Model-facing text (`src/server.rs` instructions and tool descriptions, the `SKILL.md` description) is unchanged, or this PR explains the change and includes or requests an eval ([why](https://github.com/brennengreen/speedread/blob/main/CONTRIBUTING.md#model-facing-text-is-product-behavior))
- [ ] Any number added to the docs links to committed eval data
