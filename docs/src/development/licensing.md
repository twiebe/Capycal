# Licensing

Capycal is licensed under the GPL-3.0-only (`LICENSE`), while contributions come in under the MIT License (`CONTRIBUTING.md`, confirmed in the pull request template). This page explains why, so the setup is not changed by accident.

## Why the GPL

App store terms (Apple's in particular) are widely read as incompatible with the GPL; the FSF holds this view, and VLC was removed from the App Store over it in 2011. So nobody but the copyright holder can ship a copy of Capycal to an app store. The maintainer, who owns the copyright to their own code, can. No court has decided this, so it is a practical protection, not a legal guarantee; clones have to be found and reported to the store.

## Why contributions are MIT

A contributor keeps the copyright to their code and only licenses it. Code contributed under the GPL could not go into an app store build without breaking the contributor's license: they could demand it be taken down and claim damages. Under the MIT License the maintainer may ship it anywhere, including app stores and a later change of the project's license.

Under German copyright law an author has a claim to fair remuneration (§ 32 UrhG) and, for a big success, to more afterwards (§ 32a UrhG). Both claims do not exist when the author grants a simple right of use to everyone free of charge (§ 32 Abs. 3 Satz 3, § 32a Abs. 3 Satz 3, the "Linux clause"). The MIT License is such a grant, so a paid build owes contributors nothing.

| How a contribution comes in | Remuneration claim | In an app store build | Later relicensing |
|---|---|---|---|
| GPL (a pull request merged as is) | no | no | no |
| MIT (the project's way) | no | yes | yes |
| A CLA granting rights to the maintainer only | possible | yes | yes |

BSD, ISC, 0BSD and Apache-2.0 would work like MIT here, they also grant rights to everyone for free. They differ only in their duties (notices, Apache's NOTICE file and patent grant). MIT keeps it simplest.

## Rules that follow

- Every contribution needs the MIT confirmation of the pull request template and a line in `CONTRIBUTORS.md`, whose notices every package ships.
- No CLA that grants rights only to the maintainer: it would bring back the remuneration claim.
- Contributed portions can be reused by anyone under MIT. That is fine as long as most of Capycal is the maintainer's GPL code; a clone of the app still needs it.
- No GPL or LGPL crate may be compiled into the binaries, or the app store build breaks. `about.toml` lists the accepted licenses and `make check-notices` (run by CI) rejects any other; decide before adding one. GTK and libadwaita are system libraries of the Linux app only.

This is a reading of the law, not legal advice; have the wording of `CONTRIBUTING.md` checked before the first store release.
