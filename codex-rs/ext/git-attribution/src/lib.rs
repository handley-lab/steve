mod world_state;

use std::sync::Arc;

use codex_extension_api::ContextContributor;
use codex_extension_api::ExtensionFuture;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::WorldStateContributionInput;
use codex_extension_api::WorldStateSectionContribution;

use crate::world_state::git_attribution_world_state_section;

/// Contributes model instructions for agent-created git commits and pull requests.
struct GitAttributionExtension;

impl ContextContributor for GitAttributionExtension {
    fn contribute_world_state<'a>(
        &'a self,
        _input: WorldStateContributionInput<'a>,
    ) -> ExtensionFuture<'a, Vec<WorldStateSectionContribution>> {
        Box::pin(async move { vec![git_attribution_world_state_section(/*enabled*/ false)] })
    }
}

/// Installs the git-attribution contributor into the extension registry.
pub fn install<C: Sync>(registry: &mut ExtensionRegistryBuilder<C>) {
    registry.prompt_contributor(Arc::new(GitAttributionExtension));
}
