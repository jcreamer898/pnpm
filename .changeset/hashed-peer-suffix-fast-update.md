---
"pacquet": patch
"pnpm": patch
---

`pnpm install` now updates the lockfile in place after a dependency is removed, even when the lockfile holds peer suffixes that pnpm shortened into a hash. It still resolves the whole dependency graph again when a removed package shares its name with a declared peer dependency, because a shortened suffix cannot be read.

Such an install used to resolve the whole dependency graph again.
