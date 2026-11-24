# Clusters and signing in

Skiff has no sign-in of its own. It reads your kubeconfig and connects the
way `kubectl` does: with the certificates, tokens, credential plugins and
OIDC settings found there. If `kubectl get pods` works in a terminal,
Skiff should work with the same cluster. Which files are read is described
in [configuration.md](configuration.md#which-kubeconfig-files-are-read).

- [Cloud clusters and single sign-on](#cloud-clusters-and-single-sign-on)
- [Troubleshooting a connection](#troubleshooting-a-connection)
- [Proxies](#proxies)
- [Managing clusters](#managing-clusters)
- [Read-only clusters](#read-only-clusters)
- [Accounts limited to a namespace](#accounts-limited-to-a-namespace)
- [When the network misbehaves](#when-the-network-misbehaves)

## Cloud clusters and single sign-on

A kubeconfig entry for a managed cluster does not hold a password. It
names a program, the *credential plugin*, that prints a short-lived token
when asked. Skiff runs that program exactly as `kubectl` does and never
sees your cloud password. You put the cluster into your kubeconfig once,
with the cloud's own command line:

| Cluster | Getting it into your kubeconfig | Plugin that must be installed |
| --- | --- | --- |
| Amazon EKS | `aws eks update-kubeconfig --name <cluster> --region <region>` | `aws` (the AWS CLI) |
| Google GKE | `gcloud container clusters get-credentials <cluster> --region <region>` | `gke-gcloud-auth-plugin` |
| Azure AKS | `az aks get-credentials --resource-group <group> --name <cluster>` | `kubelogin`, for clusters using Entra ID |
| OIDC (Dex, Keycloak, Okta, …) | a `user` with an `exec` plugin such as `kubectl oidc-login`, or an `auth-provider: oidc` entry | the plugin your entry names |
| Teleport, Tailscale and the like | their own login command (`tsh kube login`, …) | theirs |

These commands belong to the tools named, not to Skiff; their
documentation is the reference. After running one, switch back to Skiff:
it notices that the kubeconfig changed and lists the new cluster.

Things worth knowing:

- **PATH.** Started from the Dock, the Finder or a Linux desktop launcher,
  an app does not get your shell's `PATH`, where these plugins usually
  are. Skiff asks your login shell once at startup for `PATH`,
  `KUBECONFIG` and the proxy variables (only those, and each only if it is
  not already set). Nothing else from the shell's environment is taken
  over, so a variable such as `AWS_PROFILE` that you export in your shell
  profile is not seen: put it into the kubeconfig entry's `env` list
  instead (`aws eks update-kubeconfig --profile <name>` does that), or
  start Skiff from a terminal. On Windows the environment is the same
  however Skiff is started.
- **A plugin that needs you** (one that opens a browser for a first OIDC
  login, for example) is started by Skiff as kubectl would start it. While
  Skiff waits it shows
  `Waiting for <plugin>…`. A plugin that has not answered after 60 seconds
  is given up on, with the advice to run your login command in a terminal.
- **Tokens expire.** When a plugin reports an expired session, Skiff shows
  the last lines of what the plugin said and, where it recognises it, the
  command to run (`aws sso login`, `gcloud auth login`, `az login`, …).
  Run it in a terminal, then choose **Retry**.

## Troubleshooting a connection

When a cluster cannot be reached, Skiff shows a page with the cluster's
address, what went wrong, a hint, **Retry**, and **Copy details** (the
cluster, its address, the kubeconfig file, the plugin and the error, to
share or search for).

| What you see | What it means | What to do |
| --- | --- | --- |
| Credential plugin `…` was not found | The program named in the kubeconfig's `exec` entry is not installed or not on `PATH`. Skiff also shows the kubeconfig's own install hint if it has one. | Install it. If it works in a terminal but not in Skiff, your shell sets `PATH` somewhere a login shell does not read; start Skiff from that terminal to confirm. |
| `…` failed (…), followed by the plugin's last lines | The plugin ran and reported an error, most often an expired login. | Run the login command shown, then Retry. |
| No answer from `…` within 60 seconds | The plugin is waiting, usually for a sign-in in a browser. | Run your login command in a terminal, then Retry. |
| The cluster rejected your credentials (401) | The API server refused the token or certificate. | Run your login command again, then Retry; check the `user` of this context. |
| … (403), "does not have permission" | You are signed in; the account may not do this. | Ask a cluster administrator. See also [namespace-limited accounts](#accounts-limited-to-a-namespace). |
| The cluster address could not be found (DNS), or nothing is answering at it | Network: VPN off, cluster stopped, wrong address. | Connect the VPN, check the `server` in your kubeconfig, Retry. |
| Secure connection failed: … | The server's certificate was not accepted: it does not match the kubeconfig's certificate authority, has expired, or a proxy or VPN intercepts the connection. | Check your computer's clock; refresh the kubeconfig entry with your cloud's command. |
| Context … is not in your kubeconfig | The context was renamed or removed while Skiff was open. | Reload the kubeconfig and pick a cluster. |

A kubeconfig file that cannot be parsed is listed by name with the reason
in the cluster menu, on the welcome page and in Settings → Clusters; the
other files still work.

A context whose cluster entry sets `insecure-skip-tls-verify: true` gets a
**TLS not verified** badge: Skiff connects, as kubectl would, but the
server's identity is not checked.

## Proxies

- A `proxy-url` on the cluster entry in your kubeconfig is used for that
  cluster.
- Otherwise `HTTPS_PROXY` (or `https_proxy`) is used, unless `NO_PROXY`
  exempts the API server's address.
- Started from a launcher, Skiff takes the proxy variables from your login
  shell if they are not set.
- HTTP proxies are supported; SOCKS proxies are not in this build.
- The update check is made by the system's `curl`, which follows the same
  variables.

Prometheus needs no proxy settings of its own: it is reached through the
API server.

## Managing clusters

Settings → Clusters lists every context of your kubeconfig files with its
connection state, and for each one: open it, reconnect (which also picks
up new credentials), disconnect, and **hide** it. A hidden cluster is left
out of the cluster menu and the command palette (`"hidden": true` under
`clusters.<context>` in `config.json`); the kubeconfig is not changed, and
"Manage clusters…" at the bottom of the cluster menu says how many are
hidden. Kubeconfig files found in `~/.kube` that Skiff does not read yet
are offered next to "Add file…".

## Read-only clusters

Two switches per cluster, in the cluster menu (top left) under
**Protection**, in Settings → Clusters, and in `config.json`:

- **Read-only.** Skiff refuses every change to that cluster before any
  request is sent: no edit, apply, create, delete, scale, restart, cordon,
  suspend, no new shell, and no new port-forward. The actions are disabled and say
  why, and a lock badge shows in the top bar. Looking, logs, metrics and
  YAML viewing work as usual. The palette has `Make <cluster> read-only`
  and `Allow changes to <cluster>`.
- **Ask before changes.** Restart, scale, cordon, suspend and applying
  YAML ask for confirmation first, naming the cluster. (Deleting always
  asks.)

```json
{ "clusters": { "production": { "read_only": true } } }
```

This is a guard in Skiff against a slip of the hand, for example on a
production cluster. It is not access control: the same kubeconfig still
allows changes with `kubectl`, and anyone who can edit your `config.json`
can turn it off. For real limits use an account with read-only RBAC.
Shells and port-forwards that were already open when you switch a cluster
to read-only keep running until you close them, and a manifest that is
being applied at that moment is applied to the end. If `config.json`
cannot be read as JSON, every cluster is treated as read-only until it is
fixed.

## Accounts limited to a namespace

An account that may only see one namespace cannot list resources across
the cluster, so "All namespaces" shows nothing for it.

- Select your namespace with the picker at the top, or start with
  `skiff -n <namespace>`. Tables then work. Selecting several namespaces
  at once still needs cluster-wide list rights.
- The overview says "Limited access" and offers **Use namespace …**; it
  then shows that namespace. Figures the account may not read (nodes, for
  instance) show "no access".
- If the account may list but not watch, tables fall back to polling
  every ten seconds and say so.
- Prometheus is searched for in a few well-known namespaces and the ones
  you have selected when Services cannot be listed cluster-wide; see
  [observability.md](observability.md#how-prometheus-is-found).

## When the network misbehaves

- A watch that goes silent is treated as dead after 150 seconds and
  re-established; a request that gets no answer is abandoned after 15
  seconds and retried with backoff. The table says so (`reconnecting`,
  `polling`) instead of showing stale rows as live.
- If something between you and the API server cuts watches short, the
  table falls back to polling every ten seconds and tries watching again
  later.
- A proxy or load balancer that drops idle connections is not mistaken
  for that: a quiet table keeps its watch, and an occasional list confirms
  that nothing was missed.
- Following logs survives dropped connections and container restarts
  without losing or repeating lines. One known gap: while a node's clock
  is being corrected, lines can be missed after a backwards step, or shown
  twice after a jump ahead that is later undone.
- Closing a log tab, a shell or a port-forward closes its connection on
  the server too.
