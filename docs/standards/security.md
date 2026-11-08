# Security rules (`SEC`)

The security bar is Level 2 of version 5.0.0 of the Application Security Verification Standard (ASVS) of the Open Worldwide Application Security Project (OWASP) (D26), but for its requirement 6.3.3, which requires a second factor of every user to sign in: here every user may add a second factor, and every superadmin must, so a product's users can sign in without setting one up (D63). These rules cover the mechanisms every module relies on. The threat model in `docs/operations/threat-model.md` maps each threat to the rules that mitigate it, and records the controls that stand in for requirement 6.3.3, as the requirement asks of an application that relaxes it.

## Secrets

### SEC-01 Secrets arrive by environment or file, and stay wrapped
A secret is read from an environment variable, or from a file named by `<NAME>_FILE`. It is held in `platform::types::Secret`, which never prints its value through `Debug`, `Display` or serialization, or in a type of its own that never prints it either, such as `platform::hashing::HmacKey`. A setting that carries a secret, such as a database URL with its password, is a secret setting as a whole: it is read the same way, its secret part is held in `Secret`, and the setting is never printed. Secrets never appear in images, error details, or the URLs the API sends or serves, and telemetry holds none (OBS-04). A signed URL's signature isn't one: it is a keyed hash, under `HMAC_KEY`, which never leaves the API, and proves only its session's download of one file, for an hour (PLATFORM-080, D61).
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
Passwords are hashed with Argon2id at the current OWASP parameters, in the string format of the Password Hashing Competition (PHC), on a blocking thread (CODE-05). A successful sign-in rehashes a password stored with older parameters. The auth spec defines the password policy.
- Why: the recommended algorithm, with parameters that can be raised later (D26).
- Check: `test`.

### SEC-05 Authentication failures reveal nothing
A failed sign-in gives the same response and the same timing whether the email exists or not. Enumeration is accepted only on registration and the email-change pre-flight, both rate limited (D7).
- Why: attackers can't use sign-in to learn which accounts exist.
- Check: `test` (identical responses for known and unknown emails), plus `review` (an unknown email runs the same password hash as a known one).

### SEC-06 Sessions are bounded, and lost access ends open streams
Access tokens are short-lived. Refresh tokens rotate on every use, and reuse is detected. Every session has an absolute maximum lifetime. The auth spec sets the durations.

