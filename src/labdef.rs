//! Typed multi-machine lab definitions and run configs (the JSON format).
//!
//! Two files, both strict JSON with `deny_unknown_fields`:
//!
//!   * a **lab** (`labs/<name>.json`, built once into a 0 state): the domain, the private
//!     network, the machines (exactly one `dc`, the rest members) and the domain users.
//!   * a **run** (`<app>.json`, each start restores the 0 state first): which lab, which host
//!     folders to mount, and the steps to run and where.
//!
//! [`LabDef`] / [`RunDef`] are the raw serde shapes (what the JSON Schema in `schema/`
//! describes). [`LabDef::resolve`] and [`RunDef::resolve`] validate them with clear errors
//! *before anything boots* and return the resolved [`Lab`] / [`Run`] the runner consumes
//! (defaults filled in: roles, IPs, memory, the `as` user of each step on each machine).

use crate::util::is_safe_name;
use anyhow::{bail, Result};
use indexmap::IndexMap;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use std::path::Path;

// ===================================================================== raw serde types (lab)

/// A lab definition file, e.g. `labs/<name>.json`, or a `lab.json` in your own project.
///
/// Two modes, decided by the machines: with one machine of `role: "dc"` the lab is an
/// **AD domain** (`domain` and `netbios` required, users are domain users, the other machines
/// join the domain); with no `dc` it is a **workgroup** (no domain; users are local accounts
/// created on every machine). Any Windows Server may be the DC and any OS a member.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabDef {
    /// Optional path or URL of the JSON Schema, for editors (ignored by `wtl`).
    #[serde(default, rename = "$schema", skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// The lab's name: names its state folder `labs/state/<name>/` and its VMs. Default: the
    /// file name without `.json` (for a lab outside win-test-lab's `labs/`, plus a short hash
    /// of its path, so two projects' `lab.json` never share a state).
    #[serde(default)]
    pub name: Option<String>,
    /// AD mode: the forest's DNS name, e.g. `lab.local`. Must be absent in workgroup mode.
    #[serde(default)]
    pub domain: Option<String>,
    /// AD mode: the NetBIOS domain name, e.g. `LAB` (users log on as `LAB\student`). Must be
    /// absent in workgroup mode.
    #[serde(default)]
    pub netbios: Option<String>,
    /// Password of the Administrator: the domain Administrator (and DSRM) in AD mode, each
    /// machine's local Administrator in workgroup mode. Default `WinTest!2026` (what the base
    /// images are installed with).
    #[serde(default)]
    pub admin_password: Option<String>,
    /// The private network between the machines. Required unless the lab is one workgroup
    /// machine (which then only has the host's NAT network).
    #[serde(default)]
    pub network: Option<NetworkDef>,
    /// The machines, keyed by computer name. At most one has `role: dc` (none = workgroup).
    pub machines: IndexMap<String, MachineDef>,
    /// The users: domain users in AD mode (always in Domain Users), local users on every
    /// machine in workgroup mode (always in Users). Named groups are created if missing.
    #[serde(default)]
    pub users: IndexMap<String, UserDef>,
}

/// The private VM-to-VM network.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NetworkDef {
    /// First three octets of the /24, e.g. `10.77.30` (DC gets .10, the others .20, .21, ...).
    pub subnet: String,
    /// Host loopback UDP port the multicast lab LAN uses (unique per running lab).
    pub lan_port: u16,
}

/// What a machine is for (whether it hosts the domain). Derived from its [`roles`](LabRole): a
/// machine with the `ad-ds` role is the [`Role::Dc`], every other machine is a [`Role::Member`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// the domain controller (at most one per lab; one makes it an AD lab). A Windows Server.
    Dc,
    /// a domain-joined member in AD mode, a standalone machine in workgroup mode (the default)
    #[default]
    Member,
}

/// A server role baked into the 0 state. A machine lists the roles it runs in `roles`; the old
/// `role: "dc"` is an alias for `roles: ["ad-ds"]` and `role: "member"` for no roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum LabRole {
    /// Active Directory Domain Services: makes this machine the domain controller (one per AD
    /// lab; promotes a new forest). A Windows Server.
    AdDs,
    /// AD Certificate Services: an Enterprise Root CA (needs the domain; installed as a domain
    /// admin after the join). A Windows Server.
    AdCs,
    /// DHCP server: a scope for the lab subnet, authorized in AD when there is a domain. A
    /// Windows Server.
    Dhcp,
    /// IIS web server: `Web-Server` + the common features, the default site serving a test page.
    /// A Windows Server.
    Iis,
}

impl LabRole {
    /// The JSON spelling, e.g. `ad-ds`.
    pub fn as_str(self) -> &'static str {
        match self {
            LabRole::AdDs => "ad-ds",
            LabRole::AdCs => "ad-cs",
            LabRole::Dhcp => "dhcp",
            LabRole::Iis => "iis",
        }
    }
}

/// `role: "dc"` / `role: "member"` — the compact alias for `roles` (see [`LabRole`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum RoleAlias {
    /// same as `roles: ["ad-ds"]`
    Dc,
    /// same as `roles: []`
    Member,
}

/// `ad_cs` options (an Enterprise Root CA). All optional; see the defaults in [`AdCsOpts`].
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AdCsDef {
    /// the CA's common name (default `<NETBIOS>-ROOT-CA`)
    #[serde(default)]
    pub ca_name: Option<String>,
    /// the CA key length in bits (default 2048)
    #[serde(default)]
    pub key_length: Option<u32>,
    /// the root certificate's validity in years (default 5)
    #[serde(default)]
    pub validity_years: Option<u32>,
    /// link a domain GPO that turns on computer + user certificate auto-enrollment, and publish
    /// the Workstation/User templates enrollable by Domain Computers/Users (default true)
    #[serde(default)]
    pub autoenroll: Option<bool>,
}

/// `dhcp` options. See [`DhcpOpts`].
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DhcpDef {
    /// the scope as last octets within the lab subnet, `.<start>-.<end>` (default `.100-.199`);
    /// router and DNS are the DC
    #[serde(default)]
    pub range: Option<String>,
}

/// `iis` options. See [`IisOpts`].
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IisDef {
    /// the default site's physical path (default `C:\inetpub\wwwroot`); a test page is written
    /// there if one is not present
    #[serde(default)]
    pub site_path: Option<String>,
}

/// One setup script, run once during the 0-state build (after the join, roles and features).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetupDef {
    /// the script to run, relative to the lab file: `.ps1 .bat .cmd .exe .msi`, e.g.
    /// `./setup/configure.ps1`. Staged onto the control disc and run during the build.
    pub run: String,
    /// command-line arguments
    #[serde(default)]
    pub args: Option<String>,
    /// who to run it as: `system` (default) or a lab user (a domain user in AD mode)
    #[serde(default, rename = "as")]
    pub as_: Option<String>,
}

