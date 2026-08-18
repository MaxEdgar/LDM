# Contributing to LDM

Thanks for considering a contribution. Please keep the project's principles in
mind: correctness and security before speed, no fake functionality, and small
focused changes.

## Getting started

```bash
git clone https://github.com/MaxEdgar/LDM.git
cd LDM
cargo build
cargo test
```

## Workflow

1. Open an issue describing the change (bug, feature, or improvement).
2. Create a branch: `git checkout -b fix/description`.
3. Make the change with tests where practical.
4. Run the checks below.
5. Open a pull request referencing the issue.

## Checks

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test                    # unit + integration (local test server only)
cargo test -p ldm-native-host # browser host end-to-end
```

## Code style

- Rust: follow `rustfmt` and `clippy`; keep functions small and modules
  focused. No giant files, no global mutable state.
- The download engine (`ldm-engine`) must stay independent of the UI. New
  features belong in the engine with a UI layer on top.
- Never log credentials: passwords, cookies, authorization headers, tokens, or
  private file contents.
- Never execute downloaded content. Opening a file uses the desktop's normal
  open mechanism.

## Security

LDM takes security seriously (path traversal, credential leaks, local IPC
abuse, malicious filenames). If your change touches file paths, HTTP handling,
or the browser IPC surface, review the threat-model notes in the module docs
and add a test for the failure case. Report vulnerabilities privately via a
GitHub security advisory rather than a public issue.

## Testing notes

- All integration tests use the local test server (`ldm-test-server`); never
  rely on the public internet.
- Hash-integrity matters: multi-connection, resume and crash-recovery tests
  verify the final SHA-256 of the downloaded file.
