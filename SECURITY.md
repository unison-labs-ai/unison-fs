# Security Policy

## Reporting a vulnerability

Please report security issues privately — **do not open a public GitHub issue.**

Email **security@unisonlabs.ai** with:

- a description of the issue and its impact,
- steps to reproduce (a proof-of-concept if you have one),
- any suggested remediation.

We aim to acknowledge within 3 business days and to keep you updated as we
investigate. We will credit reporters who want it once a fix ships.

## Scope

This repository is the **open-source filesystem client** (Rust binary + library).
It holds no secrets and is not itself a security boundary — authentication,
authorization, workspace isolation, and rate limiting are enforced server-side by
the Unison brain API.

Reports about the client are most useful when they concern:

- credential handling on disk (`~/.config/unisonfs/config.json`),
- the machine-auth provisioning flow as implemented client-side,
- dependency or supply-chain risks.

Server-side or account issues should go to the same address.

## Credential handling

The client stores a bearer token (`usk_…`) in `~/.config/unisonfs/config.json`
with `0600` permissions, or reads it from the `UNISON_TOKEN` environment variable.
The token is never logged, written to cache files, or transmitted anywhere except
the configured API host (`UNISON_API_URL`, default `https://brain.unisonlabs.ai`).
