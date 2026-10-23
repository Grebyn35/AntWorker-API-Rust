# Spec: `system`

## Purpose

`system` tells clients what the running deployment offers: the capabilities endpoint lists the features a deployment can turn off, and whether each one is on (D9). The schema endpoints, which list the permissions and the audit actions, arrive with the modules whose catalogs they list.

## Owns

No tables.

## Interface

None. No other module uses `system`.

## Routes

| Method and path | Access | Behaviors |
|---|---|---|
| `GET /v1/capabilities` | `public` | SYSTEM-001 |

## Behaviors

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| SYSTEM-001 | `GET /v1/capabilities` answers 200 with a `features` object that holds one boolean per feature a deployment can turn off, `true` while the feature is on. | OPS-04, D9 | `system_001_` |

## Design

### Capabilities

- A feature is a member of `features`, so a release that adds a feature adds a field, which API-18 allows. A client tells a feature that is off, `false`, from one its API doesn't have, which is absent. A list of the features that are on couldn't tell those apart, and adding a value to a closed set of names would change the contract.
- Email is the first feature a deployment can turn off (MAIL-004), so `features` stays empty until `mail` is built.
- The endpoint reports no build version, because the Application Security Verification Standard (ASVS) of the Open Worldwide Application Security Project (OWASP) asks that responses not reveal the versions of the API or of its components.

## Open design questions

None.
