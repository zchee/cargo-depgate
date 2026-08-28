# cargo-depgate

A high-performance dependency policy enforcer and CI gatekeeper for Cargo workspaces.

`cargo-depgate` acts as an automated quality gate in your CI/CD pipelines. It ensures that dependency graphs adhere to strict organizational policies before code reaches production.

### Key Capabilities

* **Workspace Boundary Enforcement**: Prevent target-specific or internal crates from leaking across crate boundaries.
* **Transitive Dependency Auditing**: Block banned crates, outdated semver ranges, or unvetted third-party additions.
* **Deterministic Fail-Fast CI**: Emits structured diagnostics with precise exit codes tailored for GitHub Actions and automated workflows.
* **Zero Compilation Overhead**: Evaluates `Cargo.lock` and metadata directly without compiling source code.
