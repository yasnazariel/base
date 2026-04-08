# Contributing to Base

Thanks for your interest in improving Base.

This document will help you get started. **Do not let this document intimidate you**. It should be considered as a guide to help you navigate the process.

## Contributor Trust System

We use [vouch](https://github.com/mitchellh/vouch) to manage contributor trust.
Pull requests are gated behind an explicit trust list so that maintainers can
keep the review queue focused on good-faith contributions.

**What this means for you:**

- **Issues** are open to everyone — denounced users are the only exception
  (see [Denouncing policy](#denouncing-policy)).
- **Pull requests** from unvouched users must target an issue labeled
  [`M-help-wanted`][help-wanted] and are reviewed automatically by our CI
  reviewer.
- **Org collaborators** with write access bypass these checks automatically.

The trust list lives in [`.github/VOUCHED.td`](.github/VOUCHED.td).

### Vouching policy

To get vouched:

1. Find an open issue labeled [`M-help-wanted`][help-wanted] that you'd like to
   work on.
2. Open a pull request that targets that issue. Reference the issue number in
   your PR description.
3. Our CI reviewer will automatically check that the linked issue has the
   `M-help-wanted` label, then review your PR for code quality and relevance.
4. If the review passes, a maintainer will review and merge your PR, and vouch
   you for future contributions.

A maintainer can also vouch a contributor manually by commenting `vouch` on any
issue or PR authored by that contributor.

### Auto-close policy

The CI reviewer may **auto-close** a pull request if it determines the
contribution is:

- Not relevant to the linked issue or to any open issue.
- Low-effort, spammy, or not meeting the project's code quality standards.

Auto-closing is a soft rejection. You can open a new PR that addresses the
feedback.

### Denouncing policy

If a contributor's pull requests are repeatedly auto-closed, a maintainer may
**denounce** the contributor to revoke their access. Denounced users are banned
from both **issues and pull requests** — any new issues or PRs they open are
automatically closed. Denouncing is also used for:

- Deliberately disruptive behaviour (force-pushing over review comments, opening
  duplicate PRs after closure, etc.).
- Attempts to introduce malicious code, supply-chain attacks, or obfuscated
  backdoors.
- Violations of the project's code of conduct.

To denounce, a maintainer comments `denounce` or `denounce [username]` on any
issue or PR. The contributor's handle is prefixed with `-` in `.github/VOUCHED.td`
and their open PRs are auto-closed.

Denouncing is a serious action. Maintainers should leave a brief explanation in
the comment for the audit trail.

## Ways to Contribute

There are three ways an individual can contribute:

1. **By opening an issue:** If you believe you have uncovered a bug in Base or have a feature request, creating a new issue in the issue tracker is the way to begin the process.
2. **By adding context:** Provide additional context to existing issues, such as screenshots, logs, and code snippets, to help resolve them.
3. **By resolving issues:** Find an open issue labeled [`M-help-wanted`][help-wanted] and open a pull request that addresses it. This is the recommended path for new contributors — our CI reviewer will automatically review your PR and vouch you if it meets the bar.

## Scope of Contributions

To ensure we're all rowing in the same direction and to prevent wasted effort, please note the following guidelines:

### What We Accept from External Contributors

- **Small, focused changes**: One-liner fixes, typo corrections in code, small bug fixes, and similar minimal changes are welcome.
- **Issues labeled `help wanted`**: Want to contribute code? Look for unassigned issues with this label — these are ones we've specifically identified for external contributions. You can find them [here][help-wanted].
- **Bug reports**: Well-documented bug reports with reproduction steps are always appreciated.

### Before Starting Work

If you're considering a contribution (new features, refactors, architectural changes), **please open an issue first** to discuss your proposal. This helps:

- Ensure the change aligns with project goals
- Prevent duplicate work
- Get early feedback on the approach
- Save your time if the change isn't something we can accept

We want to respect your time. Opening a discussion before investing significant effort helps ensure your contribution can be merged.

## Submitting a Pull Request

### Before You Start

**Important:** Only work on issues that are assigned to you. If you're interested in an existing issue, comment on it to request assignment. We assign issues on a first-come, first-served basis. This helps prevent duplicate work and ensures your contribution can be merged.

If you want to work on something that doesn't have an issue yet, open an issue first and note that you'd like to implement it. Once we agree it's worthwhile, we'll assign the issue to you.

1. Check for existing issues or PRs that address the same problem
2. If you are assigned an issue but no longer have time to work on it, please let us know so we can unassign it

### Developer Setup

- Rust `1.93` or newer
- [`just`](https://github.com/casey/just)
- Foundry (`forge`) for Solidity-based test fixtures
- Bun and Node.js `22+` if you want to run the spec site locally
- Docker if you want to use the local devnet or build container images
- Run `just setup` to install other deps


### Making Changes

1. Fork the repository and create your branch from `main`
2. Make your changes, following the existing code style
3. Add or update tests as appropriate
4. Ensure all checks pass locally:
   ```sh
   just ci
   ```

### Opening the Pull Request

1. Link to the related issue
2. Describe what your changes do and why

### After Submitting

- Respond to feedback and requests for changes
- Keep your PR up to date with the `main` branch
- Be patient - reviews may take time

## Submitting a Bug Report

When filing a new bug report in the issue tracker, please include:

- The Base version you are on (and that it is up to date)
- Relevant logs and error messages
- Concrete steps to reproduce the bug
- Any relevant configuration

The more minimal your reproduction case, the easier it is to diagnose and fix.

## Getting Help

If you have questions:

- Open a discussion in the repository
- Comment on the relevant issue
- Check existing [documentation](https://docs.base.org/base-chain/quickstart/why-base) and issues first

[help-wanted]: https://github.com/base/base/issues?q=state%3Aopen%20label%3AM-help-wanted
