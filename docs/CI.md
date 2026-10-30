# Pull request validation

[Rust validation](../.github/workflows/validation.yml) runs on every opened,
reopened, or updated pull request, including drafts and documentation changes.
It also runs on pushes to `main`, merge queue groups, and manual dispatch.
New runs cancel older runs for the same pull request or ref.

## Required checks

Both Rust jobs run on `ubuntu-24.04`, using the committed `Cargo.lock` and the
package's default feature configuration. There are currently no package feature
flags. Each job records the active toolchain, compiler/Cargo versions, and
resolved dependency features in its log.
`RUST_LOG` is cleared so default-logging tests exercise the application's default
level rather than inheriting a runner's logging override.

| Job | Validation |
| --- | --- |
| `Rust stable` | Installs the stable toolchain with rustfmt and Clippy; runs formatting, all-target compilation, Clippy with warnings denied, workspace tests, and doctests. |
| `Rust MSRV` | Reads `package.rust-version` from `Cargo.toml` (currently `1.88`); installs that toolchain and runs all-target compilation, workspace tests, and doctests. |
| `PR validation` | Runs after both jobs, even if either failed or was skipped. Succeeds only when both report `success`; failure, cancellation, skipped, or missing results fail the gate. |

Cargo commands use `--locked` wherever dependency resolution applies. Dependency
and toolchain setup may download packages; test steps set `CARGO_NET_OFFLINE=true`
after the compilation steps have populated Cargo's cache. Tests use local
fixtures and temporary directories. This Cargo setting disables dependency
downloads, not arbitrary networking by test code. Live upstream ingestion remains
separate integration work ([#62](https://github.com/ThiagoEvoa/rust-mcp/issues/62)).

The workflow grants only `contents: read`, does not persist checkout credentials,
and needs no secrets or external AI service. It uses `pull_request`, so executing
PR code does not require the privileged `pull_request_target` event. Checkout is
pinned to a reviewed commit; review and update the pin when upgrading the action.

## Enforce validation before merging

Adding a workflow does not itself prevent merging. After publishing the workflow
and observing a run, configure **Settings → Rules → Rulesets → New branch ruleset**:

1. Target the default branch (`main`) and set enforcement to **Active**.
2. Enable **Require a pull request before merging**.
3. Enable **Require status checks to pass** and add the exact check name
   **`PR validation`**, selecting GitHub Actions as its source.
4. Enable **Require branches to be up to date before merging**. The workflow
   also supports `merge_group` if a merge queue is adopted later.
5. Leave the bypass list empty if all contributors must satisfy validation.
   Configure human review requirements separately if desired.

Do not rename the aggregate check without updating the ruleset. Keep the gate
unconditional and avoid workflow path filters: a conditionally skipped job can
otherwise appear successful, while a filtered-out required workflow can remain
pending indefinitely. A manually dispatched run is useful for diagnostics; it
does not replace validation triggered by the pull request.

Check names and enforcement behavior are documented in GitHub's
[ruleset rules](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/available-rules-for-rulesets),
[ruleset setup](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/creating-rulesets-for-a-repository),
and [required-check troubleshooting](https://docs.github.com/en/pull-requests/how-tos/merge-and-close-pull-requests/troubleshooting-required-status-checks).

## Scope and remaining acceptance work

This implements the workflow portion of
[#61](https://github.com/ThiagoEvoa/rust-mcp/issues/61) early at the user's request.
It runs the tests available in the checked-out revision, including the current
CLI and spawned stdio lifecycle checks. It does not establish acceptance for the
five completed tools/resources: that integration suite is still tracked in
[#57](https://github.com/ThiagoEvoa/rust-mcp/issues/57), a prerequisite for full
acceptance of #61. The M5 CI deliverable remains open until its dependencies,
GitHub-hosted checks, and required review/integration have evidence.

The review skill's Standards and Spec assessments still require code review and
requirements-based tests. This workflow supplies executable validation evidence;
it does not run an agent reviewer or issue a PR readiness score.
