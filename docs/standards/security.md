# Security rules (`SEC`)

The security bar is Level 2 of the Application Security Verification Standard (ASVS) of the Open Worldwide Application Security Project (OWASP) (D26). These rules cover the mechanisms every module relies on. The threat model in `docs/operations/threat-model.md` maps each threat to the rules that mitigate it.

## Secrets

### SEC-01 Secrets arrive by environment or file, and stay wrapped
A secret is read from an environment variable, or from a file named by `<NAME>_FILE`. It is held in `platform::types::Secret`, which never prints its value through `Debug`, `Display` or serialization. A setting that carries a secret, such as a database URL with its password, is a secret setting as a whole: it is read the same way, its secret part is held in `Secret`, and the setting is never printed. Secrets never appear in images, logs, error details, or the URLs the API sends, serves or logs.
- Why: the standard delivery paths for containers, and no accidental disclosure (D26).
- Check: `compiler` (`Secret` has no way to print its value), plus `review`.

### SEC-02 Secrets at rest are encrypted with associated data
Third-party credentials stored in the database, such as OAuth provider tokens, are encrypted with AES-256-GCM. The key is versioned. The associated data binds each ciphertext to its table, its column, and the ID of the row that owns the secret (for a provider token, its identity), which exists before the secret is written. No other column is encrypted at the application level.
- Why: a ciphertext can't be moved to another row, and the encryption scope stays small (D14).
- Check: `test`.

## Credentials and tokens

### SEC-03 Tokens and codes are random, hashed and compared in constant time
Access tokens, refresh tokens and verification codes come from the operating system's secure random source. Tokens are 256 bits and are stored only as SHA-256 hashes. Verification codes are short enough to guess, so they are stored as HMAC-SHA-256 (a hash-based message authentication code with SHA-256) under a secret key; the auth spec sets their lengths. Rotating that key invalidates outstanding codes, which expire within minutes anyway. Any comparison involving a secret is constant-time.
- Why: a database dump reveals no usable token or code, and timing reveals nothing (D32).
- Check: `test`, plus `review`.

### SEC-04 Passwords use Argon2id
Passwords are hashed with Argon2id at the current OWASP parameters, in the string format of the Password Hashing Competition (PHC), on a blocking thread (CODE-05). A successful login rehashes a password stored with older parameters. The auth spec defines the password policy.
- Why: the recommended algorithm, with parameters that can be raised later (D26).
- Check: `test`.

### SEC-05 Authentication failures reveal nothing
A failed login gives the same response and the same timing whether the email exists or not. Enumeration is accepted only on registration and the email-change pre-flight, both rate limited (D7).
- Why: attackers can't use sign-in to learn which accounts exist.
- Check: `test` (identical responses for known and unknown emails), plus `review` (an unknown email runs the same password hash as a known one).

### SEC-06 Sessions are bounded, and lost access ends open streams
Access tokens are short-lived. Refresh tokens rotate on every use, and reuse is detected. Every session has an absolute maximum lifetime. The auth spec sets the durations.

