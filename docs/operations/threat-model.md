# Threat model

What the API and its reference deployment protect, from whom, and which rules and behaviors stop each threat. The bar is Level 2 of version 5.0.0 of the Application Security Verification Standard (ASVS), but for its requirement 6.3.3, as [the security rules](../standards/security.md) say (D26, D63). Each mitigation is stated in its home, a rule or a behavior, which this file cites by ID rather than restating it (DOC-14), and a change that adds a threat, or changes what stops one, changes this file in the same commit. Parts not built yet, such as uploaded files and second factors, are listed with the rules that already bind them, and arrive with step 5 of [the migration](../MIGRATION.md#build-order).

## What it protects

- **Accounts and sessions:** password hashes, and the tokens of sessions, which act as their user.
- **Workspaces:** what each holds, who belongs to it, and with which role.
- **Personal data:** email addresses and names, and the client IP addresses and user agents that audit entries record (SEC-16).
- **The audit logs:** who did what, which an attacker would want to change or erase.
- **Secrets:** the key of every keyed hash, such as a rate-limit bucket's key (PLATFORM-051), the database roles' passwords, the password of the mail server, reached over the Simple Mail Transfer Protocol (SMTP), the backups' encryption passphrase, the bucket's key pair, and Grafana's credentials (SEC-01).
- **The backups**, which hold the database, and so all of the above but the secrets, in a bucket off the server ([what is backed up](backups.md#what-is-backed-up)).
- **The deployment's server**, its Postgres, and its observability stack.

## Trust boundaries

| Boundary | What crosses it | What guards it |
|---|---|---|
| The internet to Caddy | Every client's requests, and Grafana's viewers | HTTPS (OPS-20); only the API and a Grafana that signs viewers in are exposed (SEC-18); security headers (SEC-11) |
| Caddy to the replicas | The requests, with the client's address in `X-Forwarded-For` | The client IP is read only from a configured trusted proxy (SEC-12, PLATFORM-047) |
| A client to the API | Credentials, tokens, and every request's input | Authentication (SEC-06, SEC-19, API-14), authorization (SEC-07, SEC-08), strict input (API-08, API-09, API-10), rate limits (SEC-12) and load shedding (OPS-07) |
| The API to Postgres | Every query, as the role `app` | Two roles, the API's with least privilege (DATA-12, DATA-13, DATA-19); queries checked at compile time (DATA-20); every query filters by tenant (DATA-22) |
| The API to other systems | Email, and later OAuth providers | One of two outbound clients (SEC-09); TLS that verifies the certificate (OPS-20) |
| Postgres to the bucket | The write-ahead log and the base backups | Encrypted before they leave the server (OPS-20); a key pair that reaches only the bucket ([the bucket](backups.md#the-bucket)) |
| The collector to an observability backend | Logs, traces and metrics | No personal data or secret in telemetry (OBS-04); TLS (OPS-20) |
| CI to the registry to the server | The images, and the deploy's tools | Everything pinned (OPS-13); one build whose digest is tested, signed and promoted (OPS-14); advisories block the build (SEC-13); a minimal, unprivileged image (OPS-15) |
| The operator to the server | Deploys, backups, restores and settings | Secure Shell (SSH) from the developer's machine (OPS-22); secrets in files, one per secret ([the files on the server](deploy.md#the-files-on-the-server)), held so that nothing prints them (SEC-01) |

## Threats

### Authentication and sessions

| Threat | What stops it |
|---|---|
| Guessing one account's password online | The sign-in limits ([below](#sign-in-limits)), and a slow hash (SEC-04, AUTH-014) |
| Credential stuffing: one guess each at many accounts, from leaked lists | The IP limit (SEC-12, PLATFORM-048), as [below](#sign-in-limits) says; a password on the breached-password list is refused when set (AUTH-003) |
| Locking an account's owner out by failing their sign-ins | The sign-in limits, and the device tokens of the devices the owner signed in on (AUTH-019, [below](#sign-in-limits)) |
| Learning which emails have accounts | One answer, after the same hashing work, for an unknown email, an account without a password and a wrong password (SEC-05, AUTH-005); registration's answer is the exception SEC-05 accepts, under the IP limit |
| Cracking a stolen password hash | Argon2id at the parameters that the Open Worldwide Application Security Project (OWASP) recommends, rehashed when they rise (SEC-04, AUTH-014) |
| Using a stolen session token | Tokens are random and stored only as hashes (SEC-03, AUTH-007); an access token is short-lived, and every session has an absolute lifetime (SEC-06, AUTH-007); a refresh token works once, and its reuse ends the session (AUTH-012); signing out ends it at once, streams included (AUTH-013) |
| A script on a page reading a browser's tokens | Browsers get their tokens only in `HttpOnly` cookies (SEC-19, AUTH-015) |
| Another site's page acting with a browser's cookies | `SameSite=Strict` cookies, and the cross-site check of requests whose method isn't safe (SEC-19, PLATFORM-053); the headers of cross-origin resource sharing (CORS) only for listed origins (PLATFORM-052) |
| Signing in with a password alone | Accepted for users, with the controls that [Second factors](#second-factors) lists; required of superadmins (D62, D63) |

### Access control

| Threat | What stops it |
|---|---|
| Reading or changing another workspace's data | The workspace is in the path, and the route's permission in its type (API-02, API-14); only `access` authorizes (SEC-07); every query filters by tenant (DATA-22); the permission matrix is tested for every route and role, with IDs from another workspace (TEST-09) |
| A member raising their own role, or acting on a higher one | Role rank limits who may change whom (SEC-08) |
| A removed member, or an ended session, still receiving events | Access changes close the affected streams at commit, and a re-check closes what a missed signal left (SEC-06, PLATFORM-017) |
| Events reaching a member who may not see them, or from before they joined | Each event reaches only the streams whose memberships hold its permission, and had committed when it did (SEC-17) |
| Forging an audit entry's actor | The actor and scope come from the unit of work's proof, never the request (AUDIT-002) |

### Input and output

| Threat | What stops it |
|---|---|
| SQL injection | Queries are checked at compile time, and the functions that take SQL text are banned (DATA-20) |
| Malformed, unexpected or oversized input | Strict bodies and query parameters (API-08, API-09), domain types at the boundary (API-10), and body limits (PLATFORM-036, PLATFORM-055) |
| An uploaded file that runs in a browser | Uploaded content is served so that it can't (SEC-10), with the `files` module |
| Errors that leak internals | Internal errors expose nothing, and `detail` never carries internals (ERR-08, ERR-09) |
| Framing, sniffing and caching of responses | Security headers on every response (SEC-11) |
| Server-side request forgery | Outbound requests go through one of two clients, the one that reaches addresses a user gives refusing private ones (SEC-09) |

### Data protection

| Threat | What stops it |
|---|---|
| Secrets leaking through settings, logs or debug output | Secrets arrive by environment or file, and stay wrapped (SEC-01); telemetry holds no secret (OBS-04) |
| A database dump giving away secrets | Secrets at rest are encrypted with associated data (SEC-02); tokens and codes are stored as hashes (SEC-03) |
| Personal data in telemetry, audit details or rate-limit keys | No personal data in telemetry (OBS-04); audit entries hold only IP address and user agent (SEC-16); bucket keys are HMACs (PLATFORM-051) |
| Data read in transit, or from a stolen disk | TLS for every connection that leaves the server, and encryption at rest (OPS-20) |
| The bucket's provider reading the backups | They are encrypted before they leave the server (OPS-20) |
| Losing data, or a change that shouldn't have happened | Continuous backups, restores to any moment the retention covers, and a restore that another undoes (OPS-17) |
| Tampering with audit history | `app` may only insert and read audit entries (AUDIT-005), which commit with the change they record (AUDIT-001) |

### Availability

| Threat | What stops it |
|---|---|
| A flood of requests | The IP and per-user limits (SEC-12), which clients can see (API-15); a concurrency limit and timeouts (OPS-07, PLATFORM-020, PLATFORM-031) |
| A client that holds streams open, or reads them slowly | A cap of streams per user on a replica (REALTIME-006); a stream that falls behind closes (PLATFORM-072) |
| Exhausting the database's connections | The connection budget fits the server (OPS-21) |

### Supply chain and operations

| Threat | What stops it |
|---|---|
| A dependency with a known vulnerability | Advisories block the build, and the images are scanned before each release and after it (SEC-13) |
| A tampered build or image | Everything pinned (OPS-13), one build of each image, signed, whose tested digest is what ships (OPS-14), on a minimal, unprivileged image (OPS-15) |
| Cryptography that is weak or home-made | Only vetted cryptography (SEC-14) |
| A role with more privilege than its task | Two database roles, and startup refuses an unsafe one (DATA-12, DATA-13) |
| A vulnerability no one reports | A reporting path (SEC-15) |

## Sign-in limits

Requirement 6.1.1 of ASVS asks how the controls defend against credential stuffing and password guessing, how they are configured, and how they keep an attacker from locking an account's owner out. A password sign-in passes the IP limit, then the sign-in limits, which count every attempt before the account is read (SEC-12, AUTH-006). `.env.example` gives each setting below its range and its default:

- **The IP limit** of each client address (PLATFORM-048): `RATE_LIMIT_IP_REQUESTS` requests of any kind in each `RATE_LIMIT_WINDOW_SECONDS`.
- **The email from one address** (AUTH-006): `SIGN_IN_ATTEMPTS` attempts at one email from one client address in each `SIGN_IN_WINDOW_SECONDS`.
- **The email from every address** (AUTH-006): `SIGN_IN_EMAIL_ATTEMPTS` attempts at one email in the same window, which counts only what the limit of the email from one address lets through.
- **A device token** (AUTH-019): a sign-in that brings one valid for its email counts only in that token's own limit, `SIGN_IN_ATTEMPTS` in each window, and in neither of the two above (AUTH-006).

**Guessing.** An attacker at one address gets `SIGN_IN_ATTEMPTS` guesses at an account in each window. Spread over many addresses, they get at most `SIGN_IN_EMAIL_ATTEMPTS`, each costing an Argon2id hash (AUTH-014), against a password that can't be on the breached-password list (AUTH-003). A device token opens no more: one is valid for one email, so a token from the attacker's own account opens nothing of another's, and a stolen one gives its thief that token's own limit, until the owner's next sign-in on the device ends it.

**Credential stuffing.** One guess at each of many accounts passes the per-email limits, so the IP limit bounds it: an address that keeps trying is refused once it has sent its requests of the window, and a botnet's addresses are each bounded so.

**Lockout.** The two counts of an email alone can't prevent it. An attacker at one address fills only that address's count, but filling the email's shared count takes no more addresses for many accounts than for one: each address is bounded by the IP limit alone, whose window holds attempts at many accounts, so a few addresses can keep the shared counts of many accounts full, window after window. So a device that has signed in to the account before brings its device token, whose own count no attempt from elsewhere fills: the owner signs in on each device that signed in within `DEVICE_TOKEN_SECONDS` and kept its token, whatever an attack does to the email's counts, as OWASP's "device cookies" do. A browser keeps one token for each account it signs in to (AUTH-015). A session the owner already holds goes on refreshing meanwhile (AUTH-010), which no sign-in limit counts. A 429 says how long to wait (API-15).

**What remains.** An attacker with enough addresses can keep the accounts they target refusing password sign-ins from every device that holds no valid token for them: a new device, or one whose token expired, waits for a window that the attack leaves, while the owner's known devices sign in as before. The API asks no challenge that tells a person from a program, such as a CAPTCHA (Completely Automated Public Turing test to tell Computers and Humans Apart), which a client that isn't a browser can't show; a product that needs more can lower the limits, or add a second factor, which every user may (D63). And each attempt the limits let through costs the replica an Argon2id hash, so an attack on many accounts at once costs processor time, which the concurrency limit and the timeouts bound (OPS-07), at the cost of refusing other requests while it lasts.

## Second factors

Requirement 6.3.3 of ASVS asks every user to sign in with more than one factor, and asks an application that relaxes it for a documented rationale and a comprehensive set of mitigating controls.

**The rationale.** The template is the start of products whose users must be able to sign in without setting a factor up: requiring one of every user would turn away the products for which a password, or a provider's sign-in, is the expected bar. So every user may add a second factor, and every superadmin, who acts across workspaces, must (D62, D63), with the full `auth` module and `superadmins`.

**The mitigating controls,** each in its home:

- Passwords: of a length that AUTH-003 bounds, refused when breached or guessable (AUTH-003), hashed with Argon2id (SEC-04, AUTH-014).
- Guessing and lockout: the [sign-in limits](#sign-in-limits), and failures that reveal nothing (SEC-05, AUTH-005).
- Sessions: random tokens stored as hashes (SEC-03), bounded lifetimes (SEC-06, AUTH-007), a refresh token that works once and whose reuse ends the session (AUTH-012), signing out that ends it at once (AUTH-013), and browsers' tokens in cookies no script reads (SEC-19, AUTH-015).
- Detection: each failed sign-in of an existing account is recorded in its audit log (AUTH-005), with the client's address and user agent (AUDIT-003), as is the reuse of a refresh token (AUTH-012).
- Access: lost access ends open streams (SEC-06), and authorization is checked on every workspace request (SEC-07).