/// One machine in a lab.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MachineDef {
    /// OS key from `lib/oslist.conf`, e.g. `2019`, `2012`, `win11`, `win11-arm64`.
    pub os: String,
    /// The compact role alias: `dc` (= `roles: ["ad-ds"]`) or `member` (= no roles). Mutually
    /// exclusive with `roles`.
    #[serde(default)]
    pub role: Option<RoleAlias>,
    /// The server roles this machine runs (see [`LabRole`]): `ad-ds`, `ad-cs`, `dhcp`, `iis`.
    /// Mutually exclusive with `role`. Servers only.
    #[serde(default)]
    pub roles: Option<Vec<LabRole>>,
    /// Windows feature names to install during the build (`Install-WindowsFeature`), e.g.
    /// `Telnet-Client`. Servers only; an unknown name fails the build with the available list.
    #[serde(default)]
    pub features: Vec<String>,
    /// Setup scripts run once during the build, in order, after the join/roles/features.
    #[serde(default)]
    pub setup: Vec<SetupDef>,
    /// `ad-cs` role options (ignored without the `ad-cs` role).
    #[serde(default)]
    pub ad_cs: Option<AdCsDef>,
    /// `dhcp` role options (ignored without the `dhcp` role).
    #[serde(default)]
    pub dhcp: Option<DhcpDef>,
    /// `iis` role options (ignored without the `iis` role).
    #[serde(default)]
    pub iis: Option<IisDef>,
    /// A user (from `users`) this machine logs on as automatically; without one it logs on as
    /// Administrator. The DC cannot set one.
    #[serde(default)]
    pub logon: Option<String>,
    /// RAM in MB (default 4096 for a member, 3072 for the DC).
    #[serde(default)]
    pub mem: Option<u32>,
    /// vCPUs (default 2).
    #[serde(default)]
    pub cpus: Option<u32>,
    /// Static IPv4, either a full `a.b.c.d` or a bare last octet within the subnet. Default:
    /// DC `.10`, members `.20`, `.21`, ... in declaration order.
    #[serde(default)]
    pub ip: Option<String>,
}

/// One user (a domain user in AD mode, a local user on every machine in workgroup mode).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UserDef {
    pub password: String,
    /// Extra groups (domain groups in AD mode, e.g. "Domain Admins"; local groups in workgroup
    /// mode, e.g. "Administrators"). A group that does not exist is created.
    #[serde(default)]
    pub groups: Vec<String>,
}

// ===================================================================== raw serde types (run)

/// A run config file: `<app>.json`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunDef {
    /// Optional path or URL of the JSON Schema, for editors (ignored by `wtl`).
    #[serde(default, rename = "$schema", skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// The lab to boot: a name (`labs/<lab>.json` in win-test-lab, e.g. `school`) or a path to
    /// a lab file, relative to this run file (e.g. `./lab.json`). A value with a `/` or `\`
    /// or ending in `.json` is a path.
    pub lab: String,
    /// Label for the results folder (default: the run file's stem).
    #[serde(default)]
    pub name: Option<String>,
    /// Seconds a step may run before it counts as "still running" (default 60).
    #[serde(default)]
    pub wait: Option<i64>,
    /// Give up on the whole run after this many minutes (default 60).
    #[serde(default)]
    pub timeout_min: Option<i64>,
    /// Host folder for each drive letter, e.g. `{ "X": "./app" }` (read-only in the guest).
    #[serde(default)]
    pub mount: IndexMap<String, String>,
    /// Steps, run top to bottom.
    #[serde(default)]
    pub steps: Vec<StepDef>,
}

/// One step of a run.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StepDef {
    /// Where to run: a machine name, a list of names, or `all` (every member, not the DC).
    /// Default `all`.
    #[serde(default)]
    pub on: OnSpec,
    /// Who to run as: `system` (full rights) or a user name. Default: the user logged on to
    /// that machine (its `logon` user, else Administrator). A step on the DC needs an `as`.
    #[serde(default, rename = "as")]
    pub as_: Option<String>,
    /// The program to run: `.exe .bat .cmd .ps1 .msi`, e.g. `X:/setup.bat`.
    pub run: String,
    /// Command-line arguments.
    #[serde(default)]
    pub args: Option<String>,
    /// Exit code that counts as success (default 0).
    #[serde(default)]
    pub expect_exit: Option<i64>,
    /// Seconds this step may run before it counts as "still running" (default: the run's
    /// `wait`).
    #[serde(default)]
    pub wait: Option<i64>,
    /// If true, a program still running at `wait` counts as OK and is left running.
    #[serde(default)]
    pub keep_running: bool,
}

/// `on`: a single machine, a list of machines, or every member.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum OnSpec {
    /// every member (not the DC)
    #[default]
    All,
    /// the named machines
    Machines(Vec<String>),
}

impl<'de> Deserialize<'de> for OnSpec {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<OnSpec, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            One(String),
            Many(Vec<String>),
        }
        Ok(match Raw::deserialize(d)? {
            Raw::One(s) if s == "all" => OnSpec::All,
            Raw::One(s) => OnSpec::Machines(vec![s]),
            Raw::Many(v) => OnSpec::Machines(v),
        })
    }
}

impl Serialize for OnSpec {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            OnSpec::All => s.serialize_str("all"),
            OnSpec::Machines(v) => v.serialize(s),
        }
    }
}

impl JsonSchema for OnSpec {
    fn schema_name() -> String {
        "OnSpec".into()
    }
    fn json_schema(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        // a machine name, a list of names, or the literal "all"
        use schemars::schema::{InstanceType, SchemaObject, SubschemaValidation};
        let string = SchemaObject { instance_type: Some(InstanceType::String.into()), ..Default::default() };
        let list = gen.subschema_for::<Vec<String>>().into_object();
        SchemaObject {
            metadata: Some(Box::new(schemars::schema::Metadata {
                description: Some("a machine name, a list of machine names, or \"all\" (every member)".into()),
                ..Default::default()
            })),
            subschemas: Some(Box::new(SubschemaValidation { any_of: Some(vec![string.into(), list.into()]), ..Default::default() })),
            ..Default::default()
        }
        .into()
    }
}

// ===================================================================== resolved types

/// The default Administrator password: what `wtl build-image` installs base images with.
pub const DEFAULT_ADMIN_PASSWORD: &str = "WinTest!2026";

/// A fully resolved lab: defaults filled in, validated, ready to boot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lab {
    /// the explicit `name` from the file, if any (the runner derives the state name)
    pub name: Option<String>,
    /// AD mode (a domain with a DC) or workgroup mode
    pub mode: Mode,
    pub admin_password: String,
    /// the lab LAN; None for a single workgroup machine without one
    pub network: Option<Network>,
    /// in declaration order (the DC may be anywhere in it)
    pub machines: Vec<Machine>,
    /// in declaration order
    pub users: Vec<User>,
}

