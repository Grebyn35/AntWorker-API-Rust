# The tasks a fork runs on itself: `just remove`, which removes optional parts, and which
# `check-removable` runs on each copy, and `just new-module`, which adds a module, and which
# `check-new-module` runs on a copy. The justfile imports this file.

# Removes the optional parts named, as a fork does before its first release to choose its
# workspace shape (ARCH-07, architecture.md §5): it deletes the files and the marked lines of
# each, writing "None." in each section of a spec that it empties, then the cached queries that no
# kept code holds, updates Cargo.lock, writes openapi.json and the metric catalog again, and
# formats the code, then fails while a kept line or path still names one. Commit what it leaves
# once `just ci` passes.
[doc("Removes optional parts, to choose a workspace shape")]
[group('fork')]
remove +parts:
    {{ xtask }} remove {{ parts }}

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
