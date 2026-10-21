# Inventory of the old API

This folder records what `AntWorker-API` does at commit `0210895` (2026-10-20). It is a record, not a contract. Each behavior gets a verdict (keep, drop or change) while the spec is written, and only the behaviors kept or changed carry into the new API.

Production code was the only ground truth. Tests appear only as coverage, because most of them were written by earlier automated reviews rather than in response to real incidents.

## Files

| File | Prefix | Behaviors | Open questions |
|---|---|---:|---:|
| [01-auth.md](01-auth.md) | `AUTH` | 55 | 13 |
| [02-user-account.md](02-user-account.md) | `USER` | 34 | 12 |
| [03-workspaces.md](03-workspaces.md) | `WS` | 44 | 16 |
| [04-files.md](04-files.md) | `FILE` | 41 | 14 |
| [05-audit.md](05-audit.md) | `AUDIT` | 21 | 15 |
| [06-realtime.md](06-realtime.md) | `RT` | 35 | 11 |
| [07-email.md](07-email.md) | `MAIL` | 29 | 10 |
| [08-http-platform.md](08-http-platform.md) | `HTTP` | 49 | 15 |
| [09-data-layer.md](09-data-layer.md) | `DATA` | 50 | 12 |
| [10-observability.md](10-observability.md) | `OBS` | 56 | 13 |
| [11-config-build-deploy.md](11-config-build-deploy.md) | `OPS` | 75 | 13 |
| **Total** | | **489** | **144** |

By kind: 136 capabilities, 141 rules, 175 guarantees and 37 hardening.

All 65 routes in the HTTP route table are cataloged as a behavior in some domain file.

## Reference conventions

- `AUTH-012` is behavior 12 in `01-auth.md`.
- `AUTH Q5` is open question 5 in that file's Open questions section.
- `AUTH O5` is observation 5 in that file's Observations section. `06-realtime.md` groups its observations under headings instead of numbering them, so those are cited by their bold title.

## Next

[SYNTHESIS.md](SYNTHESIS.md) groups the open questions into cross-cutting decisions. Settling those first answers most of the per-domain questions. The rest are settled while each domain's spec is written.

[_TEMPLATE.md](_TEMPLATE.md) is the format every file follows.