/// AD domain or workgroup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// one DC, the others join `domain`
    Domain { domain: String, netbios: String },
    /// no domain: standalone machines with local users
    Workgroup,
}

/// The resolved lab LAN.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Network {
    pub subnet: String,
    pub lan_port: u16,
}

/// Resolved `ad-cs` options (defaults filled in).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdCsOpts {
    pub ca_name: String,
    pub key_length: u32,
    pub validity_years: u32,
    pub autoenroll: bool,
}

/// Resolved `dhcp` options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DhcpOpts {
    /// scope start/end as last octets within the subnet
    pub start: u8,
    pub end: u8,
}

/// Resolved `iis` options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IisOpts {
    pub site_path: String,
}

/// One resolved setup script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setup {
    /// forward-slashed path relative to the lab file (e.g. `setup/configure.ps1`)
    pub run: String,
    pub args: Option<String>,
    pub as_: StepAs,
}

/// One resolved machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Machine {
    pub name: String,
    pub os: String,
    pub role: Role,
    /// the server roles baked into the 0 state, in declaration order
    pub roles: Vec<LabRole>,
    /// Windows features to install during the build, in declaration order
    pub features: Vec<String>,
    /// setup scripts run once during the build, in order
    pub setup: Vec<Setup>,
    /// present iff the machine has the `ad-cs` role
    pub ad_cs: Option<AdCsOpts>,
    /// present iff the machine has the `dhcp` role
    pub dhcp: Option<DhcpOpts>,
    /// present iff the machine has the `iis` role
    pub iis: Option<IisOpts>,
    pub logon: Option<String>,
    pub mem: u32,
    pub cpus: u32,
    /// the static IP on the lab LAN; None without a lab LAN
    pub ip: Option<String>,
}

impl Machine {
    /// True if this machine runs the given role.
    pub fn has_role(&self, r: LabRole) -> bool {
        self.roles.contains(&r)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub name: String,
    pub password: String,
    pub groups: Vec<String>,
}

/// A fully resolved run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub lab: String,
    pub name: String,
    pub wait: i64,
    pub timeout_min: i64,
    /// (drive letter, host path) in declaration order
    pub mounts: Vec<(String, String)>,
    pub steps: Vec<RunStep>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunStep {
    /// the machines this step runs on (resolved, in lab order for `all`)
    pub on: Vec<String>,
    pub as_: StepAs,
    pub run: String,
    pub args: Option<String>,
    pub expect_exit: i64,
    /// None = use the run's wait
    pub wait: Option<i64>,
    pub keep_running: bool,
}

/// Who a step runs as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepAs {
    /// the SYSTEM account (full rights)
    System,
    /// a named domain user
    User(String),
    /// that machine's own logon user (resolved per machine when its disc is built)
    Logon,
}

const SCRIPT_EXT: [&str; 5] = [".exe", ".bat", ".cmd", ".ps1", ".msi"];
const MAX_COMPUTER_NAME: usize = 15;

impl Machine {
    /// default RAM in MB for a role when `mem` is not given
    fn default_mem(role: Role) -> u32 {
        match role {
            Role::Dc => 3072,
            Role::Member => 4096,
        }
    }
}

/// Resolve a machine's server roles from the `role`/`roles` fields (the compact `role` is an
/// alias for `roles`). Setting both is an error, as is listing the same role twice.
fn resolve_roles(name: &str, m: &MachineDef, path: &str) -> Result<Vec<LabRole>> {
    if m.role.is_some() && m.roles.is_some() {
        bail!("{}: machine {}: set either \"role\" or \"roles\", not both", path, name);
    }
    let roles = match (&m.role, &m.roles) {
        (Some(RoleAlias::Dc), _) => vec![LabRole::AdDs],
        (Some(RoleAlias::Member), _) => vec![],
        (None, Some(rs)) => rs.clone(),
        (None, None) => vec![],
    };
    for (i, r) in roles.iter().enumerate() {
        if roles[..i].contains(r) {
            bail!("{}: machine {}: role {:?} is listed twice", path, name, r.as_str());
        }
    }
    Ok(roles)
}

impl LabDef {
    /// Parse a lab file (strict JSON) without validating its meaning.
    pub fn parse(text: &str, path: &str) -> Result<LabDef> {
        serde_json::from_str(text).map_err(|e| anyhow::anyhow!("{}: {}", path, e))
    }