When a session is revoked, a user is deleted, a membership is removed or a role changes, the affected open streams are closed or re-filtered as soon as the change commits. If that signal is missed, they are closed or re-filtered within the configured re-check interval, 60 seconds by default. [The platform spec](../spec/platform.md#events-and-the-real-time-hub) describes the mechanism.
- Why: a stolen token has a limited life, and lost access ends at once, or within the re-check interval in the worst case (D5, D35).
- Check: `test` (PLATFORM-031, and one test per kind of access change).

### SEC-07 Browsers hold tokens only in cookies they can't read, for listed origins
A browser receives its session's tokens only as cookies that are `Secure`, `HttpOnly` and `SameSite=Strict`, named with the `__Host-` prefix (AUTH-015). A request whose method isn't safe (RFC 9110: `GET`, `HEAD`, `OPTIONS` and `TRACE` are) is served only when it carries no `Origin` or one of the origins that `CORS_ALLOWED_ORIGINS` lists, and, when a session cookie authenticates it, only when it carries a listed one (PLATFORM-059). Only a listed origin gets the headers of cross-origin resource sharing (CORS) that let a browser read a response, or send its cookies (PLATFORM-058).
- Why: no script on a page can read a token, and no other site can make a browser act with one, which is cross-site request forgery (CSRF) (D61).
- Check: `test`.

## Authorization

### SEC-08 Authorization only through `access`
Every workspace route is authorized by `Authorized<P>` (API-14), and every permission decision is made by the `access` policy. No handler or service checks roles or permissions itself.
- Why: one place decides, so one place is audited (D2).
- Check: `test` (TEST-08: every workspace route answers every role as the matrix says for the permission its builder documents, so a handler that extracts `Authorize` for another permission fails wherever the two differ), plus `review`.

### SEC-09 Role rank limits who may change whom
A member can change or remove only members at roles below their own, and can assign only roles below their own. Each workspace has exactly one owner. Two actions are exceptions, and each has its own rule:
- **Transfer.** The owner transfers ownership to a member who holds the admin role. The two swap roles: that member becomes the owner, and the former owner becomes an admin (D2).
- **Leave.** Any member except the owner may leave, which removes their own membership. The owner must transfer ownership before leaving.
- Why: no escalation by delegation, and a workspace always has exactly one owner (D2, D16).
- Check: `test`. Each route that changes or removes a member, or assigns a role, has tests that act on a target of equal and of higher rank, and that assign an equal and a higher role. Transfer and leave each have tests, including an owner trying to leave.

## Requests and responses

### SEC-10 Outbound requests go through one of two clients
All outbound HTTP that application code makes uses one of the two clients in `platform::outbound`. The object storage and telemetry adapters use their libraries' own clients, with endpoints from configuration. The probe that the `health` subcommand sends to the instance's own address (PLATFORM-063) reaches no other service, and the malware scanner's adapter speaks clamd's own protocol, not HTTP, over TCP to the address that configuration names (PLATFORM-079), so neither client applies to them.
- The configured client calls URLs that come from configuration, such as OAuth endpoints. It requires HTTPS, unless the environment is development or test.
- The untrusted client calls URLs that come from a third party, such as an OAuth avatar. It:
  - allows HTTPS only
  - blocks private, loopback and link-local addresses after DNS resolution
  - re-checks every redirect, and limits redirects
  - caps response size and time

In the test environment only, the untrusted client accepts one configured loopback address, so tests can reach a fake provider. Startup refuses that setting in any other environment.
- Why: no server-side request forgery, while every path stays testable (D26).
- Check: `ci: check-layers` (no HTTP client library or TCP stream is named outside `platform::outbound`, the object storage and telemetry adapters, the probe of the `health` subcommand and the scanner's adapter), plus `test` (including a test that the loopback setting is refused outside the test environment).

### SEC-11 Uploaded content can't run in a browser
Stored files are served with:
- `X-Content-Type-Options: nosniff`
- `Content-Security-Policy: sandbox`
- `Content-Disposition: attachment`, unless the type is an allowlisted image type

A public file's original name reaches only those who may read the file, its workspace's members with `files.read` or the user whose file it is: its public download, which anyone may make, names it by its ID and its kind's extension alone (D7, D115, D116).
- Why: an uploaded file can't execute as a page from the API's origin.
- Check: `test`.

### SEC-12 Security headers on every response
Every response carries `X-Content-Type-Options: nosniff` and `Referrer-Policy: no-referrer`, and `Cache-Control: no-store`, except a public file's download, which needs no credential and holds no name, and which shared caches may keep for 5 minutes (FILES-023). The reverse proxy in front of the API terminates TLS and sets HTTP Strict Transport Security (HSTS).
- Why: browser-level defenses are on by default.
- Check: `test`.

### SEC-13 Rate limits protect every route
An IP limit applies to every route except the health probes, before authentication. A per-user limit applies after it. Per-action limits apply to every action that checks a password or a verification code, or sends an email (D11); the tokens of SEC-03 are too long to guess, so a check of one needs no limit of its own. Every check of a user's current password goes through one `auth` function and shares one bucket per user. The client IP is taken from `X-Forwarded-For` only when the request comes from a configured trusted proxy.
- Why: brute force and abuse are bounded, and the IP can't be spoofed.
- Check: `test`.

### SEC-14 A server exposes only the API and a Grafana that signs viewers in
The reference deployment publishes only Caddy's ports, 80 and 443. Caddy serves the API's address and, with the self-hosted observability stack, Grafana's, where every viewer signs in: Grafana admits no anonymous viewer and no one who signs themselves up. Postgres, the collector and the rest of the stack answer only on the deployment's private networks.
- Why: the database holds everything, and the dashboards, logs and traces show how the system is used (D26, D42).
- Check: `ci: e2e`.

## Supply chain

### SEC-15 Advisories block the build
`cargo-deny` fails CI on any RustSec advisory affecting a dependency. Image scans fail CI on fixable critical or high vulnerabilities, in the image's operating system packages and in the crates its binary records: the scan of the image each pipeline run builds, and a weekly scan of the published `edge`, the newest final release and the newest prerelease.
- Why: known vulnerabilities never ship (D21).
- Check: `ci: deny`, plus `ci: image-scan` and `ci: image-rescan`.

### SEC-16 Only vetted cryptography
Cryptography comes from the RustCrypto crates (`aes-gcm`, `hmac`, `sha2`, `argon2`) and from `rustls` with its `ring` provider. This codebase builds no primitive or construction of its own. Crates that implement the Postgres protocol may build its standard constructions from those crates' primitives: the database driver's SCRAM-SHA-256 (Salted Challenge Response Authentication Mechanism) sign-in, and the password verifier that `bootstrap` computes with `postgres-protocol` (PLATFORM-002).
- Why: cryptography is easy to get subtly wrong, so it comes from vetted crates, and a protocol's standard construction is safer in its driver than rebuilt here. The `ring` provider builds on every supported platform without extra toolchains.
- Check: `ci: deny` (bans `openssl`, `native-tls` and `aws-lc-rs`), plus `review`.

## Disclosure

### SEC-17 Vulnerabilities have a reporting path
`SECURITY.md` states how to report a vulnerability and what response to expect. Each security fix is marked in `CHANGELOG.md` (DOC-11).
- Why: forks and users learn about fixes they need to port.
- Check: `review`.

## Privacy

### SEC-18 Audit entries hold no plain personal data, except IP address and user agent
The values a module records in an audit entry are IDs and non-personal fields. An email appears only as a keyed hash. IP address and user agent are stored, but can be read only with the permission `access` defines for them (D2, D6). Erasing an account anonymizes its entries (D31).
- Why: the audit log is kept longer than most data and read by more people (D6, D7).
- Check: `test` (each audit action's test asserts its stored values), plus `review`.

### SEC-19 Events reach only authorized streams
A user event reaches only that user's streams. A workspace event reaches a stream only if its principal holds, in that workspace, the permission that the event requires. Replay delivers an event only to members whose membership already existed when the event was created.
- Why: events never reveal what the REST permission checks would withhold, and new members don't receive history from before they joined (D2, D5).
- Check: `test` (PLATFORM-029, with events that only the tests publish, proves how user and workspace events reach streams; each workspace event a module declares is tested against a member with its permission, a member without it while some role lacks it, and a member who joined after the event; and each user event a module declares is tested to name the user it is for, as the test of the behavior that publishes it reads its row), plus `ci: check-test-map` (each workspace event that a module's `events.rs` declares is named in a test whose name starts with `sec_19_`), plus `review` (what that test does with it).
