//! "Connect Prometheus": where a cluster's metrics come from, chosen in a
//! dialog instead of written into config.json by hand.
//!
//! The dialog edits one cluster's `clusters.<context>.prometheus`. It
//! offers the common places (found automatically in the cluster, a cloud's
//! managed Prometheus, Grafana Cloud, any other address, a named Service)
//! and shows only the fields each one needs, with where to find them.
//! **Test connection** asks the Prometheus as the views will (see
//! `skiff_core::prom::test_connection`) before anything is saved.
//!
//! A view that wants the dialog calls [`request`]; the app picks that up
//! with [`take_request`] and owns the dialog from there.

use egui::{Align, Layout, RichText, Ui, Vec2};
use egui_material_icons::icons;
use skiff_core::{ConnectionTest, ExternalAuth, ExternalEndpoint, Job, PromConfig, PromEndpoint, SecretSource};

use crate::config::{PrometheusOverride, PrometheusService};
use crate::theme::{self, Palette};
use crate::widgets;

// ---------------------------------------------------------------------
// Asking for the dialog from a view

fn request_id() -> egui::Id {
    egui::Id::unique("skiff-prom-connect-request")
}

/// Asks the app to open the dialog for `context` (`None`: the cluster
/// that is open).
pub fn request(ctx: &egui::Context, context: Option<&str>) {
    let context = context.map(str::to_string);
    ctx.data_mut(|d| d.insert_temp(request_id(), context));
}

/// The request made by a view this frame, if any.
pub fn take_request(ctx: &egui::Context) -> Option<Option<String>> {
    ctx.data_mut(|d| d.remove_temp::<Option<String>>(request_id()))
}

/// The link the views show where Prometheus is missing or not working.
pub fn link(ui: &mut Ui, pal: &Palette, context: Option<&str>, label: &str) {
    if widgets::link(ui, pal, icons::ICON_TUNE, label)
        .on_hover_text("Choose where this cluster's metrics come from: in the cluster, or a managed Prometheus such as Azure Monitor")
        .clicked()
    {
        request(ui.ctx(), context);
    }
}

// ---------------------------------------------------------------------
// The choices

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    Auto,
    Azure,
    Aws,
    Gcp,
    GrafanaCloud,
    Other,
    Service,
}

impl Provider {
    const ALL: [Provider; 7] = [
        Provider::Auto,
        Provider::Azure,
        Provider::Aws,
        Provider::Gcp,
        Provider::GrafanaCloud,
        Provider::Other,
        Provider::Service,
    ];

