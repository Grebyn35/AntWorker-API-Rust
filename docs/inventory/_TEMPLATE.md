# Inventory: <Domain name> (`<PREFIX>`)

Reference: `AntWorker-API` @ `0210895` (2026-10-20). This file records what the old API does. It does not prescribe what the new one should do.

## Scope

What this domain covers, where its boundary with neighbouring domains sits, and the files that were read.

## Behaviors

One behavior = one externally observable outcome or rule that a reimplementation could get wrong on its own. An endpoint usually has several (the success path plus each distinct business rule). Error cases that are plain validation go under the endpoint's behavior, not as separate entries. Each behavior must be specific enough to write a test from.

### <PREFIX>-001 <Short title>

- **Behavior:** One or two sentences, observable from the HTTP surface, the database, or another side channel.
- **Kind:** `capability` (something a user or client can do), `rule` (a business rule constraining a capability), `guarantee` (a security, consistency, isolation or operational property the system promises), or `hardening` (defensive handling of an internal edge case or failure mode that no user-facing rule requires, such as a retry, a CAS guard or a recovery path). When torn between two, pick the more user-facing one and say why in one clause.
- **Trigger:** `METHOD /path`, background job `<name>`, startup, or an event.
- **Auth:** public, session, or workspace-scoped, plus required permission(s).
- **Input & validation:** Fields, formats, limits, and what is rejected.
- **Effects:** Database writes, audit actions, SSE events, emails, metrics, and notable logs.
- **Errors:** `code` → HTTP status → when.
- **Guarantees:** Security, consistency, idempotency, concurrency, or ordering properties.
- **Source:** `path/to/file.rs:line` (several allowed, relative to `AntWorker-API/`).
- **Tests:** Test files or names that cover it, inline or in `tests/`, or "none found". This records coverage only. It is not evidence that the behavior is intended or important.

Omit a field only when it truly does not apply. Behavior, Kind, Trigger, Source and Tests are always present.

Production code is the only ground truth. Most tests in the old repo were written by earlier automated review sessions, not in response to real incidents. A behavior that only a test asserts is not a behavior; record it under Observations.

## Data owned

Tables, notable columns, RLS policies, stored procedures, triggers and background tasks this domain owns.

## Configuration

Environment variables this domain reads, with defaults and validation.

## Cross-domain dependencies

What this domain calls in other domains, and what calls into it.

## Observations

Factual notes on inconsistencies, duplication, dead code, surprises, and places where docs or comments disagree with code. For each one: what it is, where (`file:line`), and why it matters. No fixes or recommendations.

## Open questions

Things the code does not answer, and behavior that looks unintentional and needs the owner's decision.
