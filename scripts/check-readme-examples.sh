#!/usr/bin/env bash
# Checks that the Rust examples of README.md are those of the crate doc, which
# runs them as doctests: the same code blocks in the same order, the lines a
# doctest hides (`# ...`) left out.
#
#   scripts/check-readme-examples.sh
set -euo pipefail

readme=README.md
crate_doc=crates/mpeg2-systems/src/lib.rs

# The lines of each ```rust block, each block after a line of its own.
readme_blocks() {
  awk '
    !inside && /^```rust$/ { inside = 1; print "--- example"; next }
    inside && /^```$/      { inside = 0; next }
    inside                 { print }
  ' "$readme"
}

# The lines of each Rust code block of the `//!` doc, its hidden lines left
# out. A fence with no language is Rust, as rustdoc reads it; a block in
# another language is not compared.
doc_blocks() {
  sed -n 's|^//! \{0,1\}||p' "$crate_doc" | awk '
    !inside && /^```(rust)?$/ { inside = 1; print "--- example"; next }
    !inside && /^```/         { inside = 2; next }
    inside && /^```$/         { inside = 0; next }
    inside == 1 && !/^#( |$)/ { print }
  '
}

if ! diff -u --label "$crate_doc" --label "$readme" <(doc_blocks) <(readme_blocks); then
  echo "The Rust examples of $readme differ from those of the crate doc in $crate_doc." >&2
  exit 1
fi
