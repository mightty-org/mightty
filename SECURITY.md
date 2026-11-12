# Security

A terminal shows output from programs it can't trust, including ones on
machines you ssh into. Problems that let output do more than draw text
matter most: injecting input, running commands, reading the clipboard or
files, faking prompts, or stalling or crashing the terminal.

## Reporting a problem

Report it privately through
[GitHub's security advisories](https://github.com/mightty-org/mightty/security/advisories/new),
not as an issue. Include the version or commit, and the bytes or steps that
show the problem.

You'll get a reply within a week. Once a fix is out, the advisory is
published with credit to you, unless you'd rather not be named.

## Supported versions

Security fixes go into `main` and ship in the next release. Only the latest
release is supported: update to it from [mightty.org](https://mightty.org)
or the [releases page](https://github.com/mightty-org/mightty/releases).