    /// Read + validate + resolve a lab file.
    pub fn load(path: &Path) -> Result<Lab> {
        let text = std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("{}: {}", path.display(), e))?;
        let def = LabDef::parse(&text, &path.display().to_string())?;
        def.resolve(&path.display().to_string())
    }

    /// Validate and fill in defaults. `path` is only for error messages.
    pub fn resolve(&self, path: &str) -> Result<Lab> {
        if let Some(n) = &self.name {
            if !is_safe_name(n) {
                bail!("{}: name {:?} must be A-Z 0-9 . _ -", path, n);
            }
        }
        if self.machines.is_empty() {
            bail!("{}: no machines", path);
        }
        // each machine's roles up front (the `role` alias resolved to `roles`); the DC is the
        // one with ad-ds. The mode follows: one ad-ds = AD, none = workgroup.
        let roles_by_machine: Vec<(String, Vec<LabRole>)> =
            self.machines.iter().map(|(n, m)| resolve_roles(n, m, path).map(|r| (n.clone(), r))).collect::<Result<_>>()?;
        let dc_count = roles_by_machine.iter().filter(|(_, r)| r.contains(&LabRole::AdDs)).count();
        let mode = match dc_count {
            0 => {
                if self.domain.is_some() || self.netbios.is_some() {
                    bail!(
                        "{}: domain/netbios is set but no machine has the \"ad-ds\" role: give one Windows Server \"role\": \"dc\" (or \"roles\": [\"ad-ds\"]) for an AD lab, or drop domain and netbios for a workgroup lab",
                        path
                    );
                }
                Mode::Workgroup
            }
            1 => {
                let domain = self.domain.clone().unwrap_or_default();
                if domain.trim().is_empty() {
                    bail!("{}: an AD lab (a machine has the \"ad-ds\" role) needs \"domain\", e.g. \"lab.local\"", path);
                }
                let netbios = self.netbios.clone().unwrap_or_default();
                if !is_safe_name(&netbios) || netbios.len() > MAX_COMPUTER_NAME {
                    bail!("{}: an AD lab needs \"netbios\": 1-15 of A-Z 0-9 . _ - (e.g. \"LAB\"), got {:?}", path, netbios);
                }
                Mode::Domain { domain, netbios }
            }
            n => bail!("{}: at most one machine may have the \"ad-ds\" role (found {})", path, n),
        };
        let admin_password = self.admin_password.clone().unwrap_or_else(|| DEFAULT_ADMIN_PASSWORD.to_string());
        if admin_password.is_empty() {
            bail!("{}: admin_password is empty", path);
        }
        // the LAN: needed whenever machines must talk to each other (and always for AD)
        let network = match &self.network {
            Some(n) => {
                // subnet: three octets 0-255
                let octets: Vec<&str> = n.subnet.split('.').collect();
                if octets.len() != 3 || !octets.iter().all(|o| o.parse::<u8>().is_ok()) {
                    bail!("{}: network.subnet {:?} must be three octets, e.g. 10.77.30", path, n.subnet);
                }
                if n.lan_port == 0 {
                    bail!("{}: network.lan_port must be a port 1-65535", path);
                }
                Some(Network { subnet: n.subnet.clone(), lan_port: n.lan_port })
            }
            None if mode != Mode::Workgroup => bail!("{}: an AD lab needs \"network\" (subnet + lan_port)", path),
            None if self.machines.len() > 1 => {
                bail!("{}: a lab with several machines needs \"network\" (subnet + lan_port) for the private LAN between them", path)
            }
            None => None,
        };

        // users first, so machines can reference them
        let mut users = Vec::new();
        for (name, u) in &self.users {
            if !is_safe_name(name) {
                bail!("{}: user name {:?} must be A-Z 0-9 . _ -", path, name);
            }
            if u.password.is_empty() {
                bail!("{}: user {} has an empty password", path, name);
            }
            // Windows' complexity rule refuses a password that contains the account name (any
            // case, for names of 3+ characters); catch it here instead of mid-build
            if name.len() >= 3 && u.password.to_lowercase().contains(&name.to_lowercase()) {
                bail!("{}: user {}: the password contains the user name, which Windows rejects", path, name);
            }
            users.push(User { name: name.clone(), password: u.password.clone(), groups: u.groups.clone() });
        }
        let user_names: Vec<&str> = users.iter().map(|u| u.name.as_str()).collect();

        let is_ad = mode != Mode::Workgroup;
        // default IPs: DC .10, members .20, .21, ... in declaration order
        let mut member_octet = 20u16;
        let mut machines = Vec::new();
        for (i, (name, m)) in self.machines.iter().enumerate() {
            if !is_safe_name(name) || name.len() > MAX_COMPUTER_NAME {
                bail!("{}: machine name {:?} must be 1-15 of A-Z 0-9 . _ - (Windows computer name)", path, name);
            }
            if !is_safe_name(&m.os) {
                bail!("{}: machine {}: os {:?} must be an oslist key (A-Z 0-9 . _ -)", path, name, m.os);
            }
            let lab_roles = roles_by_machine[i].1.clone();
            let role = if lab_roles.contains(&LabRole::AdDs) { Role::Dc } else { Role::Member };
            // ad-cs needs a domain to join and enroll against (a standalone CA is a follow-up)
            if lab_roles.contains(&LabRole::AdCs) && !is_ad {
                bail!(
                    "{}: machine {}: the \"ad-cs\" role needs an AD lab (a machine with \"ad-ds\"); a standalone CA is not supported yet",
                    path,
                    name
                );
            }
            if let Some(l) = &m.logon {
                if !user_names.contains(&l.as_str()) {
                    bail!("{}: machine {}: logon user {:?} is not in users", path, name, l);
                }
            }
            if role == Role::Dc && m.logon.is_some() {
                bail!("{}: machine {} is the DC and cannot have a logon user", path, name);
            }
            // role options: present only for the roles the machine actually has
            let ad_cs = if lab_roles.contains(&LabRole::AdCs) {
                let d = m.ad_cs.clone().unwrap_or_default();
                let ca_name = d
                    .ca_name
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| format!("{}-ROOT-CA", self.netbios.clone().unwrap_or_default()));
                Some(AdCsOpts {
                    ca_name,
                    key_length: d.key_length.unwrap_or(2048),
                    validity_years: d.validity_years.unwrap_or(5),
                    autoenroll: d.autoenroll.unwrap_or(true),
                })
            } else {
                None
            };
            let dhcp = if lab_roles.contains(&LabRole::Dhcp) {
                let d = m.dhcp.clone().unwrap_or_default();
                Some(parse_dhcp_range(name, d.range.as_deref().unwrap_or(".100-.199"), path)?)
            } else {
                None
            };
            let iis = if lab_roles.contains(&LabRole::Iis) {
                let d = m.iis.clone().unwrap_or_default();
                let site_path = d.site_path.filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "C:\\inetpub\\wwwroot".to_string());
                Some(IisOpts { site_path })
            } else {
                None
            };
            // setup scripts: extension, forward-slashed relative path, resolved `as`
            let mut setup = Vec::new();
            for (si, sd) in m.setup.iter().enumerate() {
                let run = sd.run.replace('\\', "/");
                let run = run.strip_prefix("./").unwrap_or(&run).to_string();
                let lower = run.to_lowercase();
                if !SCRIPT_EXT.iter().any(|e| lower.ends_with(e)) {
                    bail!("{}: machine {}: setup[{}] run {:?} must end in .exe .bat .cmd .ps1 or .msi", path, name, si, sd.run);
                }
                if run.contains(':') || run.starts_with('/') {
                    bail!(
                        "{}: machine {}: setup[{}] run {:?} must be relative to the lab file (e.g. ./setup/x.ps1)",
                        path,
                        name,
                        si,
                        sd.run
                    );
                }
                let as_ = match sd.as_.as_deref() {
                    None | Some("system") => StepAs::System,
                    Some(u) => {
                        if !user_names.contains(&u) {
                            bail!("{}: machine {}: setup[{}] as {:?} is not a user in the lab (or \"system\")", path, name, si, u);
                        }
                        StepAs::User(u.to_string())
                    }
                };
                setup.push(Setup { run, args: sd.args.clone().filter(|a| !a.is_empty()), as_ });
            }
            let last = match role {
                Role::Dc => 10u16,
                Role::Member => {
                    let o = member_octet;
                    member_octet += 1;
                    o
                }
            };
            let ip = match (&network, &m.ip) {
                (None, None) => None,
                (None, Some(_)) => bail!("{}: machine {}: ip needs a \"network\"", path, name),
                (Some(n), None) => Some(format!("{}.{}", n.subnet, last)),
                (Some(_), Some(s)) if s.contains('.') => Some(s.clone()),
                (Some(n), Some(s)) => {
                    let o: u8 = s.parse().map_err(|_| {
                        anyhow::anyhow!("{}: machine {}: ip {:?} must be a full a.b.c.d or a last octet 0-255", path, name, s)
                    })?;
                    Some(format!("{}.{}", n.subnet, o))
                }
            };
            machines.push(Machine {
                name: name.clone(),
                os: m.os.clone(),
                role,
                roles: lab_roles,
                features: m.features.clone(),
                setup,
                ad_cs,
                dhcp,
                iis,
                logon: m.logon.clone(),
                mem: m.mem.unwrap_or_else(|| Machine::default_mem(role)),
                cpus: m.cpus.unwrap_or(2),
                ip,
            });
        }

        // duplicate IPs
        for i in 0..machines.len() {
            for j in (i + 1)..machines.len() {
                if machines[i].ip.is_some() && machines[i].ip == machines[j].ip {
                    bail!(
                        "{}: machines {} and {} share IP {}",
                        path,
                        machines[i].name,
                        machines[j].name,
                        machines[i].ip.as_deref().unwrap_or("")
                    );
                }
            }
        }

        Ok(Lab { name: self.name.clone(), mode, admin_password, network, machines, users })
    }
}

