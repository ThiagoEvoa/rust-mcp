//! Binary-level checks for the [003] CLI surface.
//!
//! These exercise the *compiled* `rust-docs-mcp` binary via
//! [`CARGO_BIN_EXE_rust-docs-mcp`]. They pin the product boundary from
//! `plan.md` §8/§9 that the `serve` command keeps **stdout exclusive to MCP
//! protocol traffic** — every diagnostic in the current stubs goes to **stderr** —
//! and that invalid arguments produce **actionable, non-zero** errors rather than
//! silently succeeding or polluting stdout.
//!
//! The stub subcommands are intentionally not wired yet (their real behaviour
//! lands in M2/M4), so a run of `serve`/`sync`/`status` returns a non-zero exit
//! with an explanatory stderr message. These tests assert that contract and the
//! stdout cleanliness that later milestones must preserve.

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

/// The core invariant: a subcommand run that is not yet wired must not write to
/// stdout (kept exclusive for MCP traffic) and must exit non-zero with a
/// diagnostic on stderr.
#[test]
fn subcommand_runs_keep_stdout_clean() {
    let dir = temp_data_dir("clean-stdout");
    for sub in ["serve", "sync", "status"] {
        let args = [sub, "--data-dir", dir.as_str()];
        let (code, stdout, stderr) = run(&args);
        assert_ne!(
            code, 0,
            "stub subcommand `{sub}` should exit non-zero until wired (M2/M4)"
        );
        assert!(
            stdout.is_empty(),
            "subcommand `{sub}` must keep stdout exclusive to MCP traffic, not {stdout:?}"
        );
        assert!(
            !stderr.trim().is_empty(),
            "subcommand `{sub}` should route its diagnostic to stderr:\n{stderr}"
        );
    }
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
