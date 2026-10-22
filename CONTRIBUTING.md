# Contributing to Capycal

Thanks for helping. Bug reports and ideas go into [issues](https://github.com/twiebe/capycal/issues). For code, see [Building and testing](https://capycal.app/docs/development/building.html) and [`CLAUDE.md`](CLAUDE.md) for the conventions; `make lint` and `cargo test` should pass before you open a pull request.

## Licensing of contributions

Capycal is distributed under the GNU General Public License v3.0 only. To let the maintainer also ship builds through app stores whose terms conflict with the GPL, every contribution is licensed by its author to the public under the [MIT License](https://opensource.org/license/mit) (SPDX: `MIT`), not under the GPL.

The GPL keeps copies of Capycal out of app stores; contributions under MIT still let the maintainer ship there. Since the MIT License grants rights to everyone free of charge, contributors have no claim to a share of a paid build (the "Linux clause" of German copyright law, § 32 Abs. 3 UrhG). The full reasoning is in [Licensing](https://capycal.app/docs/development/licensing.html).

By opening a pull request you confirm that:

- you wrote the contribution yourself, or have the right to submit it under the MIT License,
- code in it taken from other projects is MIT, BSD, ISC, 0BSD or CC0 licensed and keeps its notice in `CONTRIBUTORS.md` (for Apache-2.0 code, ask first),
- it contains no copyleft code (GPL, LGPL, AGPL, MPL, EUPL), and
- you license it under the MIT License to everyone, including the maintainer, who may distribute it as part of Capycal under the GPL or under other terms.

The confirmation covers every commit in the pull request, also commits by co-authors; whoever adds a commit confirms the same, if needed in a comment. If you write the code for an employer, make sure your employer agrees. Code you generate with an AI tool counts as your own contribution; you are responsible for it. If an AI tool wrote substantial parts, say so in the pull request description, so the review can look for copied code.

Add a line `- Copyright (c) <year> <name> <profile URL>` to [`CONTRIBUTORS.md`](CONTRIBUTORS.md) in the same pull request; this keeps the copyright notice the MIT License asks for. Pull requests without the confirmation in the pull request template are not merged.
