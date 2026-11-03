# The tasks a fork runs on itself: `just remove`, which removes optional parts, and which
# `check-removable` runs on each copy. The justfile imports this file.

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
