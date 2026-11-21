# Starting a product

How a repository made from the template becomes a product of its own: its name, the optional parts it keeps, and the files in which the template speaks for itself. Each step leaves a repository that passes the checks. A product removes parts before its first release, since removing one deletes its migrations, which a release never changes (DATA-09).

## The repository

GitHub's "Use this template", on the template repository's page, makes a repository with the template's files and none of its history (D29). Its image takes the new repository's name ([ARCH-15](../standards/architecture.md#arch-15-the-product-name-is-renamable)). The checks that a pull request needs before it merges aren't among what it takes: [ci.md](../operations/ci.md#checks-required-for-merging) lists them, and says how to require them.

Clone it, then run `just setup`, as the [README](../../README.md#quickstart) says.

## Naming the product

```
just rename "Acme Notes"
```

A name is words of ASCII letters and digits, with one space between two, the first starting with a capital letter. The task replaces the product's name, as the emails show it, and its slug, `acme-notes`, which names the binary, `acme-notes-api`, and the service in telemetry, in every file that holds them, `.env` among them, then formats the code. It changes nothing while a file would still hold the old name in another form, such as in capitals, or a file already holds the new one, and names each such line. [ARCH-15](../standards/architecture.md#arch-15-the-product-name-is-renamable) says where each form goes.

It leaves the text of `README.md` and `SECURITY.md` as the template wrote it, beside the new name: [the files the template speaks in](#the-files-the-template-speaks-in) replaces them. A server's settings are its operator's, outside the repository: the sender of its emails, `EMAIL_FROM`, keeps the name it was given ([deploy.md](../operations/deploy.md#email)).

Run `just checks`, then commit.

## Choosing the parts

A product uses workspaces in one of the shapes of [architecture §5](../architecture.md#workspace-shapes), and the optional parts that it keeps decide which. [ARCH-07](../standards/architecture.md#arch-07-optional-parts-are-removable) lists the optional parts, and the runs of `crates/xtask/parts.yaml` name the parts that each shape removes. Remove those that the product doesn't keep, naming each: `just remove <part>...`.

The task deletes each part's files and the lines its markers mark, rewrites what the parts leave, such as `openapi.json` and the query cache, then fails while a line or a path that it kept still names a part it removed, as `just remove`'s description in `just/fork.just` says. Run `just ci`, then commit.

## The files the template speaks in

- `SECURITY.md` is the template's disclosure policy. The product replaces it with its own: where its users report a vulnerability, and how soon it answers a report, fixes a vulnerability and updates a component that has one.
- `README.md` says what the template is. The product says what it is, and keeps the quickstart.

## Next

[The guide to adding a module](adding-a-module.md) adds the product's first module, and [deploy.md](../operations/deploy.md) sets up a server for it.
