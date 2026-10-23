# Spec: `system`

## Purpose

`system` tells clients what the running deployment offers: the capabilities endpoint lists the features it has turned on (D9), and the schema endpoints list the permissions and the audit actions. The request pipeline's spec adds the capabilities endpoint's route and behaviors to this spec. The schema endpoints arrive with the modules whose catalogs they list.

## Owns

No tables.

## Interface

None. No other module uses `system`.

## Behaviors

None yet. The request pipeline's spec writes them.

## Design

### Capabilities

- The endpoint lists the features a deployment can turn off (OPS-04). Email is the first such feature, so the list stays empty until `mail` is built.
- The endpoint reports no build version, because the Application Security Verification Standard (ASVS) of the Open Worldwide Application Security Project (OWASP) asks that responses not reveal the versions of the API or of its components.

## Open design questions

None.
