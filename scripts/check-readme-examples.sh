#!/usr/bin/env bash
# Checks that the Rust examples of README.md are those of the crate docs, which
# run them as doctests: the code blocks of mpeg2-systems, then those of
# mpeg2-systems-arib, in the same order, the lines a doctest hides (`# ...`)
# left out.
#
#   scripts/check-readme-examples.sh
set -euo pipefail
cd "$(dirname "$0")/.."

readme=README.md
crate_docs=(crates/mpeg2-systems/src/lib.rs crates/mpeg2-systems-arib/src/lib.rs)

# The lines of each ```rust block, each block after a line of its own.
readme_blocks() {
  awk '
    !inside && /^```rust(,.*)?$/ { inside = 1; print "--- example"; next }
    inside && /^```$/      { inside = 0; next }
    inside                 { print }
  ' "$readme"
}

# The lines of each Rust code block of the `//!` doc of each crate, its hidden
# lines left out. A fence with no language, or with only rustdoc's attributes,
# is Rust as rustdoc reads it; a block in another language is not compared.
doc_blocks() {
  sed -n 's|^//! \{0,1\}||p' "${crate_docs[@]}" | awk '
    !inside && /^```(rust)?(,?(ignore|no_run|should_panic|compile_fail|edition[0-9]+))*$/ { inside = 1; print "--- example"; next }
    !inside && /^```/         { inside = 2; next }
    inside && /^```$/         { inside = 0; next }
    inside == 1 && !/^#( |$)/ { print }
  '
}

if ! diff -u --label "${crate_docs[*]}" --label "$readme" <(doc_blocks) <(readme_blocks); then
  echo "The Rust examples of $readme differ from those of the crate docs in ${crate_docs[*]}." >&2
  exit 1
fi
