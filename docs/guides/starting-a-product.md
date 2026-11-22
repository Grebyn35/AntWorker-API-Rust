# Starting a product

How a repository made from the template becomes a product of its own: its name, and the files in which the template speaks for itself. Each step leaves a repository that passes the checks.
It chooses the parts of the template that it keeps, too, before its first release, since leaving one out deletes its migrations, which a release never changes (DATA-09). <!-- part: removal -->
An ID in parentheses names what a sentence follows: a rule of the [standards](../standards/README.md), such as ARCH-15, a behavior of a [spec](../spec/README.md), such as AUTH-081, or a decision, such as D29, whose records [architecture.md](../architecture.md) says where to find. The [glossary](../architecture.md#11-glossary) defines the terms these documents share.

## The repository

GitHub's "Use this template", on the template repository's page, makes a repository with the template's files and none of its history (D29). Its image takes the new repository's name ([ARCH-15](../standards/architecture.md#arch-15-the-product-name-is-renamable)). The checks that a pull request needs before it merges aren't among what it takes: [ci.md](../operations/ci.md#checks-required-for-merging) lists them, and says how to require them.

Clone it, then run `just setup`, as the [README](../../README.md#quickstart) says.

## Naming the product

```
just rename "<name>"
```

A name is words of ASCII letters and digits, with one space between two, the first starting with a capital letter. The task replaces the product's name, as the emails show it, and its slug, such as `acme-notes` for `Acme Notes`, which names the binary, `acme-notes-api`, and the service in telemetry, in every file that holds them, `.env` among them, then formats the code. It changes nothing while a file would still hold the old name in another form, such as in capitals, or a file already holds the new one, and names each such line. [ARCH-15](../standards/architecture.md#arch-15-the-product-name-is-renamable) says where each form goes.

It leaves the text of `README.md` and `SECURITY.md` as the template wrote it, beside the new name: [the files the template speaks in](#the-files-the-template-speaks-in) replaces them. A server's settings are outside the repository, and belong to its operator, who sets the server up and runs it: there, the sender of its emails, `EMAIL_FROM`, keeps the name it was given ([deploy.md](../operations/deploy.md#email)).

Run `just checks`, then commit.

<!-- part: removal -->
## Choosing the parts

A product uses workspaces in one of the shapes of [architecture §5](../architecture.md#workspace-shapes), and the optional parts that it keeps decide which. Name every part that the product keeps: `just keep <part>...`. The task refuses a name that is no part, listing the parts, and a choice that keeps no part that creates workspaces. Each shape keeps these parts:

<!-- part: invitations -->
- Teams: `workspace_creation` and `invitations`. <!-- part: workspace_creation -->
- Provisioned organizations: `superadmins` and `invitations`. <!-- part: superadmins -->
- One organization: `workspace_setup` and `invitations`. <!-- part: workspace_setup -->
<!-- end part: invitations -->
- Personal: `workspace_creation`. <!-- part: workspace_creation -->

A product of any shape may also keep any of these, which [ARCH-07](../standards/architecture.md#arch-07-optional-parts-are-removable) describes:

- `superadmins` <!-- part: superadmins -->
- `files` <!-- part: files -->
- `client_telemetry` <!-- part: client_telemetry -->
- `github` <!-- part: github -->
- `google` <!-- part: google -->
- `openid_connect` <!-- part: openid_connect -->

The task removes every other part, as `just remove` does, then the part that makes the others removable, which ARCH-07 describes too: `crates/xtask/parts.yaml`, the tooling that reads it, its checks, and the lines that the rule gives it, this section among them. Then it takes the markers out of every file it keeps, so the product's code names the parts it kept as it names anything else, and its CI removes nothing. `just/fork.just` says what each task does. Run `just ci`, then commit.

Until then, `just remove <part>...` removes parts and keeps what makes the others removable.

<!-- end part: removal -->
## The files the template speaks in

- `SECURITY.md` is the template's disclosure policy. The product replaces it with its own: where its users report a vulnerability, and how soon it answers a report, fixes a vulnerability and updates a component that has one.
- `README.md` says what the template is. The product says what it is, and keeps the quickstart.
- `CHANGELOG.md` holds what the template's releases change. The product keeps its explanation and empties `Unreleased`, which then gathers what the product's own first release brings, since `just release` asks each of the product's final releases for its section ([release.md](../operations/release.md#before-you-release)). What a later release of the template changes, the product reads in the template's changelog, and ports.

## Next

The README's [quickstart](../../README.md#quickstart) runs the product on a developer's machine. A sign-in through a provider is off there until `.env` sets that provider's client (AUTH-081), and a client then runs it, as the [auth spec](../spec/auth.md#signing-in-through-a-provider) says. `just seed` creates the first workspaces, as the parts that the product keeps create them on a deployment, playing the operator where one is needed ([system spec](../spec/system.md#seed)).

[The guide to adding a module](adding-a-module.md) adds the product's first module, and [deploy.md](../operations/deploy.md) sets up a server for it.
