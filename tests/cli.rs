//! Binary-level checks for the [003] CLI surface.
//!
//! These exercise the *compiled* `rust-docs-mcp` binary via
//! [`CARGO_BIN_EXE_rust-docs-mcp`]. They pin the product boundary from
//! `plan.md` §8/§9 that a subcommand keeps **stdout exclusive to MCP protocol
//! traffic** — every diagnostic (including a successful human report) goes to
//! **stderr** — and that invalid arguments produce **actionable, non-zero**
//! errors rather than silently succeeding or polluting stdout.
//!
//! As of task `[031]` the `status` subcommand is wired: with no local snapshot
//! it loads the snapshot **embedded in the binary** and reports it on stderr,
//! exiting 0 — so a fresh install works offline (`plan.md` §5 / `docs/LICENSING.md`
//! §5). `serve`/`sync` remain not offline-functional (a `serve` run needs a real
//! stdio client; a `sync` run needs the network), so their non-zero exit in this
//! harness is incidental. The stdout cleanliness every subcommand must preserve
//! is what these tests pin.

use std::path::PathBuf;
use std::process::Command;

/// Path to the compiled `rust-docs-mcp` binary for this test target.
fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_rust-docs-mcp"))
}

/// A throwaway data-directory path that is resolved but never written by the
/// [003] stubs (directory creation is a later task), so the run reaches the
/// subcommand dispatch regardless of the host environment.
fn temp_data_dir(tag: &str) -> String {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let name = format!("rust-docs-mcp-{tag}-{stamp}");
    std::env::temp_dir()
        .join(name)
        .to_string_lossy()
        .into_owned()
}

fn run(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(binary())
        .args(args)
        .output()
        .expect("spawn the rust-docs-mcp binary");
    let code = out.status.code().unwrap_or(-1);
    (
        code,
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn help_documenting_subcommands_and_data_dir() {
    let (code, stdout, _stderr) = run(&["--help"]);
    assert_eq!(code, 0, "--help should exit 0");
    for token in ["serve", "sync", "status", "--data-dir"] {
        assert!(
            stdout.contains(token),
            "help output should mention `{token}`:\n{stdout}"
        );
    }
}

#[test]
fn version_is_printed() {
    let (code, stdout, _stderr) = run(&["--version"]);
    assert_eq!(code, 0, "--version should exit 0");
    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "version output should carry the crate version:\n{stdout}"
    );
}

#[test]
fn no_subcommand_is_an_actionable_error_on_stderr() {
    let (code, stdout, stderr) = run(&[]);
    assert_ne!(code, 0, "missing subcommand should be non-zero");
    assert!(
        stdout.is_empty(),
        "no subcommand must not pollute stdout:\n{stdout}"
    );
    assert!(
        !stderr.trim().is_empty(),
        "the error must name the problem on stderr:\n{stderr}"
    );
}

#[test]
fn unknown_subcommand_and_malformed_args_are_actionable_errors() {
    for args in [
        &["frobnicate"][..],
        &["sync", "--data-dir"][..],
        &["--bogus"][..],
    ] {
        let (code, stdout, stderr) = run(args);
        assert!(code != 0, "malformed args {args:?} should exit non-zero");
        assert!(
            stdout.is_empty(),
            "malformed args {args:?} must not pollute stdout:\n{stdout}"
        );
        assert!(
            !stderr.trim().is_empty(),
            "malformed args {args:?} should explain the error on stderr:\n{stderr}"
        );
    }
}

/// The core invariant: a subcommand run keeps **stdout** exclusive to MCP
/// protocol traffic — every diagnostic (including a successful human report) is
/// routed to **stderr**. This must hold for every subcommand, wired or not, so
/// it is asserted across `serve`/`sync`/`status` regardless of their exit codes.
#[test]
fn subcommand_runs_keep_stdout_clean() {
    let dir = temp_data_dir("clean-stdout");
    for sub in ["serve", "sync", "status"] {
        let args = [sub, "--data-dir", dir.as_str()];
        let (_code, stdout, stderr) = run(&args);
        assert!(
            stdout.is_empty(),
            "subcommand `{sub}` must keep stdout exclusive to MCP traffic, not {stdout:?}"
        );
        assert!(
            !stderr.trim().is_empty(),
            "subcommand `{sub}` should route its diagnostics/report to stderr:\n{stderr}"
        );
    }
}

/// Task `[031]` acceptance criterion 1, driven **through the compiled binary**:
/// with **no** local snapshot, `status` loads the snapshot **embedded in the
/// binary** and reports it on **stderr**, exiting 0 — a fresh install works
/// offline, with no network and no `sync`. The human report must never reach
/// stdout (the MCP-exclusive channel), so stdout stays empty even on success.
#[test]
fn status_serves_the_embedded_bundle_offline() {
    let dir = temp_data_dir("status-embedded-offline");
    let (code, stdout, stderr) = run(&["status", "--data-dir", dir.as_str()]);
    assert_eq!(
        code, 0,
        "status must succeed offline by loading the embedded bundle:\n{stderr}"
    );
    assert!(
        stdout.is_empty(),
        "status must keep stdout exclusive to MCP traffic even on success, not {stdout:?}"
    );
    assert!(
        stderr.contains("rust-docs-bundled-seed"),
        "status must report the embedded seed snapshot on stderr:\n{stderr}"
    );
}

/// Task [004] acceptance #2: diagnostics are routed to stderr even when the
/// caller raises verbosity, so `--verbose` can never push a diagnostic onto
/// stdout (the MCP-exclusive channel). The flag is global, so it is accepted
/// before the subcommand.
#[test]
fn verbose_stub_keeps_stdout_clean() {
    let dir = temp_data_dir("verbose-clean-stdout");
    let args = ["--verbose", "serve", "--data-dir", dir.as_str()];
    let (code, stdout, stderr) = run(&args);
    assert_ne!(
        code, 0,
        "--verbose stub should still exit non-zero until wired (M2/M4)"
    );
    assert!(
        stdout.is_empty(),
        "--verbose must not move diagnostics to stdout: {stdout:?}"
    );
    assert!(
        !stderr.trim().is_empty(),
        "diagnostics must still appear on stderr under --verbose:\n{stderr}"
    );
}

/// `--verbose` is advertised by `--help` as a global diagnostics flag.
#[test]
fn help_documents_verbose() {
    let (code, stdout, _stderr) = run(&["--help"]);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("--verbose"),
        "help should document the `--verbose` diagnostics flag:\n{stdout}"
    );
}

