# Local Git guards

Install the committed hooks for this clone:

```sh
./script/install-hooks
```

The installer checks prerequisites and creates symlinks in `.git/hooks`. It
does not change Git configuration and refuses to overwrite existing hooks.

## Pre-commit

The pre-commit hook keeps the frequent path fast and offline:

- Gitleaks scans the staged diff for credentials, API keys, private keys, and
  other secrets. Values are redacted from output.
- Ruff checks only staged Python files with the `F` and `E7` rule families.
- ShellCheck checks only staged shell scripts at error severity.

These checks block the commit. Missing required tools also block with setup
instructions.

## Pre-push

The pre-push hook runs the broader, network-dependent checks:

- Semgrep scans Rust, Python, and JavaScript with curated registry packs.
- OSV-Scanner checks dependency lockfiles against the OSV vulnerability
  database.

The installer also requires GNU `timeout` (`gtimeout` from coreutils on macOS)
so Semgrep and OSV cannot hang a push indefinitely.

Reports are written under `.git/security-reports`. The repository currently
has pre-existing Semgrep and OSV findings, so pushes run in advisory mode:
findings are reported but do not block. Scanner execution failures still
block. After those baselines are triaged, enforce findings with:

```sh
ENFORCE_SECURITY_SCAN=1 git push
```

For an exceptional push, bypass only this scan with:

```sh
SKIP_SECURITY_SCAN=1 git push
```

Run the heavy scan directly in enforcing mode with:

```sh
./script/security-scan
```

## JavaScript and TypeScript

This hook does not run ESLint or Biome. Zed has a small set of JavaScript and
TypeScript files spread across unrelated tooling and documentation, but no
root Node project or shared lint configuration. Running `npx eslint .` would
download tooling at commit time and apply undefined rules to vendored and
generated files. A future JS/TS gate should first define the intended source
roots and committed configuration.
