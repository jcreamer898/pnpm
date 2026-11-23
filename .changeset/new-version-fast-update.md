---
"pacquet": patch
"pnpm": patch
---

`pnpm install` now updates the lockfile in place after a dependency is bumped or added at a registry version the lockfile has never held. This applies when the version has no peer dependencies and the lockfile already holds every dependency it declares. The install still resolves the whole dependency graph again when the lockfile records publish times, when a trust policy is set, or when the project lists the dependency as optional.

Such an install used to resolve the whole dependency graph again.
