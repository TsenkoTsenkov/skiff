## Pull requests
Once a pull request Claude opened is ready and CI is green, merge it (merge commit) without asking again.

## Releases
Merging to main is releasing. A pull request whose change users will notice runs `hack/bump-version.py` (patch unless told otherwise) and commits the result, so the merge publishes it as the next stable version. Leave the version alone only for changes that are experimental or risky enough that they should reach nightly users first, or that users won't notice (docs, CI, tests); those ship with the next bump.