impl Lab {
    /// The machine called NAME.
    pub fn machine(&self, name: &str) -> Option<&Machine> {
        self.machines.iter().find(|m| m.name == name)
    }
    /// The domain controller (None in workgroup mode).
    pub fn dc(&self) -> Option<&Machine> {
        self.machines.iter().find(|m| m.role == Role::Dc)
    }
    /// True for an AD lab (with a DC).
    pub fn is_ad(&self) -> bool {
        matches!(self.mode, Mode::Domain { .. })
    }
    /// The domain's DNS name, or "" in workgroup mode.
    pub fn domain(&self) -> &str {
        match &self.mode {
            Mode::Domain { domain, .. } => domain,
            Mode::Workgroup => "",
        }
    }
    /// The NetBIOS domain name, or "" in workgroup mode.
    pub fn netbios(&self) -> &str {
        match &self.mode {
            Mode::Domain { netbios, .. } => netbios,
            Mode::Workgroup => "",
        }
    }
    /// The machines that are not the DC.
    pub fn members(&self) -> impl Iterator<Item = &Machine> {
        self.machines.iter().filter(|m| m.role == Role::Member)
    }
}

impl RunDef {
    /// Parse a run file (strict JSON) without validating its meaning.
    pub fn parse(text: &str, path: &str) -> Result<RunDef> {
        serde_json::from_str(text).map_err(|e| anyhow::anyhow!("{}: {}", path, e))
    }

    /// Validate and fill in defaults against the lab it names. `path` / `default_name` are for
    /// error messages and the results folder; `host_resolve` makes a mount's host path
    /// absolute (relative to the run file's directory).
    pub fn resolve(&self, path: &str, default_name: &str, lab: &Lab, host_resolve: impl Fn(&str) -> String) -> Result<Run> {
        if self.lab.trim().is_empty() {
            bail!("{}: lab is empty", path);
        }
        let name = self.name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| default_name.to_string());
        if !is_safe_name(&name) {
            bail!("{}: name {:?} must be A-Z 0-9 . _ -", path, name);
        }
        let wait = self.wait.unwrap_or(60);
        if wait <= 0 {
            bail!("{}: wait must be a positive integer", path);
        }
        let timeout_min = self.timeout_min.unwrap_or(60);
        if timeout_min < 0 {
            bail!("{}: timeout_min must be a whole number", path);
        }

        // mounts: letter -> host path, letter a single A-Z, not C/D, unique
        let mut mounts = Vec::new();
        for (letter, host) in &self.mount {
            let up = letter.to_uppercase();
            if !(up.len() == 1 && up.as_bytes()[0].is_ascii_alphabetic()) {
                bail!("{}: mount drive {:?} must be a single letter A-Z", path, letter);
            }
            if up == "C" || up == "D" {
                bail!("{}: drive {}: is reserved (C: system, D: lab disc); pick e.g. X, Y, Z", path, up);
            }
            if mounts.iter().any(|(l, _): &(String, String)| *l == up) {
                bail!("{}: drive {}: used by more than one mount", path, up);
            }
            if host.trim().is_empty() {
                bail!("{}: mount {}: has an empty host path", path, up);
            }
            mounts.push((up, host_resolve(host)));
        }
        if mounts.len() > 3 {
            bail!("{}: at most 3 mounts (the lab disc takes one of the 4 CD slots)", path);
        }
        let mounted: Vec<&str> = mounts.iter().map(|(l, _)| l.as_str()).collect();

        let member_names: Vec<&str> = lab.members().map(|m| m.name.as_str()).collect();
        let mut steps = Vec::new();
        for (i, s) in self.steps.iter().enumerate() {
            let n = i + 1;
            // where
            let on: Vec<String> = match &s.on {
                OnSpec::All => member_names.iter().map(|s| s.to_string()).collect(),
                OnSpec::Machines(v) => {
                    for name in v {
                        if lab.machine(name).is_none() {
                            bail!("{}: step {}: on {:?} is not a machine in lab {}", path, n, name, self.lab);
                        }
                    }
                    v.clone()
                }
            };
            if on.is_empty() {
                bail!("{}: step {}: on \"all\" but the lab has no members", path, n);
            }
            // who
            let as_ = match s.as_.as_deref() {
                None => StepAs::Logon,
                Some("system") => StepAs::System,
                Some(u) => {
                    if !lab.users.iter().any(|x| x.name == u) {
                        bail!("{}: step {}: as {:?} is not a user in lab {} (or \"system\")", path, n, u, self.lab);
                    }
                    StepAs::User(u.to_string())
                }
            };
            // `as` defaults to the machine's logged-on user (its logon user, else Administrator);
            // the DC has no desktop session of its own to run in
            if as_ == StepAs::Logon {
                for mname in &on {
                    let m = lab.machine(mname).expect("checked above");
                    if m.role == Role::Dc {
                        bail!("{}: step {}: runs on the DC {}, so it needs an explicit \"as\" (a user or \"system\")", path, n, mname);
                    }
                }
            }
            // what
            let run = s.run.replace('/', "\\");
            let (letter, rest) = split_ref(&run)
                .ok_or_else(|| anyhow::anyhow!("{}: step {}: run {:?} must start with a drive, e.g. X:\\setup.bat", path, n, s.run))?;
            let up = letter.to_ascii_uppercase().to_string();
            if up != "C" && !mounted.contains(&up.as_str()) {
                bail!("{}: step {}: run {:?} uses drive {}: which is not mounted", path, n, s.run, up);
            }
            let lower = run.to_lowercase();
            if !SCRIPT_EXT.iter().any(|e| lower.ends_with(e)) {
                bail!("{}: step {}: run {:?} must end in .exe .bat .cmd .ps1 or .msi", path, n, s.run);
            }
            let expect_exit = s.expect_exit.unwrap_or(0);
            if let Some(w) = s.wait {
                if w <= 0 {
                    bail!("{}: step {}: wait must be a positive integer", path, n);
                }
            }
            let path_norm = format!("{}:\\{}", up, rest.trim_start_matches('\\'));
            steps.push(RunStep {
                on,
                as_,
                run: path_norm,
                args: s.args.clone().filter(|a| !a.is_empty()),
                expect_exit,
                wait: s.wait,
                keep_running: s.keep_running,
            });
        }
        if steps.is_empty() {
            bail!("{}: nothing to do (no steps)", path);
        }

