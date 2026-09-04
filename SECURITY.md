# Security policy

## Reporting a vulnerability

Email **von.chance@venuxsystems.com**. Do not open a public issue for a
suspected sandbox escape, a permission that is granted when the manifest did
not grant it, or a way to read host memory from a guest.

What you can expect, stated at the pace one person can actually hold to:

- Acknowledgement within **5 business days**.
- An assessment — whether it reproduces, and what the fix looks like — within
  **30 days**.
- A fix released, or a public statement of why there will not be one, within
  **90 days** of the report. If a report needs longer, you will hear why before
  the 90 days are up rather than after.

Credit in the changelog and the advisory unless you would rather not be named.

Please do not report a sandbox escape in Wasmtime itself here; those go to the
[Bytecode Alliance](https://bytecodealliance.org/security) directly, and they
have a real embargo process. If you are unsure which layer a bug is in, mail
here and it will get routed.

## Supported versions

**0.1.x** is the supported line. Fixes land on `main` and go out in the next
0.1.x tag; there is no backport branch, because there is nothing yet to
backport to. The only versions on crates.io are the `0.0.0` placeholders that
reserve the names — they contain no working code and receive no fixes, so
upgrading means moving the tag you build from.

## What is in scope

Read **[docs/SECURITY.md](docs/SECURITY.md)** first — it states what the
sandbox does and does not protect against, and what the trusted computing base
is. A report is most useful when it names which of those claims it breaks.
