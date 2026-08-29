# Security Policy

## Supported Versions

Security fixes are applied to the latest released version on `main`. Older versions are not backported.

## Reporting a Vulnerability

Please do not open a public GitHub issue for security vulnerabilities.

Instead, use one of the following private channels:

1. **GitHub Private Vulnerability Reporting** (preferred) — go to the **Security** tab on this repository and select **Report a vulnerability**.
2. **Email** — [shreyas@lyzr.ai](mailto:shreyas@lyzr.ai). A PGP key is available on request for encrypted reports.

Please include as much detail as possible:

- A description of the vulnerability and its potential impact
- Steps to reproduce, or a proof-of-concept if available
- Affected version/commit
- Any suggested mitigation, if known

## Response SLA

- **Acknowledgment:** within 3 business days of report.
- **Initial assessment:** within 7 business days, including severity and next steps.
- **Fix or mitigation timeline:** communicated once the report is triaged, based on severity.

We'll keep you updated as the report is investigated, and credit reporters (unless anonymity is requested) once a fix is released.

## Scope

This policy covers the `gitagent` CLI, SDK, and runtime in this repository. Vulnerabilities in third-party dependencies should be reported upstream, but feel free to flag them here as well if they materially affect gitagent users.
