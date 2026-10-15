//! A tool declaration and the policy built from a set of declarations.
//!
//! [`ToolPolicy::from_declarations`] stores a tool that lists no effects.
//! [`crate::ToolPolicy::authorize`] denies that tool. The TOML loader refuses
//! the file instead, so an operator sees the gap before a process starts.

use std::collections::{BTreeMap, BTreeSet};

use crate::effect::Effect;
use crate::path::{PathError, validate_zone_name};
use crate::segment::has_ascii_control;
use crate::url::{ToolUrl, normalize_url};

/// Where a tool may touch the vault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultScope {
    /// The tool does not take a vault path.
    None,
    /// Call paths must normalize into `zones`.
    ///
    /// An empty `zones` list accepts any well-formed zone. `from_arg` is the
    /// legacy argument name when the zone comes from the call. A `from_arg`
    /// tool must receive at least one path.
    Zones {
        /// Allowed zones. Empty means any well-formed zone.
        zones: Vec<String>,
        /// Argument name from `zone_from_arg`, when the shim built this scope.
        from_arg: Option<String>,
    },
    /// The tool mutates vault-wide state and takes no zone path.
    VaultWide,
}

impl VaultScope {
    /// A fixed zone list. The zone does not come from a call argument.
    pub fn zones(zones: Vec<String>) -> Self {
        Self::Zones {
            zones,
            from_arg: None,
        }
    }

    /// Any well-formed zone, read from `arg`.
    pub fn from_arg(arg: impl Into<String>) -> Self {
        Self::Zones {
            zones: Vec::new(),
            from_arg: Some(arg.into()),
        }
    }

    pub(crate) fn zone_list(&self) -> &[String] {
        match self {
            Self::Zones { zones, .. } => zones,
            Self::None | Self::VaultWide => &[],
        }
    }
}

/// One tool, before the policy checks names, zones, and URLs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    /// MCP or tool server name.
    pub server: String,
    /// Bare tool name.
    pub name: String,
    /// Effects this tool performs. Empty means the tool is undeclared.
    pub effects: BTreeSet<Effect>,
    /// Vault paths this tool may touch.
    pub vault: VaultScope,
    /// Allowlisted URLs. Each entry is normalized at compile time.
    pub urls: Vec<String>,
    /// External services this tool may name.
    pub external_services: BTreeSet<String>,
}

/// A compiled tool. Fields stay private so a caller uses [`ToolPolicy`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolDescriptor {
    server: String,
    name: String,
    effects: BTreeSet<Effect>,
    vault: VaultScope,
    urls: Vec<ToolUrl>,
    external_services: BTreeSet<String>,
}

impl ToolDescriptor {
    /// Server name.
    pub fn server(&self) -> &str {
        &self.server
    }

    /// Bare tool name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Declared effects. Empty means authorize denies the tool.
    pub fn effects(&self) -> &BTreeSet<Effect> {
        &self.effects
    }

    /// Vault scope.
    pub fn vault(&self) -> &VaultScope {
        &self.vault
    }

    /// Normalized allowlisted URLs.
    pub fn urls(&self) -> &[ToolUrl] {
        &self.urls
    }

    /// External service names. Match is case-sensitive.
    pub fn external_services(&self) -> &BTreeSet<String> {
        &self.external_services
    }
}

/// The tool catalog used by [`crate::ToolPolicy::authorize`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolPolicy {
    tools: BTreeMap<(String, String), ToolDescriptor>,
}

impl ToolPolicy {
    /// Compile declarations. A tool with no effects is stored and then denied.
    pub fn from_declarations(decls: Vec<Declaration>) -> Result<Self, PolicyError> {
        let mut tools = BTreeMap::new();
        for decl in decls {
            let descriptor = compile_one(decl)?;
            let key = (descriptor.server.clone(), descriptor.name.clone());
            if tools.insert(key.clone(), descriptor).is_some() {
                return Err(PolicyError::Duplicate {
                    server: key.0,
                    name: key.1,
                });
            }
        }
        Ok(Self { tools })
    }

    /// The compiled descriptor, when this server and tool were declared.
    pub fn get(&self, server: &str, tool: &str) -> Option<&ToolDescriptor> {
        self.tools.get(&(server.to_owned(), tool.to_owned()))
    }