/// Task [004] acceptance #1, driven **through the compiled binary**: a startup
/// failure with no available data directory must surface as an actionable, typed
/// stderr message plus a non-zero exit — never an unwind and never a stdout
/// write. This pins the `main` wiring (`resolve_data_dir()?` → `AppError` →
/// `tracing::error!` + `ExitCode::FAILURE`) that the unit tests exercise only at
/// the type boundary. The subcommands are still placeholders, so the only startup
/// failure reachable here is an unresolvable data directory.
#[test]
fn startup_data_dir_failure_surfaces_actionable_stderr() {
    // Strip every source of an OS default so the process resolves no data dir,
    // regardless of the host. `env_remove` affects only the *child* process,
    // leaving the test harness environment untouched. On Unix the default reads
    // `XDG_DATA_HOME` then `HOME`; on Windows it reads `APPDATA` — all removed,
    // so `default_data_dir()` returns `None` and `resolve_data_dir` reports
    // `DataDirError::NoDefault`.
    let out = Command::new(binary())
        .arg("serve")
        .env_remove("HOME")
        .env_remove("XDG_DATA_HOME")
        .env_remove("APPDATA")
        .output()
        .expect("spawn the rust-docs-mcp binary");
    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_ne!(code, 0, "an unresolvable data directory must exit non-zero");
    assert!(
        stdout.is_empty(),
        "a startup failure must not write to stdout (MCP-exclusive): {stdout:?}"
    );
    // The typed, context-carrying message must reach stderr: the `--data-dir`
    // remedy (the `where`-fix) and the `data directory` where-clause. Neither a
    // bare abort nor a panic can produce this string.
    assert!(
        stderr.contains("--data-dir"),
        "stderr must name the `--data-dir` remedy from the typed error:\n{stderr}"
    );
    assert!(
        stderr.to_lowercase().contains("data directory"),
        "stderr must carry the failure's where-context:\n{stderr}"
    );
}
