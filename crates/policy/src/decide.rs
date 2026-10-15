//! The allow or deny decision for one tool call.
//!
//! A missing tool is denied. A tool with no effects is denied. An effect
//! outside the declaration is denied. Paths, URLs, and external services
//! must fall inside the declaration.

use crate::descriptor::{ToolDescriptor, ToolPolicy, VaultScope};
use crate::effect::Effect;
use crate::path::{PathError, VaultPath, normalize_vault_path};
use crate::url::{ToolUrl, UrlError, normalize_url};

/// One call to check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    /// MCP or tool server name.
    pub server: String,
    /// Bare tool name.
    pub tool: String,
    /// The effect this call performs.
    pub effect: Effect,
    /// Vault paths the call touches.
    pub vault_paths: Vec<String>,
    /// URLs the call touches.
    pub urls: Vec<String>,
    /// External services the call names.
    pub external_services: Vec<String>,
}

/// A call that matched a declaration. Paths and URLs are normalized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allowed {
    /// Server name from the call.
    pub server: String,
    /// Tool name from the call.
    pub tool: String,
    /// The effect that was declared.
    pub effect: Effect,
    /// Normalized vault paths.
    pub vault_paths: Vec<VaultPath>,
    /// Normalized URLs.
    pub urls: Vec<ToolUrl>,
    /// External services, copied from the call.
    pub external_services: Vec<String>,
}

/// Why a call was denied.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Deny {
    /// The policy has no declaration for this tool.
    #[error("tool {server}/{tool} is undeclared")]
    Undeclared { server: String, tool: String },
    /// The declaration lists no effects.
    #[error("tool {server}/{tool} declares no effects")]
    UndeclaredEffects { server: String, tool: String },
    /// The call's effect is outside the declaration.
    #[error("tool {server}/{tool} does not declare {effect:?}")]
    EffectNotDeclared {
        server: String,
        tool: String,
        effect: Effect,
    },
    /// A path-addressed tool was called with no vault path.
    #[error("tool {server}/{tool} needs a vault path")]
    MissingVaultPath { server: String, tool: String },
    /// The tool's scope accepts no vault path.
    #[error("tool {server}/{tool} does not touch the vault")]
    UnexpectedVaultPath { server: String, tool: String },
    /// A vault path failed normalization.
    #[error(transparent)]
    Path(#[from] PathError),
    /// A URL failed normalization.
    #[error(transparent)]
    Url(#[from] UrlError),
    /// A URL is outside the allowlist.
    #[error("url {url} is outside the declared set")]
    UrlNotAllowed { url: String },
    /// An external service is outside the declaration.
    #[error("external service {service:?} is undeclared")]
    ServiceNotDeclared { service: String },
}

impl ToolPolicy {
    /// Allow the call only when the tool, the effect, and every touch are declared.
    pub fn authorize(&self, call: &ToolCall) -> Result<Allowed, Deny> {
        let descriptor = lookup(self, call)?;
        require_effect(descriptor, call)?;
        let vault_paths = check_vault(descriptor, &call.vault_paths)?;
        let urls = check_urls(descriptor, &call.urls)?;
        let external_services = check_services(descriptor, &call.external_services)?;
        Ok(Allowed {
            server: call.server.clone(),
            tool: call.tool.clone(),
            effect: call.effect,
            vault_paths,
            urls,
            external_services,
        })
    }
}

fn lookup<'a>(policy: &'a ToolPolicy, call: &ToolCall) -> Result<&'a ToolDescriptor, Deny> {
    if let Some(descriptor) = policy.get(&call.server, &call.tool) {
        return Ok(descriptor);
    }
    Err(missing_tool(call))
}

fn missing_tool(call: &ToolCall) -> Deny {
    Deny::Undeclared {
        server: call.server.clone(),
        tool: call.tool.clone(),
    }
}

fn require_effect(descriptor: &ToolDescriptor, call: &ToolCall) -> Result<(), Deny> {
    if descriptor.effects().is_empty() {
        return empty_effects(call);
    }
    if descriptor.effects().contains(&call.effect) {
        return Ok(());
    }
    Err(Deny::EffectNotDeclared {
        server: call.server.clone(),
        tool: call.tool.clone(),
        effect: call.effect,
    })
}

fn empty_effects(call: &ToolCall) -> Result<(), Deny> {
    Err(Deny::UndeclaredEffects {
        server: call.server.clone(),
        tool: call.tool.clone(),
    })
}

fn check_vault(descriptor: &ToolDescriptor, paths: &[String]) -> Result<Vec<VaultPath>, Deny> {
    match descriptor.vault() {
        VaultScope::None | VaultScope::VaultWide => refuse_paths(descriptor, paths),
        VaultScope::Zones { zones, from_arg } => {
            normalize_zones(descriptor, zones, from_arg.as_deref(), paths)
        }
    }
}

fn refuse_paths(descriptor: &ToolDescriptor, paths: &[String]) -> Result<Vec<VaultPath>, Deny> {
    if paths.is_empty() {
        Ok(Vec::new())
    } else {
        Err(Deny::UnexpectedVaultPath {
            server: descriptor.server().to_owned(),
            tool: descriptor.name().to_owned(),
        })
    }
}

fn normalize_zones(
    descriptor: &ToolDescriptor,
    zones: &[String],
    from_arg: Option<&str>,
    paths: &[String],
) -> Result<Vec<VaultPath>, Deny> {
    if from_arg.is_some() && paths.is_empty() {
        return Err(Deny::MissingVaultPath {
            server: descriptor.server().to_owned(),
            tool: descriptor.name().to_owned(),
        });
    }
    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        out.push(normalize_vault_path(path, zones)?);
    }
    Ok(out)
}

fn check_urls(descriptor: &ToolDescriptor, urls: &[String]) -> Result<Vec<ToolUrl>, Deny> {
    let zones = descriptor.vault().zone_list();
    let mut out = Vec::with_capacity(urls.len());
    for url in urls {
        let normalized = normalize_url(url, zones)?;
        if !url_is_allowed(descriptor.urls(), &normalized) {
            return Err(Deny::UrlNotAllowed { url: url.clone() });
        }
        out.push(normalized);
    }
    Ok(out)
}

fn url_is_allowed(allow: &[ToolUrl], call: &ToolUrl) -> bool {
    allow.iter().any(|entry| entry.covers(call))
}

fn check_services(descriptor: &ToolDescriptor, services: &[String]) -> Result<Vec<String>, Deny> {
    let mut out = Vec::with_capacity(services.len());
    for service in services {
        if !service_is_declared(descriptor, service) {
            return Err(Deny::ServiceNotDeclared {
                service: service.clone(),
            });
        }
        out.push(service.clone());
    }
    Ok(out)
}

fn service_is_declared(descriptor: &ToolDescriptor, service: &str) -> bool {
    descriptor.external_services().contains(service)
}

#[cfg(test)]
#[path = "decide_tests.rs"]
mod decide_tests;