    fn label(self) -> &'static str {
        match self {
            Provider::Auto => "Find it in the cluster (automatic)",
            Provider::Azure => "Azure Monitor managed Prometheus",
            Provider::Aws => "Amazon Managed Service for Prometheus",
            Provider::Gcp => "Google Cloud Managed Prometheus",
            Provider::GrafanaCloud => "Grafana Cloud",
            Provider::Other => "Another address (Mimir, Thanos, VictoriaMetrics, …)",
            Provider::Service => "A Service in the cluster, by name",
        }
    }

    fn url_hint(self) -> &'static str {
        match self {
            Provider::Azure => "https://<workspace>.<region>.prometheus.monitor.azure.com",
            Provider::Aws => "https://aps-workspaces.<region>.amazonaws.com/workspaces/ws-…",
            Provider::Gcp => "https://monitoring.googleapis.com/v1/projects/<project>/location/global/prometheus",
            Provider::GrafanaCloud => "https://prometheus-prod-…grafana.net/api/prom",
            _ => "https://prometheus.example.com",
        }
    }

    /// Where the address is found, and what the account needs.
    fn help(self) -> &'static str {
        match self {
            Provider::Auto => {
                "Skiff looks for a Prometheus Service in the cluster (kube-prometheus-stack, the Prometheus chart, \
                 VictoriaMetrics) and reaches it through the API server, with your cluster credentials."
            }
            Provider::Azure => {
                "Address: the Query endpoint on the Overview page of your Azure Monitor workspace. \
                 Skiff signs in with the Azure CLI, so run az login first; your account needs the \
                 Monitoring Data Reader role on the workspace."
            }
            Provider::Aws => {
                "Address: the workspace's endpoint, without /api/v1. Requests are signed with credentials \
                 from the AWS CLI (version 2); they need aps:QueryMetrics on the workspace."
            }
            Provider::Gcp => {
                "Put your project ID into the address. Skiff signs in with gcloud, so run gcloud auth login \
                 first; the account needs the Monitoring Viewer role."
            }
            Provider::GrafanaCloud => {
                "In Grafana Cloud, open your stack's Prometheus details: the address is the Query endpoint, \
                 the user is the instance ID, and the password is an access policy token with metrics:read."
            }
            Provider::Other => "The address in front of /api/v1 of any Prometheus-compatible API.",
            Provider::Service => {
                "Reached through the API server with your cluster credentials; your account needs \"get\" on \
                 services/proxy in that namespace."
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SignIn {
    None,
    Token,
    Password,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum From {
    Env,
    File,
    Command,
}

impl From {
    const ALL: [From; 3] = [From::Env, From::File, From::Command];

    fn label(self) -> &'static str {
        match self {
            From::Env => "Environment variable",
            From::File => "File",
            From::Command => "Command",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            From::Env => "PROMETHEUS_TOKEN",
            From::File => "~/.config/prometheus-token",
            From::Command => "vault read -field=token secret/prometheus",
        }
    }
}

// ---------------------------------------------------------------------
// The dialog's state

pub struct PromConnect {
    pub context: String,
    provider: Provider,
    url: String,
    label_name: String,
    label_value: String,
    tenant: String,
    region: String,
    profile: String,
    sign_in: SignIn,
    username: String,
    from: From,
    secret: String,
    org_id: String,
    namespace: String,
    service: String,
    port: String,
    https: bool,
    path_prefix: String,
    /// What is being tested, and the test.
    testing: Option<(Option<PrometheusOverride>, Job<ConnectionTest>)>,
    /// The last test's answer, for what was tested.
    tested: Option<(Option<PrometheusOverride>, Result<ConnectionTest, String>)>,
    /// Why Save did not save.
    error: Option<String>,
}

/// What the user did in the dialog this frame.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Open,
    Close,
    /// Write this to `clusters.<context>.prometheus` (`None`: remove it).
    Save(Option<PrometheusOverride>),
    /// Try this: the app starts the test and hands its job to
    /// [`PromConnect::start_test`].
    Test(PromConfig),
}

/// The cluster's own name inside a context name that tools generate:
/// EKS (`arn:aws:eks:…:cluster/<name>`), GKE (`gke_<project>_<zone>_<name>`).
/// Otherwise the context name itself, as `az aks get-credentials` makes it.
pub fn cluster_name(context: &str) -> String {
    if let Some((_, name)) = context.rsplit_once(":cluster/") {
        return name.to_string();
    }
    if let Some(rest) = context.strip_prefix("gke_") {
        if let Some((_, name)) = rest.rsplit_once('_') {
            return name.to_string();
        }
    }
    context.to_string()
}

/// A command line as typed, split into the program and its arguments:
/// on spaces, with "double" or 'single' quotes around an argument that
/// holds spaces.
pub fn split_command(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for c in line.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => current.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started {
                    out.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            (None, c) => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        out.push(current);
    }
    out
}

/// The inverse of [`split_command`], for showing a saved command.
fn join_command(argv: &[String]) -> String {
    argv.iter()
        .map(|a| if a.is_empty() || a.contains(char::is_whitespace) { format!("\"{a}\"") } else { a.clone() })
        .collect::<Vec<_>>()
        .join(" ")
}

impl PromConnect {
    /// The dialog for `context`, filled in from what the config says now.
    pub fn new(context: &str, current: Option<&PrometheusOverride>) -> Self {
        let mut d = PromConnect {
            context: context.to_string(),
            provider: Provider::Auto,
            url: String::new(),
            label_name: "cluster".into(),
            label_value: String::new(),
            tenant: String::new(),
            region: String::new(),
            profile: String::new(),
            sign_in: SignIn::None,
            username: String::new(),
            from: From::Env,
            secret: String::new(),
            org_id: String::new(),
            namespace: "monitoring".into(),
            service: String::new(),
            port: "9090".into(),
            https: false,
            path_prefix: String::new(),
            testing: None,
            tested: None,
            error: None,
        };
        match current {
            None => {}
            Some(PrometheusOverride::Service(s)) => {
                d.provider = Provider::Service;
                d.namespace = s.namespace.clone();
                d.service = s.service.clone();
                d.port = s.port.to_string();
                d.https = s.scheme == "https";
                d.path_prefix = s.path_prefix.clone();
            }
            Some(PrometheusOverride::External(e)) => {
                d.url = e.url.clone();
                if let Some((name, value)) = e.labels.first() {
                    d.label_name = name.clone();
                    d.label_value = value.clone();
                }
                d.org_id = e
                    .headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("x-scope-orgid"))
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default();
                let mut secret = |source: &SecretSource| {
                    (d.from, d.secret) = match source {
                        SecretSource::Env(name) => (From::Env, name.clone()),
                        SecretSource::File(path) => (From::File, path.to_string_lossy().into_owned()),
                        SecretSource::Command(argv) => (From::Command, join_command(argv)),
                    };
                };
                d.provider = match &e.auth {
                    ExternalAuth::Azure { tenant, .. } => {
                        d.tenant = tenant.clone().unwrap_or_default();
                        Provider::Azure
                    }
                    ExternalAuth::Aws { region, profile, .. } => {
                        d.region = region.clone().unwrap_or_default();
                        d.profile = profile.clone().unwrap_or_default();
                        Provider::Aws
                    }
                    ExternalAuth::Gcp => Provider::Gcp,
                    ExternalAuth::Basic { username, password } => {
                        secret(password);
                        d.username = username.clone();
                        d.sign_in = SignIn::Password;
                        if e.url.contains("grafana.net") {
                            Provider::GrafanaCloud
                        } else {
                            Provider::Other
                        }
                    }
                    ExternalAuth::Bearer(token) => {
                        secret(token);
                        d.sign_in = SignIn::Token;
                        Provider::Other
                    }
                    ExternalAuth::None => Provider::Other,
                };
            }
        }
        d
    }

    /// Switches to another kind of Prometheus, with sensible defaults for
    /// what it needs.
    fn choose(&mut self, provider: Provider) {
        if provider == self.provider {
            return;
        }
        self.provider = provider;
        // A managed service holds many clusters, labelled by name.
        if matches!(provider, Provider::Azure | Provider::Aws | Provider::Gcp | Provider::GrafanaCloud) && self.label_value.is_empty() {
            self.label_name = "cluster".into();
            self.label_value = cluster_name(&self.context);
        }
        if provider == Provider::GrafanaCloud {
            self.sign_in = SignIn::Password;
        }
        self.error = None;
    }

    /// What Save would write (`None`: find it automatically), or what is
    /// missing or wrong.
    pub fn build(&self) -> Result<Option<PrometheusOverride>, String> {
        let text = |s: &str| s.trim().to_string();
        let opt = |s: &str| Some(s.trim().to_string()).filter(|s| !s.is_empty());
        if self.provider == Provider::Auto {
            return Ok(None);
        }
        if self.provider == Provider::Service {
            let port: u16 = self
                .port
                .trim()
                .parse()
                .ok()
                .filter(|p| *p != 0)
                .ok_or("The port is a number from 1 to 65535.")?;
            let service = PrometheusService {
                namespace: text(&self.namespace),
                service: text(&self.service),
                port,
                scheme: if self.https { "https" } else { "http" }.into(),
                path_prefix: text(&self.path_prefix),
            };
            if service.service.is_empty() {
                return Err("Enter the name of the Prometheus Service.".into());
            }
            PromEndpoint {
                namespace: service.namespace.clone(),
                service: service.service.clone(),
                port,
                scheme: service.scheme.clone(),
                path_prefix: service.path_prefix.clone(),
            }
            .validate()
            .map_err(|e| e.replace("prometheus.", "The "))?;
            return Ok(Some(PrometheusOverride::Service(service)));
        }
        if self.url.trim().is_empty() {
            return Err("Enter the address of the Prometheus.".into());
        }
        let secret = || -> Result<SecretSource, String> {
            let value = self.secret.trim();
            if value.is_empty() {
                return Err(match self.from {
                    From::Env => "Enter the name of the environment variable that holds the secret.",
                    From::File => "Enter the file that holds the secret.",
                    From::Command => "Enter the command that prints the secret.",
                }
                .into());
            }
            Ok(match self.from {
                From::Env => SecretSource::Env(value.to_string()),
                From::File => SecretSource::File(value.into()),
                From::Command => SecretSource::Command(split_command(value)),
            })
        };
        let auth = match self.provider {
            Provider::Azure => ExternalAuth::Azure { tenant: opt(&self.tenant), resource: None },
            Provider::Aws => ExternalAuth::Aws { region: opt(&self.region), profile: opt(&self.profile), credentials_command: None },
            Provider::Gcp => ExternalAuth::Gcp,
            Provider::GrafanaCloud | Provider::Other => match self.sign_in {
                SignIn::None => ExternalAuth::None,
                SignIn::Token => ExternalAuth::Bearer(secret()?),
                SignIn::Password => {
                    if self.username.trim().is_empty() {
                        return Err("Enter the user name.".into());
                    }
                    ExternalAuth::Basic { username: text(&self.username), password: secret()? }
                }
            },
            Provider::Auto | Provider::Service => unreachable!("handled above"),
        };
        let mut labels = Vec::new();
        if !self.label_value.trim().is_empty() {
            let name = if self.label_name.trim().is_empty() { "cluster".to_string() } else { text(&self.label_name) };
            labels.push((name, text(&self.label_value)));
        }
        let mut headers = Vec::new();
        if self.provider == Provider::Other && !self.org_id.trim().is_empty() {
            headers.push(("X-Scope-OrgID".to_string(), text(&self.org_id)));
        }
        let endpoint = ExternalEndpoint { url: text(&self.url), auth, headers, labels };
        endpoint.validate().map_err(|e| friendly(&e))?;
        Ok(Some(PrometheusOverride::External(endpoint)))
    }

    /// Hands over the test the app started for [`Outcome::Test`].
    pub fn start_test(&mut self, job: Job<ConnectionTest>) {
        self.testing = Some((self.build().ok().flatten(), job));
    }
}

