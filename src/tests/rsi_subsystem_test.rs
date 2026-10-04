//! Tests for `rsi_subsystem::classify_bash_command`. The classifier
//! turns raw bash command text from the feedback ledger into a
//! stable subsystem slug so RSI can group "all the gh things" or
//! "all the docker things" together and notice patterns the
//! per-tool aggregation can't see.
//!
//! Lossy by design: well-known CLIs get a slug, everything else
//! returns None so RSI doesn't crowd disparate one-offs into an
//! "other" bucket and propose nonsense.

use crate::brain::rsi_subsystem::classify_bash_command;

#[test]
fn empty_command_returns_none() {
    assert_eq!(classify_bash_command(""), None);
    assert_eq!(classify_bash_command("   "), None);
}

#[test]
fn unknown_command_returns_none() {
    // The point of None: don't bucket one-offs into a misleading
    // "other" pile. RSI should ignore commands it can't classify
    // rather than propose tools for `weird_user_script.sh`.
    assert_eq!(classify_bash_command("./my-custom-script.sh"), None);
    assert_eq!(classify_bash_command("totally-unknown-cli arg"), None);
}

// ── Version control ────────────────────────────────────────────

#[test]
fn classifies_git_commands() {
    assert_eq!(classify_bash_command("git status"), Some("git"));
    assert_eq!(classify_bash_command("git log --oneline -10"), Some("git"));
    assert_eq!(classify_bash_command("git push origin main"), Some("git"));
}

#[test]
fn classifies_gh_commands_separately_from_git() {
    // `gh` is GitHub-specific CLI, distinct from `git` (vendor-
    // neutral). RSI cares about the difference because high
    // gh-call volume suggests a github-* tool extraction; high
    // git-call volume suggests a local-workflow skill.
    assert_eq!(
        classify_bash_command("gh issue list --repo foo/bar"),
        Some("gh")
    );
    assert_eq!(
        classify_bash_command("gh pr comment 123 --body x"),
        Some("gh")
    );
}

#[test]
fn classifies_legacy_hub_as_gh() {
    // `hub` was the predecessor of `gh`; same domain so RSI
    // aggregates them together when computing GitHub patterns.
    assert_eq!(classify_bash_command("hub pull-request"), Some("gh"));
}

// ── Containers / orchestration ─────────────────────────────────

#[test]
fn classifies_docker_variants() {
    assert_eq!(classify_bash_command("docker ps"), Some("docker"));
    assert_eq!(
        classify_bash_command("docker-compose up -d"),
        Some("docker")
    );
}

#[test]
fn classifies_kubectl() {
    assert_eq!(classify_bash_command("kubectl get pods"), Some("kubectl"));
    assert_eq!(classify_bash_command("k9s"), Some("kubectl"));
}

#[test]
fn classifies_terraform_and_tofu_together() {
    // OpenTofu is the OSS terraform fork; aggregating together
    // gives RSI signal for IaC patterns regardless of which CLI
    // the user has installed.
    assert_eq!(classify_bash_command("terraform plan"), Some("terraform"));
    assert_eq!(classify_bash_command("tofu apply"), Some("terraform"));
}

// ── Language / build toolchains ────────────────────────────────

#[test]
fn classifies_cargo_family() {
    assert_eq!(
        classify_bash_command("cargo build --release"),
        Some("cargo")
    );
    assert_eq!(classify_bash_command("rustup update"), Some("cargo"));
}

#[test]
fn classifies_python_variants() {
    assert_eq!(classify_bash_command("python3 script.py"), Some("python"));
    assert_eq!(classify_bash_command("py -V"), Some("python"));
}

#[test]
fn classifies_pip_family() {
    // Modern Python packaging is fragmented (pip, pipx, uv, poetry).
    // RSI aggregates them so "package management" shows up as one
    // signal regardless of which manager the user prefers.
    assert_eq!(classify_bash_command("pip install requests"), Some("pip"));
    assert_eq!(classify_bash_command("uv pip install foo"), Some("pip"));
    assert_eq!(classify_bash_command("poetry add foo"), Some("pip"));
}

#[test]
fn classifies_node_and_npm_separately() {
    // `node` is runtime; `npm` is package management. Different
    // concerns, different aggregation buckets.
    assert_eq!(classify_bash_command("node server.js"), Some("node"));
    assert_eq!(classify_bash_command("npm install"), Some("npm"));
    assert_eq!(classify_bash_command("pnpm add foo"), Some("npm"));
    assert_eq!(classify_bash_command("yarn build"), Some("npm"));
}

