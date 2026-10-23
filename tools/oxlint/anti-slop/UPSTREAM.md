# Vendored anti-slop

## Source and merge baseline

- Repository: https://github.com/dmmulroy/anti-slop
- Incoming revision: `c44ef22ca116d0ba62a3ff663a0bd13a3f3fa40b`.
- Production source: `skills/install-anti-slop/assets/anti-slop/` at that revision,
  verified byte-for-byte against the refreshed local skill bundle before merging.
- Installed paths: `tools/oxlint/anti-slop/index.ts` and the optional
  `tools/oxlint/anti-slop/effect/index.ts`.
- Recoverable previous base: Redoor commit
  `e2990dbd187bcd67e595985423ffd2576a7589de`, path
  `.agents/skills/install-anti-slop/assets/anti-slop/`, Git tree
  `d7eb6a2fb937a95fd78958bb9c8c34b654c2824b`. The active installation matched
  this retained pristine bundle exactly; there were no local source changes.
- All incoming production files were reconciled and adopted. The new production
  baseline is the incoming revision above, with no source deviations.
- Pre-update files, including the user's refreshed skill and lockfile, are backed
  up at `/tmp/redoor-anti-slop-update.KWPC1z/backup/`.

The update adopts lexical alias resolution, generic type binding fixes, broader
known-value checks, type-predicate input support, `typeof` existence probes,
borrowed member-name exceptions, clearer diagnostics, and eight new generic
rules. It also includes the optional Effect plugin and the vendored Stylistic
spacing implementation with its license and provenance.

## Local integration and policy

- Existing rule severities, options, overrides, ignores, and plugin paths remain
  unchanged in `.oxlintrc.json`. The user approved enabling all eight new generic
  rules plus the native accumulating-spread companion at error severity.
- No dependency changes are needed: installed `oxlint` and `@oxlint/plugins` are
  both `1.78.0`, matching upstream's requirements.
- Effect rules remain unregistered because Redoor has no direct Effect dependency.
- The 23 upstream RuleTester files from `src/` are retained beside their rules,
  renamed from `*.test.ts` to `*.rule-test.ts` so Vitest does not collect them as
  application tests. Their contents are unchanged.
- `test.mjs` runs those cases plus `cli.rule-test.ts`, a local adaptation of
  upstream's spacing CLI regression using async filesystem APIs, zx, and mise.
  `pn test:anti-slop` is included in the full `pn test` workflow.

## New rule activation and migration

All settings below are enabled following explicit user approval. The initial
trial counts are retained to document the migration; application lint now passes.
Oxlint's `--fix` applied all 3,717 spacing fixes, followed by the repository's
formatter. A second fix/format pass left the working tree unchanged.

| Rule | Severity | Initial application findings |
| --- | --- | ---: |
| `anti-slop/no-array-filter-map` | error | 6 |
| `anti-slop/no-reduce-accumulator-copy` | error | 0 |
| `oxc/no-accumulating-spread` (native companion) | error | 0 |
| `anti-slop/no-module-mocking` | error | 2 |
| `anti-slop/no-reflect-apply` | error | 0 |
| `anti-slop/no-reflect-get` | error | 0 |
| `anti-slop/no-unknown-returns` | error | 4 |
| `anti-slop/require-readable-spacing` | error | 3717 |
| `anti-slop/require-safety-comment-for-type-assertion` | error | 0 |

The 12 findings without autofixes were resolved with single-pass `flatMap`
transformations in transfer summaries and Git grouping, generated API response
types for upgrade/restart callbacks, a void-returning readiness probe, and real
Query/Router providers with call-through spies in refresh tests. Git grouping
retains conflict precedence, staged/worktree membership, and deduplicated diff
order. No source merges or activation decisions remain pending.

At the user's request, the skill refresh and mechanical changes are committed
separately: `7013333` (skill), `a54ad11` (source spacing), and `4a83d1a` (test
spacing). Both spacing commits contain only blank-line insertions. The active
plugin update, policy activation, and manual migration remain separate changes.

## Verification

- `pn test:anti-slop`: passed all 24 test files, including multi-file CLI loading,
  rejected input, documentation-preserving spacing fixes, and repeated-fix stability.
- `pn lint` and `pn types`: passed with all new generic rules enabled.
- Focused refresh/query tests: all 6 tests passed with real providers. The dirty
  editor assertion specifically excludes file-content refetches while allowing
  the real QueryClient's Git refetch triggered by invalidation.
- Route/UI build: passed.
- Explicit strict TypeScript check of both plugin entry points: passed using
  TypeScript's NodeNext module resolution and ES2024 target.
- Initial full `pn test`: passed in 445 seconds. The final full run covering
  rule activation and migration passed in 443 seconds, including integration
  and Playwright suites. The Android check was skipped because `cargo-ndk`
  is not installed.