/// A validation message in the dialog's words rather than the config
/// file's keys.
fn friendly(problem: &str) -> String {
    let out = problem
        .replace("prometheus.url", "The address")
        .replace("prometheus.auth.region", "The region")
        .replace("prometheus.auth.", "The ")
        .replace("prometheus.labels", "The label")
        .replace("prometheus.headers", "The header");
    let mut chars = out.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => out,
    }
}

/// What the dialog's config would be in effect, for the test.
pub fn to_config(p: &PrometheusOverride) -> PromConfig {
    match p {
        PrometheusOverride::Service(s) => PromConfig::Service(PromEndpoint {
            namespace: s.namespace.clone(),
            service: s.service.clone(),
            port: s.port,
            scheme: s.scheme.clone(),
            path_prefix: s.path_prefix.clone(),
        }),
        PrometheusOverride::External(e) => PromConfig::External(e.clone()),
    }
}

// ---------------------------------------------------------------------
// Drawing

const LABEL_WIDTH: f32 = 112.0;

fn field(ui: &mut Ui, pal: &Palette, label: &str, add: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(Vec2::new(LABEL_WIDTH, 24.0), Layout::left_to_right(Align::Center), |ui| {
            ui.set_width(LABEL_WIDTH);
            ui.label(RichText::new(label).color(pal.muted));
        });
        add(ui);
    });
}

