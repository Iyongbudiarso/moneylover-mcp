# Security policy

## Supported versions

Only the latest release receives security fixes. Please upgrade before reporting
an issue that may already be fixed.

## Reporting a vulnerability

**Do not open a public issue for security problems.**

Report privately through GitHub:
[Report a vulnerability](https://github.com/Iyongbudiarso/moneylover-mcp/security/advisories/new)
(repository → Security → Advisories → "Report a vulnerability").

Please include:

- the affected version (`moneylover-mcp --version`) and platform,
- steps to reproduce, or a proof of concept,
- the impact you expect (for example credential or token exposure, or a write
  made while `MONEYLOVER_ALLOW_WRITE` is off).

You can expect an acknowledgement within 7 days. This is a single-maintainer
project, so fixes are made on a best-effort basis; critical issues are
prioritised. Once a fix is released you will be credited in the advisory unless
you prefer otherwise.

## Scope

In scope: this server's handling of the Money Lover credentials and token it
stores, the opt-in write guard, and the release artifacts (binaries and
`.mcpb` bundles).

Out of scope: vulnerabilities in the Money Lover service itself. This is an
unofficial project and is not affiliated with Money Lover.

## Verifying a release

Release archives and bundles are signed with [cosign](https://github.com/sigstore/cosign)
(keyless, via the release workflow) and carry GitHub build provenance
attestations. See the notes of each release for the exact verification commands.
