# Security rules (`SEC`)

The security bar is Level 2 of version 5.0.0 of the Application Security Verification Standard (ASVS) of the Open Worldwide Application Security Project (OWASP) (D26), but for the requirements that it leaves out. One is 6.3.3, which requires a second factor of every user to sign in: here every user may add a second factor, so a product's users can sign in without setting one up (D63), and a deployment may require one of every user, which meets the requirement in full (D133). The others are those that the reference deployment can't meet on one server, or with the tools it runs, and those that a decision of the API's design leaves out, each of which the threat model lists with its reason and the controls that stand in for it (D198, D207).
Every superadmin must have one, whatever the deployment requires of its users (SUPERADMINS-005). <!-- part: superadmins -->
In the part that stores files, the bar leaves out more requirements, which the threat model lists as it lists the others (D199). <!-- part: files -->
These rules cover the mechanisms every module relies on. The threat model in `docs/operations/threat-model.md` maps each threat to the rules that mitigate it, and records, for each requirement that the bar leaves out, why, and the controls that stand in for it, as requirement 6.3.3 asks of an application that relaxes it. `docs/operations/asvs.md` maps each requirement of the bar to the rules and behaviors that meet it, and says which the bar leaves out.

## Secrets

### SEC-01 Secrets arrive by environment or file, and stay wrapped
A secret is read from an environment variable, or from a file named by `<NAME>_FILE`. It is held in `platform::types::Secret`, which never prints its value through `Debug`, `Display` or serialization, or in a type of its own that never prints it either, such as `platform::hashing::HmacKey`. A setting that carries a secret, such as a database URL with its password, is a secret setting as a whole: it is read the same way, its secret part is held in `Secret`, and the setting is never printed. Secrets never appear in images, error details, or the URLs the API sends or serves, and telemetry holds none (OBS-04). An authenticator app's setup is the one answer that gives its caller a secret that the API then keeps encrypted, the app's, once, in base32 and in the `otpauth://` uniform resource identifier (URI) that a client shows as a quick-response (QR) code (AUTH-055). The other answers that hold a secret hold one that the API keeps only as a hash (SEC-03): ten recovery codes, once, which a setup's confirmation and a request for new ones answer (AUTH-056, AUTH-063), a session's tokens and a device token (AUTH-007, AUTH-019), and a sign-in's challenge (AUTH-058).
A signed URL's signature isn't one: it is a keyed hash, under `HMAC_KEY`, which never leaves the API, and proves only its session's download of one file, for an hour (PLATFORM-080, D61). <!-- part: files -->
The reference deployment gives no credential a value of its own choosing: its operator enters each one that the deployment doesn't generate, such as another system's key pair, or the password of Grafana's administrator, which is theirs to choose (SEC-14), and the deployment generates the others from the operating system's random source ([deploy.md](../operations/deploy.md#setting-up-a-server)).
- Why: the standard delivery paths for containers, and no accidental disclosure (D26).
- Check: `compiler` (`Secret` has no way to print its value), plus `review`.

### SEC-02 Secrets at rest are encrypted with associated data
Secrets stored in the database that the API must read back, such as an authenticator app's secret, are encrypted with AES-256-GCM. The key is versioned: `ENCRYPTION_KEYS` holds each version, `ENCRYPTION_KEY_VERSION` names the one that encrypts, and each ciphertext records its own (PLATFORM-086). The associated data binds each ciphertext to its table, its column, and the ID of the row that owns the secret, which exists before the secret is written. No other column is encrypted at the application level.
- Why: a ciphertext can't be moved to another row, and the encryption scope stays small (D14).
- Check: `test` (PLATFORM-086, PLATFORM-087, AUTH-066), plus `review` (the cipher, and that no other column is encrypted).

## Credentials and tokens

### SEC-03 Tokens and codes are random, hashed and compared in constant time
Access tokens, refresh tokens and verification codes come from the operating system's secure random source. Tokens are 256 bits and are stored only as SHA-256 hashes, and so are recovery codes, whose 120 bits are too many to guess, each hash covering its user's ID (AUTH-062). Verification codes are short enough to guess, so they are stored as HMAC-SHA-256 (a hash-based message authentication code with SHA-256) under a secret key; the auth spec sets their lengths. Rotating that key invalidates outstanding codes, which expire within minutes anyway. Any comparison involving a secret is constant-time.
- Why: a database dump reveals no usable token or code, and timing reveals nothing (D32).
- Check: `test`, plus `review`.

### SEC-04 Passwords use Argon2id
Passwords are hashed with Argon2id at the current OWASP parameters, in the string format of the Password Hashing Competition (PHC), on a blocking thread (CODE-05). A successful sign-in rehashes a password stored with older parameters. The auth spec defines the password policy. A password lasts until its user changes or resets it: nothing expires one, or asks its user for a new one after a time. The API takes no hint of a password, and no answer to a secret question.
- Why: the recommended algorithm, with parameters that can be raised later (D26).
- Check: `test` (AUTH-014; and AUTH-093 and OBS-06, a password's hash in a turn of the helper that runs it on a blocking thread), plus `review` (that nothing expires a password, and that no route takes a hint or a secret question).

### SEC-05 Authentication failures reveal nothing
A failed sign-in gives the same response, after the same password-hashing work, whether the email exists or not, and so does the check of a password reset's code; a reset's request gives the same response and enqueues the same job. A wrong password of an account that exists also commits the record of its failure before its answer (AUTH-005), which a sign-in for an unknown email has none of: the time of that write isn't hidden (D220). Enumeration is accepted only on registration and the email-change pre-flight, both rate limited (D7).
- Why: an attacker reads which accounts exist neither from a sign-in's answer nor from the time of its hashing, the work that takes it longest.
- Check: `test` (identical responses for known and unknown emails: AUTH-005, AUTH-034, AUTH-038; the same password-hashing work, in a sign-in and in a reset's confirmation: AUTH-005, AUTH-038; the same job, in a reset's request: AUTH-034), plus `review` (that no work of the request's but the record of a failure tells the two apart by its time).

### SEC-06 Sessions are bounded, and lost access ends open streams
A session starts only in a request that carries its user's password, a provider's code for the user, or the challenge of such a sign-in with its second factor. Access tokens are short-lived. Refresh tokens rotate on every use, and reuse is detected. A proof of who a session's user is gives the session new tokens, as a refresh does (AUTH-095), and a request that starts a session ends the one that its device, or its browser, held (AUTH-008, AUTH-096). Every session has an absolute maximum lifetime. The auth spec sets the durations. A session's end refuses each request that is authenticated after it; a request that the session authenticated before it completes (AUTH-009, D224).

When a session is revoked, a user is deleted, a membership is removed or a role changes, the affected open streams are closed or re-filtered as soon as the change commits. If that signal is missed, they are closed or re-filtered within the re-check interval, which `STREAM_RECHECK_SECONDS` in [`.env.example`](../../.env.example) sets. [The platform spec](../spec/platform.md#events-and-the-real-time-hub) describes the mechanism.
- Why: a stolen token has a limited life, and lost access ends at once, or within the re-check interval in the worst case (D5, D35).
- Check: `test` (PLATFORM-031, and one test per kind of access change), plus `review` (what starts a session).

### SEC-07 Browsers hold tokens only in cookies they can't read, for listed origins
A browser receives its session's tokens only as cookies that are `Secure`, `HttpOnly` and `SameSite=Strict`, named with the `__Host-` prefix (AUTH-015). A request whose method isn't safe (RFC 9110: `GET`, `HEAD`, `OPTIONS` and `TRACE` are) is served only when it carries no `Origin` or one of the origins that `CORS_ALLOWED_ORIGINS` lists, and, when a session cookie authenticates it, only when it carries a listed one (PLATFORM-059). Only a listed origin gets the headers of cross-origin resource sharing (CORS) that let a browser read a response, or send its cookies (PLATFORM-058).
- Why: no script on a page can read a token, and no other site can make a browser act with one, which is cross-site request forgery (CSRF) (D61).
- Check: `test`.

## Authorization

### SEC-08 Authorization only through `access`
Every workspace route is authorized by `Authorized<P>` (API-14), and every permission decision is made by the `access` policy. No handler or service checks roles or permissions itself.
A superadmin's route is authorized by `Superadmin`, which the policy of `superadmins` decides, by no role and no permission, and a generated test sends each such route as a caller who is no superadmin (SUPERADMINS-004). <!-- part: superadmins -->
- Why: one place decides, so one place is audited (D2).
- Check: `test` (TEST-08: every workspace route answers every role as the matrix says for the permission its builder documents, so a handler that extracts `Authorize` for another permission fails wherever the two differ), plus `review`.

### SEC-09 Role rank limits who may change whom
A member can change or remove only members at roles below their own, and can assign only roles below their own. Each workspace has exactly one owner.
An invitation offers its role, so a member invites, changes, sends again or cancels only an invitation at a role below their own (D165). <!-- part: invitations -->
Three actions are exceptions to the rank of members, and each has its own rule:
- **Transfer.** The owner transfers ownership to a member who holds the admin role. The two swap roles: that member becomes the owner, and the former owner becomes an admin (D2).
- **Leave.** Any member except the owner may leave, which removes their own membership. The owner must transfer ownership before leaving.
- **Deletion.** Of a workspace's members, the owner alone deletes it, which ends every membership with it, the owner's own among them. A deletion of an account ends its user's memberships as ACCOUNTS-013 and ACCOUNTS-014 say (D16).
  A superadmin deletes any workspace, as no member of it, which ends every membership the same way (WORKSPACES-030, D62). <!-- part: superadmins -->
- Why: no escalation by delegation, and a workspace always has exactly one owner (D2, D16).
- Check: `test` (ACCESS-004, WORKSPACES-006, WORKSPACES-013, WORKSPACES-014, WORKSPACES-015, WORKSPACES-017). Each route that changes or removes a member, or assigns a role, has tests that act on a target of equal rank, and of higher rank wherever one can exist, and that assign an equal and a higher role.
  So has each route that acts on an invitation, an offer of a role. <!-- part: invitations -->
  Transfer, leave and deletion each have tests, including an owner trying to leave, and an admin trying to delete.

## Requests and responses

### SEC-10 Outbound requests go through one of two clients
All outbound HTTP that application code makes uses one of the two clients in `platform::outbound`. The telemetry adapters use their libraries' own clients, with endpoints from configuration, and follow no redirect. The probe that the `health` subcommand sends to the instance's own address (PLATFORM-063) reaches no other service, so neither client applies to it.
The object storage adapter sends its requests through a client of its own too, to the endpoint that configuration names, and follows no redirect (PLATFORM-094), and the malware scanner's adapter speaks clamd's own protocol, not HTTP, over TCP to the address that configuration names (PLATFORM-079), so neither client applies to them either. <!-- part: files -->
The relay of clients' batches of telemetry is a telemetry adapter too: it sends each batch to the collector's receiver that configuration names, through a client of its own, which follows no redirect and sends none of the caller's credentials (PLATFORM-091). <!-- part: client_telemetry -->
- The configured client calls URLs that come from configuration or from the code, such as an OAuth provider's endpoints. It requires HTTPS, unless the environment is development or test, follows no redirect, and caps response size and time.
- The untrusted client calls URLs that come from a third party. It:
  - allows HTTPS only, at HTTPS's own port
  - fetches only from the hosts that it is built with, each named as a URL holds it, so from no host that a URL gives as an IP address
  - blocks private, loopback and link-local addresses after DNS resolution
  - re-checks every redirect, and limits redirects
  - caps response size and time
  - sends no credential, and goes through no proxy, which would resolve a name in its place

  The picture of a user that an OAuth provider names is such a URL, and the client that fetches it is built with the hosts that the deployment's providers name for their users' pictures (AUTH-097). <!-- part: files -->

In the test environment only, the configured client sends every request to one configured loopback origin, keeping its path, and the untrusted client accepts that origin, over `http`, as the one address that isn't public, whatever hosts it is built with, so tests can reach a fake provider. Startup refuses the setting in any other environment.
- Why: no server-side request forgery, while every path stays testable (D26). A third party's URL reaches only the hosts that the code or the deployment named beforehand, as requirements 1.3.6 and 13.2.4 of ASVS ask (D195).
- Check: `ci: check-layers` (no HTTP client library or TCP stream is named outside the places that this rule allows), plus `test` (including a test that the loopback setting is refused outside the test environment).

<!-- part: files -->
### SEC-11 Uploaded content can't run in a browser
Stored files, avatars and workspaces' pictures are served with:
- `X-Content-Type-Options: nosniff`
- `Content-Security-Policy: sandbox`
- `Content-Disposition: attachment`, unless the type is an allowlisted image type

A public file's original name reaches only those who may read the file, its workspace's members with `files.read` or the user whose file it is: its public download, which anyone may make, names it by its ID and its kind's extension alone (D7, D115, D116).
- Why: an uploaded file can't execute as a page from the API's origin.
- Check: `test` (PLATFORM-081, FILES-009, FILES-023, FILES-024, FILES-027).
<!-- end part: files -->

### SEC-12 Security headers on every response
Every response that the API's router answers carries `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`, `Cache-Control: no-store`, and `Content-Security-Policy: default-src 'none'; base-uri 'none'; frame-ancestors 'none'`, with which a browser loads nothing for the response, takes no base address from it, and shows it in no frame. A download carries, in that policy's place, the same policy with `sandbox` added (PLATFORM-081), and the page of the API reference that development serves one that lets the page load what it shows, with the same `base-uri` and `frame-ancestors` (PLATFORM-052). A request that the API's own HTTP server refuses before the router gets that server's answer (PLATFORM-099), which carries none of the four.
A public file's download, an avatar's and a workspace's picture's are the only responses without `no-store`: each needs no credential, and names its bytes by their ID and their kind's extension alone (SEC-11), so shared caches may keep it for 5 minutes (FILES-023, FILES-024, FILES-027). <!-- part: files -->

The reverse proxy in front of the API terminates TLS and sets HTTP Strict Transport Security (HSTS), for a year and for subdomains too, at the API's address and at Grafana's. At the API's address it also sets each of the API's four headers on a response that lacks it, as the answer of a replica's own server does, and the 503 that it answers itself while no replica can carries the four too (OPS-05). A request that Caddy's server refuses before a site handles it, because it can't read it, or doesn't take its version, its transfer encoding or headers of its size, gets that server's own answer, a 400, a 431, a 501 or a 505 with a line of plain text. That answer carries none of these headers, since no setting of a site reaches it. A page of Grafana's carries `X-Content-Type-Options: nosniff`, `Referrer-Policy: same-origin`, which the proxy sets, and the `Content-Security-Policy` that `deploy/observability/compose.yaml` gives Grafana, with `object-src 'none'`, `frame-ancestors 'none'`, and `base-uri 'self'`, since Grafana's page names a base address.
- Why: browser-level defenses are on by default.
- Check: `test`, plus `ci: e2e` (the headers of the 503 that the proxy answers itself at the API's address, of the answer of a replica's own server that the proxy passes on, and of Grafana's sign-in page), plus `review` (the answer of Caddy's own server to a request that it refuses before a site handles it, as a run shows it). PLATFORM-099's tests read the answer of the API's own server.

### SEC-13 Rate limits protect every route
An IP limit applies to every route except the health probes, before authentication, and counts each request but those that PLATFORM-054 names, such as a preflight of an origin that the API lists. A per-user limit applies after it. Per-action limits apply to every action that checks a password or a verification code, or sends an email (D11); the tokens of SEC-03 are too long to guess, so a check of one needs no limit of its own. Every check of a signed-in user's current password goes through one `auth` function and shares one bucket per user. The client IP is taken from `X-Forwarded-For` only when the request comes from a configured trusted proxy. In the reference deployment the replicas list the subnet of a network that only the proxy and they join, beside the addresses that the proxy trusts in turn, so no other service of the deployment names a client to a replica (D221).
A user's uploads count their bytes in a budget of a day, whatever their number (PLATFORM-093, D196). <!-- part: files -->
- Why: brute force and abuse are bounded, and the IP can't be spoofed.
- Check: `test` (PLATFORM-054 and PLATFORM-038, the IP and user limits and the probes' exemption; PLATFORM-058, a listed origin's preflight's; PLATFORM-053, the trusted proxy; AUTH-022, the one bucket of a signed-in user's password checks; and each per-action limit has its test, such as AUTH-006's and AUTH-033's), plus `ci: e2e` (each item below, in the reference deployment).
  - the containers on the proxy's network
  - that a replica counts two requests under the clients that their `X-Forwarded-For` names when they come from that network, and under their sender from each other network that the replica joins
  - that a request through the proxy is recorded under an address that isn't the proxy's
  - the deploy's refusal of a subnet that isn't an IPv4 range within its bounds

### SEC-14 A server exposes only the API and a Grafana that signs viewers in
The reference deployment publishes only Caddy's ports, 80 and 443. Caddy serves the API's address and, with the self-hosted observability stack, Grafana's, where every viewer signs in: Grafana admits no anonymous viewer and no one who signs themselves up, and serves its own metrics to no one, which nothing of the stack scrapes; its administrator signs in under a name and with a password that the operator chose at the setup, in place of the `admin` that every Grafana starts with, and a deploy generates neither; its health check answers without a sign-in, as the API's probes do (OPS-05).

A viewer's session lasts 7 days without a request, and 30 days at most, and a viewer holds any number at once. Its cookie is `__Host-grafana_session`, which is `Secure`, `HttpOnly` and `SameSite=Lax`, so that a link on another site, such as an alert's, opens a dashboard, while what another site's page sends of its own, such as a form's `POST` or a frame's request, carries no cookie; Grafana names its one other cookie, `grafana_session_expiry`, itself, which holds the time at which the session's token changes next. Grafana hashes its sessions' tokens, and encrypts what it stores, under a key that a deploy generates, in place of the one that its image holds. Its one WebSocket, which its pages open, takes a browser's connection only from a page on Grafana's own host name, as Grafana does while its `allowed_origins` is empty.

Caddy serves no file: it passes a request on, or answers what its sites state, and a request that names neither address, such as one for the server's own, gets no answer, since a site that takes every other host aborts it. What Caddy's server refuses before a site handles it gets that server's own answer (SEC-12). Postgres, the collector and the rest of the stack answer only on the deployment's private networks. Prometheus, Loki, Tempo and Alertmanager, which ask no credential, answer on one that only the collector and Grafana join beside them: the replicas, the proxy and the API's one-shot subcommands, which send the collector their telemetry on others, reach none of the four.
- Why: the database holds everything, and the dashboards, logs and traces show how the system is used (D26, D42). Grafana's settings are the deployment's own, not its image's defaults (D185), and so is its administrator (D191). A replica that someone else controls reads and changes no stored log, trace, metric or silence (D193).
- Check: `ci: e2e`, plus `review`, each for what its item below says.
  - `ci: e2e`: what the server publishes, that a request for another host gets no answer, over HTTPS and in plain HTTP, what Grafana answers without a sign-in, the administrator's sign-in and `admin`'s refusal, the deploy's refusal of the name `admin`, and of a password that is missing or shorter than 15 characters, the cookies that its sign-in sets, the session's for 30 days, the networks that Prometheus, Loki, Tempo, Alertmanager, the collector, Grafana, the proxy and the replicas join, and that Prometheus answers on none of the proxy's and the replicas'.
  - `review`: the lifetime of a session without a request, Grafana's key, the origin of its WebSocket, and the networks of the one-shot subcommands.

### SEC-20 A value is decoded once, and encoded where it is written
An input is decoded once, by the reader of the format it arrives in, such as a JSON body's, a query's or a `filename*`'s, only where its media type, its header or its parameter says that it is so encoded, and before its domain type checks it (API-10). No code decodes it again. A value is kept as it was given, once its domain type has normalized it, and is encoded only where it is written into another format, by that format's own writer:
- JSON that holds a value is written by `serde_json`, from typed values, never put together as text. The one exception is the proxy's own problem, into which Caddy writes the trace ID that it made, of hexadecimal digits (OPS-05).
- An SQL statement's values are bound parameters (DATA-14).
- An HTML email's values are escaped by its template, and an email's headers are encoded by the mail library ([platform spec](../spec/platform.md#sending-email)).
- A user's value in a URL is percent-encoded for its place, as the address in an authenticator app's `otpauth://` URI is (AUTH-055).
- A download's name is written into `Content-Disposition` as RFC 6266 says (PLATFORM-081).

The API starts no process, and its image holds no shell (OPS-13). A task that sends a command to a server's shell sends each value that its operator gave as one quoted word, or as a release's version, which names one of the repository's tags. No code evaluates as code, or as an expression, text that an input holds, and a format string is a literal, whose values are its arguments.
The Extensible Markup Language (XML) that the API reads is the pages of the files bucket's two listings, of its objects and of what it keeps of them: `rusty-s3` reads the first and `Storage` the second, each with `instant-xml`'s parser, which refuses a document type, and with it every entity of a document's own (PLATFORM-101, PLATFORM-102). <!-- part: files -->
- Why: a value that is decoded twice, or encoded anywhere but where it is written, is how injection and double encoding come about (D25, D26).
- Check: `compiler` (a format string of Rust is a literal), plus `test` (the encodings that the behaviors above state, such as AUTH-055's and PLATFORM-081's, and an HTML part's escaping, which AUTH-091's test shows, and the word that a task sends a server's shell, OPS-15's and OPS-14's), plus `review`.

## Supply chain

### SEC-15 Advisories block the build
`cargo-deny` fails CI on any RustSec advisory affecting a dependency. Image scans fail CI on fixable critical or high vulnerabilities, in the image's operating system packages and in the crates its binary records: the scan of the image each pipeline run builds, and a weekly scan of the published `edge`, the newest final release and the newest prerelease. A weekly job lists the fixable critical or high vulnerabilities of each third-party image that the reference deployment's Compose files pin, and fails on none of them: the deployment runs each as its maker built it, so a fix comes with the release of the image that its pin moves to (OPS-11, D200).
- Why: known vulnerabilities never ship in what this repository builds (D21), and a product sees those of the images that run beside it, to which it applies the time frames it sets itself (D183).
- Check: `ci: deny`, plus `ci: image-scan` and `ci: image-rescan`, plus `ci: third-party-scan` (the list, which fails on no vulnerability).

### SEC-16 Only vetted cryptography
Cryptography comes from the RustCrypto crates (`aes-gcm`, `hmac`, `sha2`, `argon2`) and from `rustls` with its `ring` provider, which speaks TLS 1.2 and 1.3 alone, whose cipher suites each agree on a key by an ephemeral elliptic curve exchange and encrypt with authentication, and which checks a certificate's signature by the Elliptic Curve Digital Signature Algorithm (ECDSA) on the curve P-256 or P-384, by Ed25519, or by Rivest-Shamir-Adleman (RSA) under a key of 2,048 to 8,192 bits, each over a hash of the SHA-2 family. This codebase builds no primitive or construction of its own. A hash that stands in for a token or a code, or that authenticates a value, is SHA-256 or HMAC-SHA-256, kept whole (SEC-03), but for a password's, which is Argon2id (SEC-04), an authenticator app's code, below, and the two digests by which the page of development's API reference names the files of Swagger UI that it loads, which are SHA-384 (PLATFORM-052). Where the code keeps only the first bytes of a hash, they name a thing and authenticate nothing, as those in the name of a device token's cookie do (AUTH-015). `totp-rs` builds the one-time passwords of RFC 6238 from RustCrypto's `hmac` and `sha1`, for authenticator apps (AUTH-057). Crates that implement the Postgres protocol may build its standard constructions from those crates' primitives: the database driver's SCRAM-SHA-256 (Salted Challenge Response Authentication Mechanism) sign-in, and the password verifier that `bootstrap` computes with `postgres-protocol`, of 4,096 iterations (PLATFORM-002).
`rusty-s3` builds the signature of each request to the files bucket, Signature Version 4 of Amazon Web Services (AWS), which S3's protocol asks for, from RustCrypto's `hmac` and `sha2` (PLATFORM-094). <!-- part: files -->
- Why: cryptography is easy to get subtly wrong, so it comes from vetted crates, and a protocol's standard construction is safer in its driver than rebuilt here. The `ring` provider builds on every supported platform without extra toolchains.
- Check: `ci: deny` (bans `openssl`, `native-tls` and `aws-lc-rs`), plus `review`.

## Disclosure

### SEC-17 Vulnerabilities have a reporting path
`SECURITY.md` states how to report a vulnerability and what response to expect: the template is maintained on a best-effort basis, and promises no time for an answer or for a fix (D183). Each security fix is marked in `CHANGELOG.md` (DOC-11). A product built on a fork replaces the file with its own policy.
- Why: forks and users learn about fixes they need to port.
- Check: `review`.

## Privacy

### SEC-18 Audit entries hold no plain personal data, except IP address and user agent
The values a module records in an audit entry are IDs and non-personal fields. An email appears only as a keyed hash. IP address and user agent are stored, but a workspace's log shows them only to a reader who holds the permission `access` defines for them, and a user reads them only of the entries that were theirs, in their own read and in their export (D2, D6, D166, AUDIT-010). Erasing an account anonymizes its entries (D31).
- Why: the audit log is kept longer than most data and read by more people (D6, D7).
- Check: `compiler` (an entry holds only the type of details that its action declares, in its module's `audit.rs`, AUDIT-014), `ci: check-test-map` (each action that a module's `audit.rs` declares is named in a string of the tests' files, as [the check's row](../operations/ci.md#checks-that-exist) says), plus `review` (the test of the behavior that records an action asserts the values that its entry stores, and what each type of details holds).

### SEC-19 Events reach only authorized streams
A user event reaches only that user's streams. A workspace event reaches a stream only if its principal holds, in that workspace, the permission that the event requires. Replay delivers an event only to members whose membership already existed when the event was created. An event's frame carries the trace ID of the request that published it only on a stream of that request's user.
- Why: events never reveal what the REST permission checks would withhold, new members don't receive history from before they joined (D2, D5), and no frame gives a user the trace ID of another's request, by which a member could tell that user's events together, and pass events of their own for the echo of that user's requests (D201).
- Check: `test` (PLATFORM-029, with events that only the tests publish, proves how user and workspace events reach streams, and PLATFORM-095 which streams read an event's trace ID; and the two items below), plus `ci: check-test-map` (each workspace event that a module's `events.rs` declares is named in a test whose name starts with `sec_19_`), plus `review` (what that test does with it).
  - Each workspace event a module declares is tested against a member with its permission, a member without it while some role lacks it, and a member who joined after the event.
  - Each user event a module declares is tested to name the user it is for, as the test of the behavior that publishes it reads its row.
