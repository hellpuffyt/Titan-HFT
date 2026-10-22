# Security policy

## Supported versions

Only the latest release on `main` receives fixes. This is educational software (see the README); it is not intended for production or adversarial environments.

## Reporting a vulnerability

Please **do not open a public issue** for security problems. Report privately through GitHub's private vulnerability reporting:

https://github.com/hellpuffyt/Titan-HFT/security/advisories/new

Include a description, the affected version or commit, and a reproducer (a command file or journal is ideal). You can expect an acknowledgement within 7 days. Fixes are released as soon as practical, and reporters are credited unless they ask not to be.

## In scope

- Memory-safety issues (the SPSC queue contains `unsafe` code).
- Panics or unbounded resource use triggered by command or journal input.
- Incorrect matching that violates the documented rules.

## Known, documented non-goals

These are not vulnerabilities; they are listed in `docs/LIMITATIONS.md`:
- the id hasher is not HashDoS-resistant;
- the journal hash (FNV-1a) detects accidental divergence, not deliberate tampering.