    /// Number of declared tools, including tools whose effect set is empty.
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// No tools are declared. Every call is denied.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

/// Why a declaration or a TOML file was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    /// The TOML loader found a tool with no effects.
    #[error("tool {server}/{name} declares no effects")]
    UndeclaredEffects { server: String, name: String },
    /// Two declarations share a server and a tool name.
    #[error("duplicate tool {server}/{name}")]
    Duplicate { server: String, name: String },
    /// The server name is empty.
    #[error("server name is empty")]
    EmptyServer,
    /// The tool name is empty.
    #[error("tool name is empty")]
    EmptyName,
    /// A name contains an ASCII control character.
    #[error("a tool name contains a control character")]
    ControlCharacter,
    /// A vault zone failed validation.
    #[error(transparent)]
    Path(#[from] PathError),
    /// An allowlisted URL failed normalization.
    #[error(transparent)]
    Url(#[from] crate::url::UrlError),
    /// Two zones differ only by ASCII case.
    #[error("zones {a:?} and {b:?} differ only by case")]
    ZoneCaseCollision { a: String, b: String },
    /// The same zone is listed twice.
    #[error("zone {zone:?} is listed twice")]
    DuplicateZone { zone: String },
    /// `vault` disagrees with `zones` or `from_arg`.
    #[error("vault scope does not match its fields")]
    ScopeMismatch,
    /// The argument name is empty or not a single token.
    #[error("path argument name is invalid")]
    BadArg,
    /// An external service name is empty.
    #[error("external service name is empty")]
    EmptyService,
    /// An external service name contains a control character.
    #[error("external service name contains a control character")]
    ServiceControl,
    /// TOML did not match the descriptor schema.
    #[error("toml: {0}")]
    Toml(String),
}

fn compile_one(decl: Declaration) -> Result<ToolDescriptor, PolicyError> {
    validate_identity(&decl.server, &decl.name)?;
    validate_zones(decl.vault.zone_list())?;
    validate_from_arg(&decl.vault)?;
    validate_services(&decl.external_services)?;
    let urls = normalize_declared_urls(&decl.urls, decl.vault.zone_list())?;
    Ok(ToolDescriptor {
        server: decl.server,
        name: decl.name,
        effects: decl.effects,
        vault: decl.vault,
        urls,
        external_services: decl.external_services,
    })
}

fn validate_identity(server: &str, name: &str) -> Result<(), PolicyError> {
    if server.is_empty() {
        return Err(PolicyError::EmptyServer);
    }
    if name.is_empty() {
        return Err(PolicyError::EmptyName);
    }
    if has_ascii_control(server) || has_ascii_control(name) {
        return Err(PolicyError::ControlCharacter);
    }
    Ok(())
}

fn validate_zones(zones: &[String]) -> Result<(), PolicyError> {
    for zone in zones {
        validate_zone_name(zone)?;
    }
    refuse_duplicate_zones(zones)?;
    refuse_zone_case_collision(zones)
}

fn refuse_duplicate_zones(zones: &[String]) -> Result<(), PolicyError> {
    let mut seen = BTreeSet::new();
    for zone in zones {
        if !seen.insert(zone.as_str()) {
            return Err(PolicyError::DuplicateZone { zone: zone.clone() });
        }
    }
    Ok(())
}

fn refuse_zone_case_collision(zones: &[String]) -> Result<(), PolicyError> {
    for (index, left) in zones.iter().enumerate() {
        if let Some(right) = case_collision(left, &zones[index + 1..]) {
            return Err(PolicyError::ZoneCaseCollision {
                a: left.clone(),
                b: right,
            });
        }
    }
    Ok(())
}

fn case_collision(left: &str, rest: &[String]) -> Option<String> {
    rest.iter()
        .find(|right| right.eq_ignore_ascii_case(left) && right.as_str() != left)
        .cloned()
}

fn validate_from_arg(vault: &VaultScope) -> Result<(), PolicyError> {
    let VaultScope::Zones {
        from_arg: Some(arg),
        ..
    } = vault
    else {
        return Ok(());
    };
    if arg.is_empty() || has_ascii_control(arg) || arg_has_separator(arg) {
        return Err(PolicyError::BadArg);
    }
    Ok(())
}

fn arg_has_separator(arg: &str) -> bool {
    arg.contains('/') || arg.contains('\\') || arg.contains(':') || arg.contains(' ')
}

fn validate_services(services: &BTreeSet<String>) -> Result<(), PolicyError> {
    for service in services {
        if service.is_empty() {
            return Err(PolicyError::EmptyService);
        }
        if has_ascii_control(service) {
            return Err(PolicyError::ServiceControl);
        }
    }
    Ok(())
}

fn normalize_declared_urls(urls: &[String], zones: &[String]) -> Result<Vec<ToolUrl>, PolicyError> {
    let mut out = Vec::with_capacity(urls.len());
    for url in urls {
        out.push(normalize_url(url, zones)?);
    }
    Ok(out)
}
