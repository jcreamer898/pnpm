---
"pacquet": patch
"pnpm": patch
---

`pnpm install` now updates the lockfile in place after a dependency is removed or moved to another dependency group, in a project where `overrides` rewrites a direct dependency.

Such an install used to resolve the whole dependency graph again.