// ── Networking / data fetch ────────────────────────────────────

#[test]
fn classifies_http_clients_together() {
    assert_eq!(
        classify_bash_command("curl https://api.example.com"),
        Some("curl")
    );
    assert_eq!(
        classify_bash_command("wget -O - https://x.com"),
        Some("curl")
    );
}

#[test]
fn classifies_ssh_family() {
    assert_eq!(
        classify_bash_command("ssh user@host 'uname -a'"),
        Some("ssh")
    );
    assert_eq!(
        classify_bash_command("rsync -avz src/ remote:dst/"),
        Some("ssh")
    );
}

// ── Cloud ──────────────────────────────────────────────────────

#[test]
fn classifies_cloud_clis_distinctly() {
    assert_eq!(classify_bash_command("aws s3 ls"), Some("aws"));
    assert_eq!(
        classify_bash_command("gcloud compute instances list"),
        Some("gcloud")
    );
    assert_eq!(classify_bash_command("az login"), Some("az"));
    assert_eq!(classify_bash_command("flyctl deploy"), Some("fly"));
    assert_eq!(classify_bash_command("wrangler deploy"), Some("wrangler"));
}

// ── Shell metalevel ────────────────────────────────────────────

#[test]
fn classifies_sudo_and_time_as_shell() {
    // `sudo` is a deliberate user action (elevated privileges);
    // RSI sees it as a meta-signal worth tracking on its own
    // rather than peeling it off to see what's underneath.
    assert_eq!(classify_bash_command("sudo apt update"), Some("shell"));
    assert_eq!(classify_bash_command("time cargo build"), Some("shell"));
}

#[test]
fn strips_leading_env_var_assignments() {
    // `RUSTFLAGS="..." cargo build` should classify as cargo, not
    // shell. Env assignments are configuration, not the actual
    // command.
    assert_eq!(
        classify_bash_command("RUSTFLAGS=\"-C opt-level=0\" cargo build"),
        Some("cargo")
    );
    assert_eq!(
        classify_bash_command("AWS_REGION=us-east-1 aws s3 ls"),
        Some("aws")
    );
    assert_eq!(
        classify_bash_command("FOO=bar BAZ=qux gh pr list"),
        Some("gh")
    );
}

#[test]
fn does_not_strip_dashed_option_with_equals() {
    // `--features=foo` looks like KEY=VALUE but the leading dash
    // makes it an option, not an env assignment. The classifier
    // must not skip past it looking for the "real" command.
    // `cargo --features=foo build` still classifies as cargo.
    assert_eq!(
        classify_bash_command("cargo --features=foo build"),
        Some("cargo")
    );
}

#[test]
fn case_insensitive_match() {
    // Some users (notably on Windows, or copying from docs) emit
    // uppercase command names. Aggregation must not split GIT vs
    // git into two buckets.
    assert_eq!(classify_bash_command("GIT status"), Some("git"));
    assert_eq!(classify_bash_command("GH pr list"), Some("gh"));
    assert_eq!(classify_bash_command("Docker ps"), Some("docker"));
}

// ── Realistic noisy inputs ─────────────────────────────────────

#[test]
fn classifies_long_realistic_gh_command() {
    let cmd = "gh pr comment 130 --repo opencrabs/opencrabs --body \"$(cat <<'EOF'\nLooks good!\nEOF\n)\"";
    assert_eq!(classify_bash_command(cmd), Some("gh"));
}

#[test]
fn classifies_chained_command_by_first_part() {
    // Pipe / && chains: we classify by the first command. RSI is
    // looking for "this leading invocation keeps coming up", not
    // for the full pipeline. If users want pipeline-level grouping
    // they can extend the classifier later.
    assert_eq!(
        classify_bash_command("git log --oneline | head -10"),
        Some("git")
    );
    assert_eq!(
        classify_bash_command("docker ps && docker logs $(docker ps -q | head -1)"),
        Some("docker")
    );
}

#[test]
fn realistic_python_inline_script() {
    // Common pattern: `python3 -c "..."`. Still python.
    assert_eq!(
        classify_bash_command("python3 -c \"import json; print(json.dumps({}))\""),
        Some("python")
    );
}

#[test]
fn realistic_cargo_with_features_and_env() {
    let cmd =
        "CARGO_TERM_COLOR=always cargo nextest run --cargo-profile ci --all-features --retries 2";
    assert_eq!(classify_bash_command(cmd), Some("cargo"));
}
