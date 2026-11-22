# The tasks a fork runs on itself: `just rename`, which renames the product, and which
# `check-rename` runs on a copy, and `just new-module`, which adds a module, and which
# `check-new-module` runs on a copy. The justfile imports this file.
# part: removal
# `just keep` chooses the parts that a fork keeps, once, and `just remove` removes parts until
# then; `check-removable` runs one of them on a copy for each run of crates/xtask/parts.yaml.
# end part: removal

# Renames the product, such as `just rename "Acme Notes"` (ARCH-15): it replaces the product's name
# and its slug, its words in lowercase with a hyphen between two, such as `acme-notes`, which
# names the binary and the service in telemetry, with the same forms of the new name in every file
# that holds them, and in .env, then formats the code. It changes nothing while a file would still
# hold the old name in another form, or a file already holds the new one.
# docs/guides/starting-a-product.md says what comes before and after.
[doc("Renames the product in every file that holds its name")]
[group('fork')]
rename name:
    {{ xtask }} rename {{ quote(name) }}

# part: removal
# Keeps the optional parts named, with which a fork chooses its parts once, before its first
# release (ARCH-07, architecture.md §5): it removes every other part as `just remove` does, then
# what makes a part removable, this task among it, and takes the markers out of every file it
# keeps. It refuses a choice that keeps no part that creates workspaces. Commit what it leaves once
# `just ci` passes.
[doc("Keeps the optional parts named, and removes the others and what makes a part removable")]
[group('fork')]
keep +parts: bound-build
    {{ xtask }} keep {{ parts }}

# Removes the optional parts named, and keeps what makes the others removable: it deletes the files
# and the marked lines of each, outside Rust with a blank line where ARCH-07 says, writing "None."
# in each section of a spec that it empties, then the cached queries that no kept code holds,
# updates Cargo.lock, writes openapi.json and the metric catalog again, and formats the code, then
# fails while a kept line or path still names one.
[doc("Removes optional parts, and keeps what makes the others removable")]
[group('fork')]
remove +parts: bound-build
    {{ xtask }} remove {{ parts }}
# end part: removal

# Writes a new module, such as `just new-module order_items`, that holds one workspace resource, by
# default the singular of its name, such as `order_item`, or the one named after it, as in `just
# new-module people person` (ARCH-03): its files, migration, permissions, spec and tests, with its
# registration in the files every module shares, then the query cache, from a database of its own,
# and openapi.json, and it formats the code. A step that fails undoes what it wrote.
# docs/guides/adding-a-module.md says what comes next.
[doc("Writes a new module that holds one workspace resource")]
[group('fork')]
new-module module *resource: bound-build services
    {{ xtask }} new-module {{ module }} {{ resource }}
