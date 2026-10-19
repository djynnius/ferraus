# ADR 0005: The binary package is named `ferraus`

Status: accepted (2026-10-19)

The PRD's install URLs use `ferraus-installer.sh` / `ferraus-installer.ps1` and archives named
`ferraus-<target>`. cargo-dist names release artifacts after the Cargo package that owns the
binary, so the CLI package (directory `crates/ferraus-cli`) is published as `ferraus`; its
library target keeps the name `ferraus_cli`. Consequently the from-source install is
`cargo install ferraus` (the PRD's `cargo install ferraus-cli` is superseded). Library crates
keep their `ferraus-*` names.
