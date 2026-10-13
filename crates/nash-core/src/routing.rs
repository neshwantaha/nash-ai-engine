//! Request routing: choosing which provider should handle a chat request.
//!
//! Routing is a pure ranking over lightweight [`ProviderView`]s built from the
//! live registry. Preference order, the local/privacy exception and declared
//! capabilities all contribute to a score; providers that plainly cannot serve
//! a request are excluded entirely.

use crate::capability::Support;
use crate::config::RoutingConfig;

/// The routing-relevant facts about a provider, snapshot from the registry.
#[derive(Debug, Clone)]
pub struct ProviderView {
    pub id: String,
    pub local: bool,
    pub requires_api_key: bool,
    /// The provider's configured/selected model, if any.
    pub default_model: Option<String>,
    /// All model ids this provider can serve (config list + default).
    pub models: Vec<String>,
    pub tools: Support,
    pub vision: Support,
}

impl ProviderView {
    /// Snapshot a provider into a routing view.
    pub fn from_provider(p: &dyn crate::provider::Provider, models: Vec<String>) -> Self {
        let info = p.info();
        let caps = p.capabilities();
        let default = p.default_model();
        let mut all = default.clone().into_iter().collect::<Vec<_>>();
        for m in models {
            if !all.contains(&m) {
                all.push(m);
            }
        }
        Self {
            id: info.id.clone(),
            local: info.local,
            requires_api_key: info.requires_api_key,
            default_model: default,
            models: all,
            tools: caps.tools,
            vision: caps.vision,
        }
    }
}

/// The constraints of a request that routing must respect.
#[derive(Debug, Clone, Default)]
pub struct RouteRequest {
    /// Requested model id. Providers that cannot serve it are excluded; the
    /// first candidate's default model may substitute when this is `None`.
    pub model: Option<String>,
    /// When true the request must not leave this machine.
    pub privacy: bool,
    pub requires_tools: bool,
    pub requires_vision: bool,
}

impl RouteRequest {
    pub fn simple(model: Option<String>) -> Self {
        Self {
            model,
            ..Default::default()
        }
    }
}

/// Ranks providers for a request. Returns provider ids in descending order of
/// suitability. Ties are broken by `preferred` order (earlier wins).
pub fn rank(
    routing: &RoutingConfig,
    candidates: &[ProviderView],
    req: &RouteRequest,
    preferred: &[String],
) -> Vec<String> {
    let mut eligible: Vec<&ProviderView> = candidates
        .iter()
        .filter(|v| {
            if req.privacy && routing.privacy_only_local && !v.local {
                return false;
            }
            if req.requires_tools && v.tools.is_known_unsupported() {
                return false;
            }
            if req.requires_vision && v.vision.is_known_unsupported() {
                return false;
            }
            if let Some(m) = &req.model {
                if !v.models.contains(m) {
                    return false;
                }
            }
            true
        })
        .collect();

    eligible.sort_by(|a, b| {
        let sa = score(a, req, preferred);
        let sb = score(b, req, preferred);
        sb.cmp(&sa)
            .then_with(|| preferred_position(a.id.as_str(), preferred).cmp(&preferred_position(b.id.as_str(), preferred)))
    });
    eligible.into_iter().map(|v| v.id.clone()).collect()
}

fn preferred_position(id: &str, preferred: &[String]) -> usize {
    preferred
        .iter()
        .position(|p| p == id)
        .unwrap_or(usize::MAX)
}

fn score(v: &ProviderView, req: &RouteRequest, preferred: &[String]) -> i64 {
    let mut s = 0_i64;

    if req.privacy {
        s += if v.local { 1_000 } else { -1_000 };
    }

    if let Some(pos) = preferred.iter().position(|p| *p == v.id) {
        s += 500 - pos as i64;
    }

    if v.local {
        s += 20;
    }
    if !v.requires_api_key {
        s += 10;
    }

    match &req.model {
        Some(m) => {
            if v.models.contains(m) {
                s += 100;
            }
        }
        None => {
            if v.default_model.is_some() {
                s += 25;
            }
        }
    }

    if req.requires_tools && v.tools.is_supported() {
        s += 30;
    }
    if req.requires_vision && v.vision.is_supported() {
        s += 30;
    }

    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(id: &str, local: bool, requires_key: bool) -> ProviderView {
        ProviderView {
            id: id.into(),
            local,
            requires_api_key: requires_key,
            default_model: None,
            models: vec![id.into()],
            tools: Support::Unknown,
            vision: Support::Unknown,
        }
    }

    #[test]
    fn rank_prefers_models_that_match() {
        let routing = RoutingConfig::default();
        let mut a = view("a", false, true);
        a.models.push("gpt-4o".into());
        let mut b = view("b", false, true);
        b.models.push("claude-3".into());
        let ranked = rank(
            &routing,
            &[a, b],
            &RouteRequest::simple(Some("gpt-4o".into())),
            &[],
        );
        assert_eq!(ranked, vec!["a"]);
    }

    #[test]
    fn users_preferred_provider_wins_ties() {
        let routing = RoutingConfig::default();
        let a = view("a", false, true);
        let b = view("b", false, true);
        let ranked = rank(&routing, &[a, b], &RouteRequest::default(), &["b".to_string()]);
        assert_eq!(ranked, vec!["b", "a"]);
    }

    #[test]
    fn privacy_mode_drops_remote_providers() {
        let routing = RoutingConfig::default();
        let cloud = view("cloud", false, true);
        let local = view("local", true, false);
        let ranked = rank(
            &routing,
            &[cloud, local],
            &RouteRequest {
                privacy: true,
                ..Default::default()
            },
            &[],
        );
        assert_eq!(ranked, vec!["local"]);
    }
}