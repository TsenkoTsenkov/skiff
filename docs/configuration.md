# Configuration

Skiff works without any configuration. Most of what is here can also be
changed in the Settings window (`Ctrl+,`, or `Cmd+,` on a Mac), which
writes the same file.

- [config.json](#configjson)
- [Which kubeconfig files are read](#which-kubeconfig-files-are-read)
- [Keyboard](#keyboard) and [keys.json](#keysjson)
- [Command-line options](#command-line-options)
- [Environment variables](#environment-variables)

## config.json

| System | Location |
| --- | --- |
| Linux, macOS | `$XDG_CONFIG_HOME/skiff/config.json`, or `~/.config/skiff/config.json` |
| Windows | `%APPDATA%\skiff\config.json` |
| Anywhere | the file named by `$SKIFF_CONFIG` |

Settings → Files shows the path in use and opens the file in your editor
(creating it as `{}` if it does not exist).

Every key is optional. Loading is lenient: a mistake in one key loses only
that key, and Skiff shows a notice saying which key and what is wrong.
Keys Skiff does not know are reported and otherwise left alone. A file
that is not valid JSON is not used at all: Skiff says so, runs with the
default settings, and, because it cannot know which clusters you
protected, treats every cluster as read-only until the file is fixed. When Skiff
saves a setting it changes only that key in the file as it is on disk, so
your own edits and unknown keys survive (the keys are written in
alphabetical order). The file is re-read when the window gets the focus
back after you edited it. There are no secrets in it.

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `kubeconfigs` | list of paths | `$KUBECONFIG`, else `~/.kube/config` | Kubeconfig files to read. An entry is a file or a directory (every file in it); it may start with `~` and use `*` in its last part. When set, nothing else is read. |
| `updates.check` | true or false | `true` | Look for a new version of Skiff at startup. `SKIFF_NO_UPDATE_CHECK=1` also turns it off. |
| `updates.channel` | `"stable"` or `"nightly"` | `"stable"` | Which releases to offer: stable releases only, or every build of the main branch. |
| `terminal` | text | Skiff picks one | The terminal app for "open in terminal": a name such as `iTerm`, `Ghostty` or `kitty`, or on Linux a program path. `$SKIFF_TERMINAL` wins over it. |
| `theme` | `"dark"`, `"light"` or `"system"` | `"dark"` | The colours of the window. `"system"` follows the light or dark mode of your computer. |
| `zoom` | number, 0.5 to 3.0 | `1.0` | Size of everything in the window. Also changed by the zoom shortcuts. |
| `clusters.<context>.read_only` | true or false | `false` | Refuse every change in this cluster: no edit, delete, scale or restart, and no new shell or port-forward. Shells and port-forwards that are already open keep running until closed. |
| `clusters.<context>.confirm` | true or false | `false` | Ask before every change in this cluster, also for the ones that normally go ahead at once. |
| `clusters.<context>.prometheus.namespace` | text | found automatically | Namespace of the Prometheus Service to chart from. Set namespace and service to skip the automatic search. |
| `clusters.<context>.prometheus.service` | text | found automatically | Name of the Prometheus Service. It is reached through the API server, with your cluster credentials. |
| `clusters.<context>.prometheus.port` | number, 1 to 65535 | `9090` | Port of the Prometheus Service. |
| `clusters.<context>.prometheus.scheme` | `"http"` or `"https"` | `"http"` | What Prometheus itself speaks inside the cluster. |
| `clusters.<context>.prometheus.path_prefix` | text | `""` | Path in front of `/api/v1`, for example `/select/0/prometheus` for VictoriaMetrics. |
| `clusters.<context>.grafana_url` | http or https URL | none | Address of your Grafana, for "Open in Grafana" links. Opened in the browser; no login is stored. |

`<context>` is the name of a kubeconfig context. This table is the one in
the program (`KEYS` in `crates/skiff/src/config.rs`); the loader accepts
exactly these keys.

A complete file using every key:

```json
{
  "kubeconfigs": ["~/.kube/config", "~/.kube/conf.d/*.yaml"],
  "updates": { "check": true, "channel": "stable" },
  "terminal": "iTerm",
  "theme": "system",
  "zoom": 1.0,
  "clusters": {
    "production": {
      "read_only": true,
      "confirm": true,
      "prometheus": {
        "namespace": "monitoring",
        "service": "prometheus-operated",
        "port": 9090,
        "scheme": "http",
        "path_prefix": ""
      },
      "grafana_url": "https://grafana.example.com"
    }
  }
}
```

JSON has no comments. Notes on single keys:

- `prometheus`: a mistake in one of its fields drops the whole override,
  with a warning, and the automatic search is used. See
  [observability.md](observability.md).
- `grafana_url`: must be an `http://` or `https://` address without a user
  name or password in it.
- `read_only` and `confirm` are also in the cluster menu and in
  Settings → Clusters. See [clusters.md](clusters.md#read-only-clusters).
- `--light` on the command line overrides `theme` for that run.

Window state (pane sizes, the last view, the namespaces selected per
cluster) is not in this file. It is kept separately in the application
data folder of your system and needs no editing.

## Which kubeconfig files are read

The first of these that applies:

1. `--kubeconfig <path>` on the command line (repeat it to merge several).
2. `kubeconfigs` in `config.json`.
3. The path list in `$KUBECONFIG`.
4. `~/.kube/config` (Windows: `%USERPROFILE%\.kube\config`).

Rules:

- Several files are merged the way `kubectl` merges them: for a context,
  cluster or user with the same name, the first file wins.
- A file that does not exist is skipped. A file that cannot be parsed is
  reported by name (in the cluster menu and on the welcome page) and the
  others are still used.
- In `kubeconfigs`, an entry may be a directory, which means every file
  directly in it except hidden files and editor backups (`name~`), and
  may use `~` and `*` or `?` wildcards in its last part, for example
  `~/.kube/conf.d/*.yaml`.
- **Add kubeconfig file…** (welcome page, cluster menu, Settings →
  Clusters, the palette, or dropping a file on the window) writes
  `kubeconfigs` for you: the files Skiff reads at that moment plus the new
  one. From then on `$KUBECONFIG` is no longer followed; remove the key,
  or choose "Use kubectl's locations" in Settings → Clusters, to go back.
- The files are read again when the window gets the focus back and one of
  them has changed, and by **Reload kubeconfig** in the cluster menu or
  the palette. A cluster you added with `aws eks update-kubeconfig` shows
  up when you switch back to Skiff.
- Started from the Dock, the Finder or a Linux desktop launcher, Skiff
  takes `KUBECONFIG` from your login shell if it is not already set.

## Keyboard

Press `?` in the app for this list with your own bindings filled in.
`Cmd` replaces `Ctrl` on a Mac except where noted.

| Key | Action |
| --- | --- |
| `Ctrl+K` | Search or jump to anything: resource types (kubectl short names work: `po`, `deploy`, `svc`), loaded objects, namespaces, clusters, actions |
| `Ctrl+,` | Settings |
| `?` | Keyboard shortcuts |
| `Ctrl+=` `Ctrl+-` `Ctrl+0` | Zoom in, zoom out, actual size |
| `/` | Filter the table |
| `j` `k` or `↓` `↑` | Next / previous row or sidebar entry |
| `Ctrl+d` `Ctrl+u` or `PageDown` `PageUp` | Down / up a page (`Ctrl` on a Mac too) |
| `g` `G` or `Home` `End` | First / last row |
| `h` or `←` | Move to the sidebar |
| `l` or `→` | Back to the table, or open the selected row |
| `Enter` | Open the selected row or sidebar entry |
| `]` `[` | Next / previous tab of the details |
| `Ctrl+S` | Apply the YAML you edited |
| `Esc` | Close what is on top: a dialog, the details, the filter |
| `Enter` `Shift+Enter` | In the log find box: next / previous match |

Right-click a row for its actions.

**In a shell** every key goes to the shell, including `Esc`, `Tab`,
`Ctrl+K` and `Ctrl+S`:

| Key | Action |
| --- | --- |
| `Ctrl+Shift+C` / `Ctrl+Shift+V` (also `Ctrl+V`) | Copy the selection / paste. On a Mac `Cmd+C` / `Cmd+V` |
| `Shift+PageUp` `Shift+PageDown` | Scroll back and forth |
| `Ctrl+Shift+K` | Search or jump, from a shell (`Cmd+K` on a Mac) |

Click outside the terminal to give the keyboard back to the rest of the
app. Because `Ctrl+V` pastes, programs that use it themselves need their
other binding (in vim, `Ctrl+Q` starts a visual block). On Linux and
Windows the zoom shortcuts are not taken while a shell has the keyboard.

### keys.json

The keys for moving around can be changed. Skiff reads `keys.json` from
the same folder as `config.json` (or the file named by `$SKIFF_KEYMAP`);
Settings → Files opens it, writing the defaults first if it does not
exist. On Windows a `keys.json` at the older location
`%USERPROFILE%\.config\skiff` is still used while there is none in
`%APPDATA%\skiff`.

The file maps actions to lists of keys. Actions it leaves out keep their
defaults, and `[]` unbinds one. For example, the Colemak home row with the
arrows kept:

```json
{
  "down": ["n", "ArrowDown"],
  "up": ["e", "ArrowUp"],
  "focus_left": ["h", "ArrowLeft"],
  "focus_right": ["i", "ArrowRight"]
}
```

| Action | Default | Does |
| --- | --- | --- |
| `down` | `j`, `ArrowDown` | Next row or sidebar entry |
| `up` | `k`, `ArrowUp` | Previous row or sidebar entry |
| `page_down` | `Ctrl+d`, `PageDown` | Down a page |
| `page_up` | `Ctrl+u`, `PageUp` | Up a page |
| `top` | `g`, `Home` | First row |
| `bottom` | `G`, `End` | Last row |
| `open` | `Enter` | Open the selected row or sidebar entry |
| `focus_left` | `h`, `ArrowLeft` | Move to the sidebar |
| `focus_right` | `l`, `ArrowRight` | Back to the table, or open the selected row |
| `next_tab` | `]` | Next tab |
| `prev_tab` | `[` | Previous tab |
| `filter` | `/` | Filter the table |
| `help` | `?` | Show keyboard shortcuts |

Keys are written as `j`, `ArrowDown`, `PageUp`, `Home`, `Enter`, `Space`,
`/`, `[`, `F5` and so on, optionally after modifiers joined with `+`:
`Ctrl+d`, `Shift+g`, `Alt+j` (or `Option+j`), `Cmd+j`. In this file `Cmd`
means the Command key on a Mac and `Ctrl` on Linux and Windows. A single
capital letter includes Shift: `G` is `Shift+g`. A key binds only with
exactly the modifiers given, so `j` does not also fire on `Ctrl+j`. If the
file has mistakes, Skiff says so and uses the parts it understood.

## Command-line options

```
skiff [OPTIONS]

-c, --context <NAME>     Open this kubeconfig context (default: last used / current)
-n, --namespace <NS>     Start with only this namespace selected
    --kubeconfig <PATH>  Read clusters from this kubeconfig file instead of the
                         configured ones; repeat it to merge several files
    --view <RESOURCE>    Start on a resource view, e.g. pods or deployments.apps
    --light              Use the light theme
-h, --help               Show this help
-V, --version            Show the version
```

A value can follow its option (`--context dev`) or be attached to a long
one (`--context=dev`). `--version` prints
`skiff <version> (build <N>, <commit>, <target>)`; the build number and
commit appear only in builds made by the release workflow.

On Windows, Skiff is a windowed program: started from a terminal it
prints to that terminal, but the prompt returns before the output
appears.

## Environment variables

Read when Skiff runs:

| Variable | Effect |
| --- | --- |
| `SKIFF_CONFIG` | Path of the config file, instead of the default location. |
| `SKIFF_KEYMAP` | Path of the key bindings file, instead of `keys.json` next to the config file. |
| `SKIFF_NO_UPDATE_CHECK` | Set to anything: no automatic update check, whatever the config says. |
| `SKIFF_TERMINAL` | The terminal app for "open in terminal"; wins over `terminal` in the config. |
| `SKIFF_RENDERER` | `wgpu` or `glow`: draw with this renderer only. By default wgpu (Vulkan, Metal, DirectX) is tried first, then glow (OpenGL). |
| `SKIFF_TRACE_STARTUP` | `1` prints to stderr when the first frame was built and when the first table was fully loaded; `exit` also quits then. Used by the benchmark. |
| `KUBECONFIG` | Kubeconfig path list, as for kubectl (see above). |
| `XDG_CONFIG_HOME` | Base of the config folder on Linux and macOS. |
| `HTTPS_PROXY`, `NO_PROXY` | Proxy for reaching clusters (and, through `curl`, for the update check); see [clusters.md](clusters.md#proxies). |
| `TERMINAL` | On Linux, the terminal app to try first when none is configured. |
| `GITHUB_TOKEN`, `GH_TOKEN` | Never read by official builds. Only a build compiled for another repository uses them, see [security.md](security.md#updates). |

Fixed when Skiff is compiled (release pipeline and packaging scripts):

| Variable | Effect |
| --- | --- |
| `SKIFF_BUILD` | The build number, `git rev-list --count HEAD`. 0 when unset. |
| `SKIFF_COMMIT` | The commit shown by `--version` and in Settings → About. |
| `SKIFF_UPDATE_REPO` | `owner/repo` the update check asks instead of `TsenkoTsenkov/skiff`, for forks. |
| `SKIFF_TEAM_ID` | The Apple Team ID a signed macOS build was signed with; such a build accepts updates only from that team. |

Used by the tests and the scripts in `hack/` only:
`SKIFF_TEST_KUBECONFIG`, `SKIFF_TEST_CONTEXT`,
`SKIFF_TEST_RESTRICTED_KUBECONFIG`, `SKIFF_TEST_RESTRICTED_NAMESPACE`,
`SKIFF_PERF_CONTEXT` and `SKIFF_KUBECONFIG`; see
[development.md](development.md).