When a session is revoked, a user is deleted, a membership is removed or a role changes, the affected open streams are closed or re-filtered as soon as the change commits. If that signal is missed, they are closed or re-filtered within the configured re-check interval, 60 seconds by default. [The platform spec](../spec/platform.md#events-and-the-real-time-hub) describes the mechanism.
- Why: a stolen token has a limited life, and lost access ends at once, or within the re-check interval in the worst case (D5, D35).
- Check: `test` (PLATFORM-017, and one test per kind of access change).

## Authorization

### SEC-07 Authorization only through `access`
Every workspace route is authorized by `Authorized<P>` (API-14), and every permission decision is made by the `access` policy. No handler or service checks roles or permissions itself.
- Why: one place decides, so one place is audited (D2).
- Check: `compiler` (API-14), plus `review`.

### SEC-08 Role rank limits who may change whom
A member can change, remove or invite only members at roles below their own, and can assign only roles below their own. Each workspace has exactly one owner. Two actions are exceptions, and each has its own rule:
- **Transfer.** The owner transfers ownership to a member who holds the admin role. The two swap roles: that member becomes the owner, and the former owner becomes an admin (D2).
- **Leave.** Any member except the owner may leave, which removes their own membership. The owner must transfer ownership before leaving.
- Why: no escalation by delegation, and a workspace always has exactly one owner (D2, D16).
- Check: `test`. Each route that changes, removes or invites a member has tests that act on a target of equal and of higher rank, and that assign an equal and a higher role. Transfer and leave each have tests, including an owner trying to leave.

## Requests and responses

### SEC-09 Outbound requests go through one of two clients
All outbound HTTP that application code makes uses one of the two clients in `platform::outbound`. The object storage and telemetry adapters use their libraries' own clients, with endpoints from configuration. The probe that the `health` subcommand sends to the instance's own address (PLATFORM-044) reaches no other service, so neither client applies to it.
- The configured client calls URLs that come from configuration, such as OAuth endpoints. It requires HTTPS, unless the environment is development or test.
- The untrusted client calls URLs that come from a third party, such as an OAuth avatar. It:
  - allows HTTPS only
  - blocks private, loopback and link-local addresses after DNS resolution
  - re-checks every redirect, and limits redirects
  - caps response size and time

In the test environment only, the untrusted client accepts one configured loopback address, so tests can reach a fake provider. Startup refuses that setting in any other environment.
- Why: no server-side request forgery, while every path stays testable (D26).
- Check: `ci: check-layers` (no other HTTP client is built, apart from the probe of the `health` subcommand), plus `test` (including a test that the loopback setting is refused outside the test environment).

### SEC-10 Uploaded content can't run in a browser
Stored files are served with:
- `X-Content-Type-Options: nosniff`
- `Content-Security-Policy: sandbox`
- `Content-Disposition: attachment`, unless the type is an allowlisted image type

Public files never expose their original file name (D7).
- Why: an uploaded file can't execute as a page from the API's origin.
- Check: `test`.

### SEC-11 Security headers on every response
Every response carries `X-Content-Type-Options: nosniff` and `Referrer-Policy: no-referrer`. Authenticated responses carry `Cache-Control: no-store`, except public files. The reverse proxy in front of the API terminates TLS and sets HTTP Strict Transport Security (HSTS).
- Why: browser-level defenses are on by default.
- Check: `test`.

### SEC-12 Rate limits protect every route
An IP limit applies to every route except the health probes, before authentication. A per-user limit applies after it. Per-action limits apply to every action that checks a secret or sends an email (D11). Every check of a user's current password goes through one `auth` function and shares one bucket per user. The client IP is taken from `X-Forwarded-For` only when the request comes from a configured trusted proxy.
- Why: brute force and abuse are bounded, and the IP can't be spoofed.
- Check: `test`.

### SEC-18 A server exposes only the API and a Grafana that signs viewers in
The reference deployment publishes only Caddy's ports, 80 and 443. Caddy serves the API's address and, with the self-hosted observability stack, Grafana's, where every viewer signs in: Grafana admits no anonymous viewer and no one who signs themselves up. Postgres, the collector and the rest of the stack answer only on the deployment's private networks.
- Why: the database holds everything, and the dashboards, logs and traces show how the system is used (D26, D42).
- Check: `ci: e2e`.

## Supply chain

### SEC-13 Advisories block the build
`cargo-deny` fails CI on any RustSec advisory affecting a dependency. Image scans fail CI on fixable critical or high vulnerabilities, in the image's operating system packages and in the crates its binary records: the scan of the image each pipeline run builds, and a weekly scan of the published `edge`, the newest final release and the newest prerelease.
- Why: known vulnerabilities never ship (D21).
- Check: `ci: deny`, plus `ci: image-scan` and `ci: image-rescan`.

### SEC-14 Only vetted cryptography
Cryptography comes from the RustCrypto crates (`aes-gcm`, `hmac`, `sha2`, `argon2`) and from `rustls` with its `ring` provider. This codebase builds no primitive or construction of its own. Crates that implement the Postgres protocol may build its standard constructions from those crates' primitives: the database driver's SCRAM-SHA-256 (Salted Challenge Response Authentication Mechanism) sign-in, and the password verifier that `bootstrap` computes with `postgres-protocol` (PLATFORM-010).
- Why: cryptography is easy to get subtly wrong, so it comes from vetted crates, and a protocol's standard construction is safer in its driver than rebuilt here. The `ring` provider builds on every supported platform without extra toolchains.
- Check: `ci: deny` (bans `openssl`, `native-tls` and `aws-lc-rs`), plus `review`.

## Disclosure

### SEC-15 Vulnerabilities have a reporting path
`SECURITY.md` states how to report a vulnerability and what response to expect. Each security fix is marked in `CHANGELOG.md` (DOC-12).
- Why: forks and users learn about fixes they need to port.
- Check: `review`.

## Privacy

### SEC-16 Audit entries hold no plain personal data, except IP address and user agent
The values a module records in an audit entry are IDs and non-personal fields. An email appears only as a keyed hash. IP address and user agent are stored, but can be read only with the permission `access` defines for them (D2, D6). Erasing an account anonymizes its entries (D31).
- Why: the audit log is kept longer than most data and read by more people (D6, D7).
- Check: `test` (each audit action's test asserts its stored values), plus `review`.

### SEC-17 Events reach only authorized streams
A user event reaches only that user's streams. A workspace event reaches a stream only if its principal holds, in that workspace, the permission the event's type requires. Replay delivers an event only to members whose membership already existed when the event was created.
- Why: events never reveal what the REST permission checks would withhold, and new members don't receive history from before they joined (D2, D5).
- Check: `test` (PLATFORM-016, and each event type is tested against a member with the permission, a member without it, and a member who joined after the event).
