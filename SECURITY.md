# Security Policy

syswatch is a read-only, single-host diagnostics tool by design — it observes
the system and never mutates it. That keeps the attack surface small, but
"small" isn't "zero", and reports are very welcome.

## Reporting a vulnerability

**Please do not open a public issue for security problems.**

- Preferred: [**GitHub private vulnerability reporting**](https://github.com/matthart1983/syswatch/security/advisories/new)
  ("Report a vulnerability" under the Security tab).
- Alternatively: email **matthew.t.hartley@gmail.com** with `[syswatch security]`
  in the subject.

You'll get an acknowledgement within **72 hours** and a triage verdict within
**7 days**. Confirmed vulnerabilities ship as a patch release as soon as the
fix is ready. Please allow a reasonable window before public disclosure —
happy to coordinate timing, and you'll be credited in the advisory and release
notes unless you'd rather stay anonymous.

## Supported versions

syswatch is pre-1.0. Security fixes target the **latest release** only —
please reproduce on the current version before reporting.

| Version | Supported |
|---|---|
| latest release | ✅ |
| anything older | ❌ upgrade first |

## Scope — what we care about most

1. **Violations of the read-only promise** — any code path where syswatch
   modifies system state, however indirectly. That promise is the product;
   breaking it is a security bug.
2. **Parsing untrusted data** — syswatch renders process names, file paths,
   log fragments, and device strings that other (possibly malicious local)
   software controls. Crashes, hangs, or terminal escape-sequence injection
   through those values are in scope.
3. **Privilege handling** — anything that leverages an elevated syswatch
   process (some collectors read more when run with sudo) to do work for an
   unprivileged user.
4. **Information exposure** — session exports or snapshots landing with
   overly permissive file modes, or leaking data from other users' processes
   beyond what the OS already grants.

Out of scope: vulnerabilities in dependencies with no syswatch-specific
exploit path (report upstream, though a heads-up is welcome), and issues that
require an already-root attacker.