fn text_edit<'a>(text: &'a mut String, hint: &str, mono: bool) -> egui::TextEdit<'a> {
    let edit = egui::TextEdit::singleline(text).hint_text(hint).desired_width(f32::INFINITY);
    if mono {
        edit.font(theme::mono(12.5))
    } else {
        edit
    }
}

fn note(ui: &mut Ui, pal: &Palette, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).color(pal.muted).size(12.0)).wrap());
}

/// Draws the dialog. The app keeps it open until [`Outcome::Close`] or a
/// save that worked.
pub fn show(ctx: &egui::Context, pal: &Palette, d: &mut PromConnect, can_test_in_cluster: bool) -> Outcome {
    let mut outcome = Outcome::Open;
    // A test that finished.
    if let Some((what, job)) = &d.testing {
        if let Some(result) = job.take() {
            d.tested = Some((what.clone(), result));
            d.testing = None;
        }
    }
    let built = d.build();
    let width = 560.0_f32.min(ctx.content_rect().width() - 48.0).max(360.0);
    let resp = egui::Modal::new(egui::Id::unique("prom-connect")).show(ctx, |ui| {
        ui.set_width(width);
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.label(widgets::semibold("Prometheus for this cluster", 15.0));
        ui.horizontal(|ui| {
            ui.label(widgets::icon(icons::ICON_HUB, 14.0).color(pal.faint));
            ui.label(RichText::new(&d.context).color(pal.muted).font(theme::mono(12.0)));
        });
        ui.add_space(4.0);

        field(ui, pal, "Metrics from", |ui| {
            egui::ComboBox::from_id_salt("prom-provider")
                .width(ui.available_width())
                .selected_text(d.provider.label())
                .show_ui(ui, |ui| {
                    for p in Provider::ALL {
                        if ui.selectable_label(d.provider == p, p.label()).clicked() {
                            d.choose(p);
                        }
                    }
                });
        });
        ui.horizontal(|ui| {
            ui.add_space(LABEL_WIDTH + ui.spacing().item_spacing.x);
            ui.vertical(|ui| note(ui, pal, d.provider.help()));
        });
        ui.add_space(2.0);

        egui::ScrollArea::vertical().max_height(ctx.content_rect().height() * 0.55).auto_shrink([false, true]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            match d.provider {
                Provider::Auto => {}
                Provider::Service => {
                    field(ui, pal, "Namespace", |ui| {
                        ui.add(text_edit(&mut d.namespace, "monitoring", true));
                    });
                    field(ui, pal, "Service", |ui| {
                        ui.add(text_edit(&mut d.service, "prometheus-operated", true));
                    });
                    field(ui, pal, "Port", |ui| {
                        ui.add(egui::TextEdit::singleline(&mut d.port).desired_width(70.0).font(theme::mono(12.5)));
                        ui.checkbox(&mut d.https, "Prometheus itself speaks https");
                    });
                    field(ui, pal, "Path prefix", |ui| {
                        ui.add(text_edit(&mut d.path_prefix, "none (/select/0/prometheus for vmselect)", true));
                    });
                }
                external => {
                    field(ui, pal, "Address", |ui| {
                        ui.add(text_edit(&mut d.url, external.url_hint(), true));
                    });
                    match external {
                        Provider::Azure => {
                            field(ui, pal, "Tenant", |ui| {
                                ui.add(text_edit(&mut d.tenant, "optional: when not your default az tenant", true));
                            });
                        }
                        Provider::Aws => {
                            field(ui, pal, "AWS profile", |ui| {
                                ui.add(text_edit(&mut d.profile, "optional: default profile or $AWS_PROFILE", true));
                            });
                            field(ui, pal, "Region", |ui| {
                                ui.add(text_edit(&mut d.region, "optional: read from the address", true));
                            });
                        }
                        Provider::GrafanaCloud | Provider::Other => {
                            if external == Provider::Other {
                                field(ui, pal, "Sign in", |ui| {
                                    let options = ["None", "Bearer token", "User and password"];
                                    let selected = match d.sign_in {
                                        SignIn::None => 0,
                                        SignIn::Token => 1,
                                        SignIn::Password => 2,
                                    };
                                    if let Some(i) = widgets::segmented(ui, pal, &options, selected, 0.0) {
                                        d.sign_in = [SignIn::None, SignIn::Token, SignIn::Password][i];
                                    }
                                });
                            }
                            if d.sign_in == SignIn::Password {
                                let hint = if external == Provider::GrafanaCloud { "instance ID, e.g. 1234567" } else { "user name" };
                                field(ui, pal, "User", |ui| {
                                    ui.add(text_edit(&mut d.username, hint, true));
                                });
                            }
                            if d.sign_in != SignIn::None {
                                let what = if d.sign_in == SignIn::Token { "Token from" } else { "Password from" };
                                field(ui, pal, what, |ui| {
                                    egui::ComboBox::from_id_salt("prom-secret-from")
                                        .selected_text(d.from.label())
                                        .show_ui(ui, |ui| {
                                            for f in From::ALL {
                                                ui.selectable_value(&mut d.from, f, f.label());
                                            }
                                        });
                                    ui.add(text_edit(&mut d.secret, d.from.hint(), true));
                                });
                                ui.horizontal(|ui| {
                                    ui.add_space(LABEL_WIDTH + ui.spacing().item_spacing.x);
                                    ui.vertical(|ui| {
                                        note(
                                            ui,
                                            pal,
                                            "Skiff stores only where the secret comes from, never the secret. A command is \
                                             run without a shell and its output used for five minutes.",
                                        )
                                    });
                                });
                            }
                            if external == Provider::Other {
                                field(ui, pal, "Tenant (OrgID)", |ui| {
                                    ui.add(text_edit(&mut d.org_id, "optional: X-Scope-OrgID for Mimir, Cortex, Thanos", true));
                                });
                            }
                        }
                        _ => {}
                    }
                    ui.add_space(4.0);
                    field(ui, pal, "Only the label", |ui| {
                        ui.add(egui::TextEdit::singleline(&mut d.label_name).desired_width(90.0).font(theme::mono(12.5)));
                        ui.label(RichText::new("=").color(pal.muted));
                        ui.add(text_edit(&mut d.label_value, "empty: all of its metrics", true));
                    });
                    ui.horizontal(|ui| {
                        ui.add_space(LABEL_WIDTH + ui.spacing().item_spacing.x);
                        ui.vertical(|ui| {
                            note(
                                ui,
                                pal,
                                "A managed workspace usually holds the metrics of several clusters. AKS, EKS and GKE \
                                 label every series with the cluster's name; Test connection says whether this one matches.",
                            )
                        });
                    });
                }
            }
        });

        // What is wrong, and what the test said.
        ui.add_space(4.0);
        let current = built.as_ref().ok().cloned();
        if let Some(err) = &d.error {
            ui.add(egui::Label::new(RichText::new(err).color(pal.err)).wrap().selectable(true));
        }
        if d.testing.is_some() {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(14.0));
                ui.label(RichText::new("Asking Prometheus…").color(pal.muted));
            });
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        } else if let Some((what, result)) = &d.tested {
            // (Only while the fields still say what was tested.)
            if current.as_ref() == Some(what) {
                let (ic, color, text) = match result {
                    Ok(ConnectionTest::Ready { source, series }) => (
                        icons::ICON_CHECK_CIRCLE,
                        pal.ok,
                        format!("Connected to {source}: {} container series.", widgets::thousands(*series as usize)),
                    ),
                    Ok(ConnectionTest::NoData { source, scope }) if !scope.is_empty() => (
                        icons::ICON_WARNING,
                        pal.warn,
                        format!(
                            "Connected to {source}, but it has no container metrics with {scope}. Check the label \
                             value (it is usually the cluster's name as the cloud shows it)."
                        ),
                    ),
                    Ok(ConnectionTest::NoData { source, .. }) => (
                        icons::ICON_WARNING,
                        pal.warn,
                        format!("Connected to {source}, but it has no container metrics (container_memory_working_set_bytes)."),
                    ),
                    Err(e) => (icons::ICON_ERROR, pal.err, e.clone()),
                };
                ui.horizontal_top(|ui| {
                    ui.label(widgets::icon(ic, 16.0).color(color));
                    ui.add(egui::Label::new(RichText::new(text).color(if color == pal.err { pal.err } else { pal.text })).wrap().selectable(true));
                });
            }
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let ready = built.is_ok();
            let save = widgets::primary_button(ui, pal, icons::ICON_SAVE, "Save", 90.0, true);
            if save.clicked() {
                match &built {
                    Ok(p) => outcome = Outcome::Save(p.clone()),
                    Err(e) => d.error = Some(e.clone()),
                }
            }
            let testable = ready
                && d.provider != Provider::Auto
                && d.testing.is_none()
                && (d.provider != Provider::Service || can_test_in_cluster);
            let test = ui.add_enabled(testable, egui::Button::new((widgets::icon(icons::ICON_SPEED, 16.0), "Test connection")).min_size(Vec2::new(0.0, 32.0)));
            let test = if d.provider == Provider::Service && !can_test_in_cluster {
                test.on_disabled_hover_text("Connect to the cluster first: this Prometheus is reached through it.")
            } else {
                test.on_disabled_hover_text(built.as_ref().err().map(String::as_str).unwrap_or("Nothing to test"))
            };
            if test.clicked() {
                if let Ok(Some(p)) = &built {
                    d.error = None;
                    outcome = Outcome::Test(to_config(p));
                }
            }
            if ui.add(egui::Button::new("Cancel").min_size(Vec2::new(0.0, 32.0))).clicked() {
                outcome = Outcome::Close;
            }
        });
    });
    if resp.should_close() && outcome == Outcome::Open {
        outcome = Outcome::Close;
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cluster_names_are_read_from_generated_context_names() {
        assert_eq!(cluster_name("arn:aws:eks:eu-west-1:123456789012:cluster/shop-prod"), "shop-prod");
        assert_eq!(cluster_name("gke_my-project_europe-west1-b_shop-prod"), "shop-prod");
        assert_eq!(cluster_name("aks-prod"), "aks-prod");
    }

    #[test]
    fn commands_are_split_like_a_shell_would() {
        assert_eq!(split_command("vault read -field=token secret/prom"), ["vault", "read", "-field=token", "secret/prom"]);
        assert_eq!(split_command(r#"  op read "op://My Vault/prom/token" "#), ["op", "read", "op://My Vault/prom/token"]);
        assert_eq!(split_command("a '' b"), ["a", "", "b"]);
        let argv = split_command(r#"op read "op://My Vault/x""#);
        assert_eq!(split_command(&join_command(&argv)), argv);
    }

    #[test]
    fn azure_fills_in_the_cluster_label_and_builds_what_the_config_reads() {
        let mut d = PromConnect::new("aks-prod", None);
        assert_eq!(d.build(), Ok(None));
        d.choose(Provider::Azure);
        assert_eq!(d.build(), Err("Enter the address of the Prometheus.".into()));
        d.url = " https://ws-1.westeurope.prometheus.monitor.azure.com ".into();
        let built = d.build().unwrap().unwrap();
        assert_eq!(
            built,
            PrometheusOverride::External(ExternalEndpoint {
                url: "https://ws-1.westeurope.prometheus.monitor.azure.com".into(),
                auth: ExternalAuth::Azure { tenant: None, resource: None },
                headers: Vec::new(),
                labels: vec![("cluster".into(), "aks-prod".into())],
            })
        );
        // Opened again on what was saved, it shows the same.
        let again = PromConnect::new("aks-prod", Some(&built));
        assert_eq!(again.provider, Provider::Azure);
        assert_eq!(again.build(), Ok(Some(built)));
    }

    #[test]
    fn every_kind_reopens_as_it_was_saved() {
        let mut d = PromConnect::new("ctx", None);
        d.choose(Provider::Other);
        d.url = "https://mimir.example.com/prometheus".into();
        d.sign_in = SignIn::Token;
        d.from = From::Command;
        d.secret = r#"op read "op://My Vault/prom""#.into();
        d.org_id = "team-a".into();
        let other = d.build().unwrap().unwrap();
        let PrometheusOverride::External(e) = &other else { panic!() };
        assert_eq!(e.auth, ExternalAuth::Bearer(SecretSource::Command(vec!["op".into(), "read".into(), "op://My Vault/prom".into()])));
        assert_eq!(e.headers, [("X-Scope-OrgID".to_string(), "team-a".to_string())]);
        assert!(e.labels.is_empty());

        let mut g = PromConnect::new("ctx", None);
        g.choose(Provider::GrafanaCloud);
        g.url = "https://prometheus-prod-01-eu-west-0.grafana.net/api/prom".into();
        assert_eq!(g.build(), Err("Enter the user name.".into()));
        g.username = "123456".into();
        g.secret = "GRAFANA_TOKEN".into();
        let grafana = g.build().unwrap().unwrap();

        let mut s = PromConnect::new("ctx", None);
        s.choose(Provider::Service);
        assert!(s.build().is_err());
        s.service = "vmselect".into();
        s.port = "8481".into();
        s.path_prefix = "/select/0/prometheus".into();
        let service = s.build().unwrap().unwrap();
        s.port = "0".into();
        assert!(s.build().is_err());

        let mut a = PromConnect::new("arn:aws:eks:eu-west-1:1:cluster/shop", None);
        a.choose(Provider::Aws);
        a.url = "https://aps-workspaces.eu-west-1.amazonaws.com/workspaces/ws-1".into();
        a.profile = "prod".into();
        let aws = a.build().unwrap().unwrap();
        let PrometheusOverride::External(e) = &aws else { panic!() };
        assert_eq!(e.labels, [("cluster".to_string(), "shop".to_string())]);

        for saved in [other, grafana, service, aws] {
            let reopened = PromConnect::new("ctx", Some(&saved));
            assert_eq!(reopened.build(), Ok(Some(saved)));
        }
    }

    #[test]
    fn mistakes_are_said_in_the_dialogs_words() {
        let mut d = PromConnect::new("ctx", None);
        d.choose(Provider::Azure);
        d.url = "http://prom.example.com".into();
        let err = d.build().unwrap_err();
        assert!(err.starts_with("The address") && err.contains("https"), "{err}");
        d.choose(Provider::Aws);
        d.url = "https://amp.example.com".into();
        let err = d.build().unwrap_err();
        assert!(err.starts_with("The region"), "{err}");
    }
}
