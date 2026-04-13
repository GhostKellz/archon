# Security Policy

## Supported Versions

This repository does not currently publish a formal long-term support matrix.
Security fixes are applied to the active development branch when they are verified and ready.

## Reporting a Vulnerability

Report suspected vulnerabilities privately.

- Do not open public issues for unpatched security problems.
- Send a report with reproduction details, impact, affected configuration, and any proof-of-concept material available.
- Include the commit hash or branch name if the issue is not present on `main`.

If a private reporting channel is not already established for this project, coordinate directly with the maintainers through the normal private contact path you use for repository access.

## What To Include

- A clear description of the issue
- Preconditions and affected components
- Step-by-step reproduction instructions
- Expected impact
- Any relevant logs, traces, or minimized test cases

## Response Expectations

- Reports will be triaged and reproduced before a fix is accepted as valid
- Remediation may include code changes, dependency updates, configuration hardening, or documentation updates
- Public disclosure should wait until a fix or mitigation is available

## Dependency Hygiene

This project uses `cargo audit` as one input to dependency review.
RustSec advisories are evaluated case by case based on reachability, upstream status, and whether a safe in-repo remediation is available.
