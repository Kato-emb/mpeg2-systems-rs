# Security policy

This library reads streams from untrusted sources, such as broadcast transport
streams. A panic, an unbounded allocation, an infinite loop, or excessive work
on crafted input is a vulnerability, and so is output that a conforming reader
would misinterpret.

## Reporting a vulnerability

Report it privately through
[GitHub's private vulnerability reporting](https://github.com/Kato-emb/mpeg2-systems-rs/security/advisories/new),
not in a public issue. Include the input that triggers it and the crate version
or commit.

A fix is released before the advisory is published, and you are credited in it
unless you ask not to be.

## Supported versions

Before 1.0, only the latest release receives fixes.
