# Security policy

Skiff holds whatever access your kubeconfig gives it, so problems in it
matter. Thank you for taking the time to report one.

## Reporting a vulnerability

Please report it privately, not in a public issue or discussion:

**[Report a vulnerability](https://github.com/TsenkoTsenkov/skiff/security/advisories/new)**
(the repository's Security tab → Advisories → Report a vulnerability).

This opens a private advisory that only the maintainers and you can see.
Include what you can of:

- the Skiff version and build (`skiff --version`, or Settings → About) and
  your operating system;
- what an attacker needs (a cluster they control, a file on your machine,
  a network position) and what they gain;
- steps or a small manifest that reproduce it.

Do not include real credentials, tokens or kubeconfig contents.

Skiff is maintained by one person in their spare time. You can expect an
acknowledgement within a week, an honest assessment, and credit in the
advisory and the changelog if you want it. There is no bug bounty.

Please give a fix a reasonable time to ship before publishing details.

## Supported versions

| Version | Fixes |
| --- | --- |
| The latest stable release (`vX.Y.Z`) | yes |
| The current nightly (`build-N`) | yes, by the next nightly |
| Anything older | no: update to the latest release |

## Scope

In scope, with examples of what counts:

- **Credentials:** anything that makes Skiff reveal, log or send a
  kubeconfig credential, a token printed by a credential plugin, or a
  Secret value somewhere it should not go.
- **Hostile clusters:** an API server, object name, log line, terminal
  output or Prometheus answer that makes Skiff run a command, write a
  file, write to the clipboard, or crash.
- **The updater:** installing or running something that is not a genuine
  Skiff release, or sending a token to GitHub from an official build.
- **Launching your terminal app:** names from a cluster or kubeconfig
  being interpreted as shell syntax.
- **Read-only mode:** a way to change a cluster through Skiff while it is
  marked read-only.
- **The release pipeline and packages:** a way to get unreviewed code or
  a secret into or out of a release build.

Out of scope:

- what someone can do who can already edit your kubeconfig, your
  `config.json` or other files in your home folder, or who controls a
  credential plugin your kubeconfig runs;
- vulnerabilities in a cluster, in Prometheus or Grafana, or in your
  terminal app (report those to their projects);
- the first-launch warnings of unsigned builds, which are a known and
  documented state ([docs/install.md](docs/install.md));
- vulnerabilities in dependencies without a way to reach them through
  Skiff. A dependency advisory is still welcome as an ordinary issue; CI
  checks the advisory database weekly.

## More

What Skiff connects to, how updates are verified and how secrets are
handled: [docs/security.md](docs/security.md). How to check that a
download is genuine: [docs/install.md](docs/install.md#checking-a-download).