        Ok(Run { lab: self.lab.clone(), name, wait, timeout_min, mounts, steps })
    }
}

/// Parse a DHCP `range` like `.100-.199` (last octets within the subnet) into `(start, end)`.
fn parse_dhcp_range(machine: &str, range: &str, path: &str) -> Result<DhcpOpts> {
    let bad = || anyhow::anyhow!("{}: machine {}: dhcp range {:?} must be \".<start>-.<end>\", e.g. .100-.199", path, machine, range);
    let (a, b) = range.split_once('-').ok_or_else(bad)?;
    let octet = |s: &str| -> Option<u8> { s.strip_prefix('.').unwrap_or(s).trim().parse().ok() };
    let (start, end) = (octet(a).ok_or_else(bad)?, octet(b).ok_or_else(bad)?);
    if start == 0 || start >= end {
        bail!("{}: machine {}: dhcp range {:?}: start must be 1-254 and below end", path, machine, range);
    }
    Ok(DhcpOpts { start, end })
}

/// Is a run file's `lab` value a path to a lab file (rather than a name in `labs/`)?
pub fn lab_is_path(lab: &str) -> bool {
    lab.contains('/') || lab.contains('\\') || lab.ends_with(".json")
}

/// `^([A-Za-z]):[\\/](.*)$` -> (letter, rest-with-backslashes)
fn split_ref(r: &str) -> Option<(char, &str)> {
    let mut it = r.chars();
    let letter = it.next().filter(char::is_ascii_alphabetic)?;
    if it.next() != Some(':') {
        return None;
    }
    match it.next() {
        Some('\\') | Some('/') => Some((letter, &r[3..])),
        _ => None,
    }
}

