# Flaky tests
- 2026-10-22 `pn test` integration (`tests/terminal.test.ts`): `dedicated terminal tunnel > reports the shell's exit status after its last output` failed at line 334 because the lifecycle events were empty (`expected [] to deep equally contain { type: "exit", code: 3, signal: null }`). All 5 tests in the file passed on immediate rerun.
