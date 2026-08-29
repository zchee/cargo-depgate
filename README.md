# cargo-depgate

A high-performance dependency policy enforcer and CI gatekeeper for Cargo workspaces.

`cargo-depgate` acts as an automated quality gate in your CI/CD pipelines. It ensures that dependency graphs adhere to strict organizational policies before code reaches production.

### Key Capabilities

* **Workspace Boundary Enforcement**: Prevent target-specific or internal crates from leaking across crate boundaries.
* **Transitive Dependency Auditing**: Block banned crates, outdated semver ranges, or unvetted third-party additions.
* **Deterministic Fail-Fast CI**: Emits structured diagnostics with precise exit codes tailored for GitHub Actions and automated workflows.
* **Zero Compilation Overhead**: Evaluates `Cargo.lock` and metadata directly without compiling source code.

<!-- depgate:semantics -->

## Policy semantics

Policy semantics will be documented in a later implementation phase.

<!-- depgate:gap-table -->

## Cargo feature gap table

The supported Cargo feature matrix will be documented in a later implementation phase.

<!-- depgate:exit-codes -->

## Exit codes

Exit codes are 0 for success, 1 for policy violations, 2 for configuration or usage errors, and 3 for `cargo metadata` failures.

<!-- depgate:ci -->

## CI integration

CI integration guidance will be documented in a later implementation phase.

<!-- depgate:version-blind -->

## Version-blind policies

Version-blind policy behavior will be documented in a later implementation phase.

<!-- depgate:codeowners -->

## CODEOWNERS integration

CODEOWNERS integration guidance will be documented in a later implementation phase.