/// The JSON Schema of the lab and run files, as pretty JSON `(lab, run)`.
pub fn schemas() -> (String, String) {
    let settings = schemars::gen::SchemaSettings::draft07();
    let lab = settings.clone().into_generator().into_root_schema_for::<LabDef>();
    let run = settings.into_generator().into_root_schema_for::<RunDef>();
    (serde_json::to_string_pretty(&lab).unwrap(), serde_json::to_string_pretty(&run).unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lab(json: &str) -> Result<Lab> {
        LabDef::parse(json, "lab.json")?.resolve("lab.json")
    }

    const SCHOOL: &str = r#"{
        "domain": "lab.local", "netbios": "LAB", "admin_password": "WinTest!2026",
        "network": { "subnet": "10.77.30", "lan_port": 47030 },
        "machines": {
            "DC1": { "os": "2019", "role": "dc" },
            "CL1": { "os": "2012", "logon": "student1" },
            "CL2": { "os": "2016", "logon": "student2" }
        },
        "users": {
            "student1": { "password": "Pupil!2026", "groups": ["Students"] },
            "student2": { "password": "Pupil!2026", "groups": ["Students"] },
            "teacher":  { "password": "Teach!2026", "groups": ["Teachers"] }
        }
    }"#;

    #[test]
    fn resolves_school() {
        let l = lab(SCHOOL).unwrap();
        assert!(l.is_ad());
        assert_eq!(l.domain(), "lab.local");
        assert_eq!(l.netbios(), "LAB");
        assert_eq!(l.network.as_ref().unwrap().lan_port, 47030);
        assert_eq!(l.machines.len(), 3);
        let dc = l.dc().unwrap();
        assert_eq!((dc.name.as_str(), dc.ip.as_deref(), dc.mem), ("DC1", Some("10.77.30.10"), 3072));
        let members: Vec<_> = l.members().map(|m| (m.name.as_str(), m.ip.as_deref().unwrap(), m.mem, m.cpus)).collect();
        assert_eq!(members, vec![("CL1", "10.77.30.20", 4096, 2), ("CL2", "10.77.30.21", 4096, 2)]);
        assert_eq!(l.machine("CL1").unwrap().logon.as_deref(), Some("student1"));
        // users in declaration order, with groups
        let us: Vec<_> = l.users.iter().map(|u| (u.name.as_str(), u.groups.clone())).collect();
        assert_eq!(
            us,
            vec![
                ("student1", vec!["Students".to_string()]),
                ("student2", vec!["Students".to_string()]),
                ("teacher", vec!["Teachers".to_string()]),
            ]
        );
    }

    #[test]
    fn rejects_unknown_fields() {
        let e = lab(r#"{"domain":"d","netbios":"L","admin_password":"p","network":{"subnet":"10.0.0","lan_port":1},"machines":{"DC":{"os":"2019","role":"dc","frob":1}}}"#).unwrap_err();
        assert!(format!("{}", e).contains("unknown field"), "{}", e);
    }

    #[test]
    fn mode_follows_the_dc() {
        // a domain without a DC is an error (not silently a workgroup)
        let none =
            lab(r#"{"domain":"d","netbios":"L","network":{"subnet":"10.0.0","lan_port":1},"machines":{"A":{"os":"2019"}}}"#).unwrap_err();
        assert!(format!("{}", none).contains("domain/netbios is set but no machine has the \"ad-ds\" role"), "{}", none);
        let two = lab(r#"{"domain":"d","netbios":"L","network":{"subnet":"10.0.0","lan_port":1},"machines":{"A":{"os":"2019","role":"dc"},"B":{"os":"2019","role":"dc"}}}"#).unwrap_err();
        assert!(format!("{}", two).contains("at most one machine may have the \"ad-ds\" role (found 2)"), "{}", two);
        // a DC needs a domain and a network
        let nodomain = lab(r#"{"network":{"subnet":"10.0.0","lan_port":1},"machines":{"A":{"os":"2019","role":"dc"}}}"#).unwrap_err();
        assert!(format!("{}", nodomain).contains("needs \"domain\""), "{}", nodomain);
        let nonet = lab(r#"{"domain":"d","netbios":"L","machines":{"A":{"os":"2019","role":"dc"}}}"#).unwrap_err();
        assert!(format!("{}", nonet).contains("an AD lab needs \"network\""), "{}", nonet);
    }

    #[test]
    fn workgroup_labs() {
        // one machine: no network needed, default admin password, local users
        let one = lab(
            r#"{"machines":{"WS":{"os":"2019","logon":"tester"}},"users":{"tester":{"password":"Test!2026","groups":["Administrators"]}}}"#,
        )
        .unwrap();
        assert_eq!(one.mode, Mode::Workgroup);
        assert!(!one.is_ad() && one.dc().is_none() && one.network.is_none());
        assert_eq!(one.admin_password, DEFAULT_ADMIN_PASSWORD);
        assert_eq!(one.machine("WS").unwrap().ip, None);
        assert_eq!(one.users[0].groups, vec!["Administrators".to_string()]);
        // several machines need the LAN; they get .20, .21, ...
        let multi = lab(r#"{"machines":{"A":{"os":"2019"},"B":{"os":"2016"}}}"#).unwrap_err();
        assert!(format!("{}", multi).contains("several machines needs \"network\""), "{}", multi);
        let two = lab(r#"{"network":{"subnet":"10.0.0","lan_port":5},"machines":{"A":{"os":"2019"},"B":{"os":"2016"}}}"#).unwrap();
        assert_eq!(two.machine("B").unwrap().ip.as_deref(), Some("10.0.0.21"));
        // an ip without a network makes no sense
        let ip = lab(r#"{"machines":{"A":{"os":"2019","ip":"5"}}}"#).unwrap_err();
        assert!(format!("{}", ip).contains("ip needs a \"network\""), "{}", ip);
        // Windows rejects a password containing the user name: caught before boot
        let pw = lab(r#"{"machines":{"A":{"os":"2019"}},"users":{"appuser":{"password":"AppUser!2026"}}}"#).unwrap_err();
        assert!(format!("{}", pw).contains("the password contains the user name"), "{}", pw);
    }

    #[test]
    fn any_server_dc_any_member() {
        // the user's example: a 2012 R2 DC and a 2019 member server
        let l = lab(r#"{"domain":"lab.local","netbios":"LAB","network":{"subnet":"10.77.40","lan_port":47040},"machines":{"DC1":{"os":"2012","role":"dc"},"SRV1":{"os":"2019"}}}"#).unwrap();
        assert_eq!(l.dc().unwrap().os, "2012");
        assert_eq!(l.machine("SRV1").unwrap().logon, None);
    }

    #[test]
    fn roles_alias_and_resolution() {
        // role: "dc" is an alias for roles: ["ad-ds"]; the CA machine joins and runs ad-cs + iis
        let l = lab(r#"{"domain":"lab.local","netbios":"LAB","network":{"subnet":"10.77.50","lan_port":47050},
            "machines":{
              "DC1":{"os":"2019","role":"dc"},
              "CA1":{"os":"2012","roles":["ad-cs","iis"]},
              "CL1":{"os":"2016","role":"member","logon":"student"}
            },
            "users":{"student":{"password":"Pupil!2026"}}}"#)
        .unwrap();
        let dc = l.dc().unwrap();
        assert_eq!(dc.roles, vec![LabRole::AdDs]);
        let ca = l.machine("CA1").unwrap();
        assert_eq!(ca.role, Role::Member);
        assert!(ca.has_role(LabRole::AdCs) && ca.has_role(LabRole::Iis));
        // ad-cs/iis options defaulted
        assert_eq!(ca.ad_cs.as_ref().unwrap().ca_name, "LAB-ROOT-CA");
        assert_eq!(ca.ad_cs.as_ref().unwrap().key_length, 2048);
        assert!(ca.ad_cs.as_ref().unwrap().autoenroll);
        assert_eq!(ca.iis.as_ref().unwrap().site_path, "C:\\inetpub\\wwwroot");
        // role: "member" and no roles both mean no server roles
        assert!(l.machine("CL1").unwrap().roles.is_empty());
    }

    #[test]
    fn dhcp_range_and_options() {
        let l = lab(r#"{"domain":"d","netbios":"LAB","network":{"subnet":"10.9.9","lan_port":1},
            "machines":{"DC1":{"os":"2019","roles":["ad-ds","dhcp"],"dhcp":{"range":".50-.80"}}}}"#)
        .unwrap();
        let dc = l.dc().unwrap();
        let d = dc.dhcp.as_ref().unwrap();
        assert_eq!((d.start, d.end), (50, 80));
        // a bad range is rejected
        let bad = lab(r#"{"domain":"d","netbios":"L","network":{"subnet":"10.0.0","lan_port":1},"machines":{"DC":{"os":"2019","roles":["ad-ds","dhcp"],"dhcp":{"range":".200-.100"}}}}"#).unwrap_err();
        assert!(format!("{}", bad).contains("start must be"), "{}", bad);
    }

    #[test]
    fn rejects_role_and_roles_together() {
        let e = lab(r#"{"domain":"d","netbios":"L","network":{"subnet":"10.0.0","lan_port":1},"machines":{"DC":{"os":"2019","role":"dc","roles":["ad-ds"]}}}"#).unwrap_err();
        assert!(format!("{}", e).contains("set either \"role\" or \"roles\""), "{}", e);
    }

    #[test]
    fn rejects_adcs_in_workgroup() {
        let e = lab(r#"{"network":{"subnet":"10.0.0","lan_port":1},"machines":{"S":{"os":"2019","roles":["ad-cs"]}}}"#).unwrap_err();
        assert!(format!("{}", e).contains("the \"ad-cs\" role needs an AD lab"), "{}", e);
    }

    #[test]
    fn setup_scripts_resolve_and_validate() {
        let l =
            lab(r#"{"machines":{"S":{"os":"2019","setup":[{"run":"./setup/a.ps1"},{"run":"setup/b.bat","args":"--go","as":"operator"}]}},
            "users":{"operator":{"password":"Pass!2026x"}}}"#)
            .unwrap();
        let s = &l.machine("S").unwrap().setup;
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].run, "setup/a.ps1"); // leading "./" normalized away
        assert_eq!(s[0].as_, StepAs::System);
        assert_eq!(s[1].as_, StepAs::User("operator".into()));
        assert_eq!(s[1].args.as_deref(), Some("--go"));
        // a non-script extension is rejected
        let bad = lab(r#"{"machines":{"S":{"os":"2019","setup":[{"run":"./x.txt"}]}}}"#).unwrap_err();
        assert!(format!("{}", bad).contains("must end in"), "{}", bad);
        // an absolute / drive path is rejected
        let abs = lab(r#"{"machines":{"S":{"os":"2019","setup":[{"run":"C:/x.ps1"}]}}}"#).unwrap_err();
        assert!(format!("{}", abs).contains("must be relative"), "{}", abs);
    }

    #[test]
    fn rejects_unknown_logon_and_dup_ip() {
        let bad_logon = lab(r#"{"domain":"d","netbios":"L","admin_password":"p","network":{"subnet":"10.0.0","lan_port":1},"machines":{"DC":{"os":"2019","role":"dc"},"CL":{"os":"2019","logon":"ghost"}}}"#).unwrap_err();
        assert!(format!("{}", bad_logon).contains("logon user \"ghost\" is not in users"), "{}", bad_logon);
        let dup = lab(r#"{"domain":"d","netbios":"L","admin_password":"p","network":{"subnet":"10.0.0","lan_port":1},"machines":{"DC":{"os":"2019","role":"dc","ip":"10.0.0.10"},"CL":{"os":"2019","ip":"10"}}}"#).unwrap_err();
        assert!(format!("{}", dup).contains("share IP 10.0.0.10"), "{}", dup);
    }

    #[test]
    fn explicit_ip_octet_and_full() {
        let l = lab(r#"{"domain":"d","netbios":"L","admin_password":"p","network":{"subnet":"10.0.0","lan_port":1},"machines":{"DC":{"os":"2019","role":"dc"},"A":{"os":"2019","ip":"55"},"B":{"os":"2019","ip":"192.168.1.9"}}}"#).unwrap();
        assert_eq!(l.machine("A").unwrap().ip.as_deref(), Some("10.0.0.55"));
        assert_eq!(l.machine("B").unwrap().ip.as_deref(), Some("192.168.1.9"));
    }

    fn run_against(lab_json: &str, run_json: &str) -> Result<Run> {
        let l = lab(lab_json).unwrap();
        RunDef::parse(run_json, "run.json")?.resolve("run.json", "run", &l, |h| h.to_string())
    }

    const SMOKE: &str = r#"{
        "lab": "school", "name": "smoke", "wait": 60, "timeout_min": 60,
        "mount": { "X": "./app" },
        "steps": [
            { "on": "all", "as": "system", "run": "X:/setup.bat" },
            { "on": "CL1", "as": "student1", "run": "X:/smoke.exe" },
            { "on": ["CL1", "CL2"], "run": "X:/smoke.exe", "args": "--quiet" },
            { "on": "DC1", "as": "system", "run": "X:/check.ps1", "expect_exit": 0 }
        ]
    }"#;

    #[test]
    fn resolves_run() {
        let r = run_against(SCHOOL, SMOKE).unwrap();
        assert_eq!(r.lab, "school");
        assert_eq!(r.name, "smoke");
        assert_eq!(r.mounts, vec![("X".to_string(), "./app".to_string())]);
        assert_eq!(r.steps.len(), 4);
        // "all" expands to the members in lab order (not the DC)
        assert_eq!(r.steps[0].on, vec!["CL1", "CL2"]);
        assert_eq!(r.steps[0].as_, StepAs::System);
        assert_eq!(r.steps[0].run, "X:\\setup.bat");
        assert_eq!(r.steps[1].on, vec!["CL1"]);
        assert_eq!(r.steps[1].as_, StepAs::User("student1".into()));
        // default `as` = logon
        assert_eq!(r.steps[2].on, vec!["CL1", "CL2"]);
        assert_eq!(r.steps[2].as_, StepAs::Logon);
        assert_eq!(r.steps[2].args.as_deref(), Some("--quiet"));
        assert_eq!(r.steps[3].on, vec!["DC1"]);
    }

    #[test]
    fn run_rejects_bad_targets() {
        let bad_on = run_against(SCHOOL, r#"{"lab":"school","mount":{"X":"./a"},"steps":[{"on":"NOPE","run":"X:/a.bat"}]}"#).unwrap_err();
        assert!(format!("{}", bad_on).contains("is not a machine"), "{}", bad_on);
        let bad_as = run_against(SCHOOL, r#"{"lab":"school","mount":{"X":"./a"},"steps":[{"on":"CL1","as":"ghost","run":"X:/a.bat"}]}"#)
            .unwrap_err();
        assert!(format!("{}", bad_as).contains("is not a user"), "{}", bad_as);
        // default `as` on the DC (no logon) must be explicit
        let dc_logon = run_against(SCHOOL, r#"{"lab":"school","mount":{"X":"./a"},"steps":[{"on":"DC1","run":"X:/a.bat"}]}"#).unwrap_err();
        assert!(format!("{}", dc_logon).contains("needs an explicit \"as\""), "{}", dc_logon);
    }

    #[test]
    fn run_rejects_bad_mounts_and_scripts() {
        let dup = run_against(SCHOOL, r#"{"lab":"school","mount":{"C":"./a"},"steps":[{"on":"CL1","as":"system","run":"C:/a.bat"}]}"#)
            .unwrap_err();
        assert!(format!("{}", dup).contains("reserved"), "{}", dup);
        let ext = run_against(SCHOOL, r#"{"lab":"school","mount":{"X":"./a"},"steps":[{"on":"CL1","as":"system","run":"X:/a.zip"}]}"#)
            .unwrap_err();
        assert!(format!("{}", ext).contains("must end in"), "{}", ext);
        let unm = run_against(SCHOOL, r#"{"lab":"school","mount":{"X":"./a"},"steps":[{"on":"CL1","as":"system","run":"Y:/a.bat"}]}"#)
            .unwrap_err();
        assert!(format!("{}", unm).contains("not mounted"), "{}", unm);
        let msi =
            run_against(SCHOOL, r#"{"lab":"school","mount":{"X":"./a"},"steps":[{"on":"CL1","as":"system","run":"X:/a.msi"}]}"#).unwrap();
        assert_eq!(msi.steps[0].run, "X:\\a.msi");
    }

    /// Every lab in labs/ and every example run in examples/configs/ loads and resolves.
    #[test]
    fn repo_labs_and_runs_resolve() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut labs = std::collections::HashMap::new();
        for (file, text) in crate::assets::LABS {
            let l = LabDef::parse(text, file).and_then(|d| d.resolve(file)).unwrap_or_else(|e| panic!("{}: {:#}", file, e));
            labs.insert(file.trim_end_matches(".json").to_string(), l);
        }
        for want in ["school", "2019", "mixed", "single-2019"] {
            assert!(labs.contains_key(want), "no labs/{}.json: {:?}", want, labs.keys());
        }
        // examples/ is not in the published package: check the runs where it exists
        let Ok(rd) = std::fs::read_dir(root.join("examples/configs")) else { return };
        let mut runs = 0;
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "json") {
                let def = RunDef::parse(&std::fs::read_to_string(&p).unwrap(), &p.display().to_string()).unwrap();
                let lab = labs.get(&def.lab).unwrap_or_else(|| panic!("{}: no labs/{}.json", p.display(), def.lab));
                def.resolve(&p.display().to_string(), "x", lab, |h| h.to_string()).unwrap_or_else(|e| panic!("{}: {:#}", p.display(), e));
                runs += 1;
            }
        }
        assert!(runs >= 4, "{} example runs", runs);
    }

    #[test]
    fn schema_is_generated() {
        let (lab, run) = schemas();
        assert!(lab.contains("\"machines\""), "{}", lab);
        assert!(lab.contains("admin_password"));
        assert!(run.contains("\"steps\""));
        assert!(run.contains("expect_exit"));
        // both are valid JSON
        let _: serde_json::Value = serde_json::from_str(&lab).unwrap();
        let _: serde_json::Value = serde_json::from_str(&run).unwrap();
    }
}
