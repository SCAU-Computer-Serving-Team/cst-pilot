use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use pi_ai::{Models, Provider};

use crate::virtual_models::{VirtualModelDefinition, create_virtual_model, with_virtual_models};

#[derive(Clone)]
pub(crate) enum CatalogChange {
    Provider(Provider),
    RemoveProvider(String),
    Virtual(VirtualModelDefinition),
    RemoveVirtual(String, String),
}

/// Serializes queued and live catalog changes. Baselines allow unregistering an
/// extension override without deleting a provider supplied by the application.
#[derive(Default)]
pub(crate) struct ExtensionCatalog {
    pub providers: BTreeMap<String, Provider>,
    pub virtual_models: BTreeMap<(String, String), VirtualModelDefinition>,
    models: Option<Arc<Models>>,
    baselines: BTreeMap<String, Option<Provider>>,
    touched: BTreeSet<String>,
}

impl ExtensionCatalog {
    pub fn models(&self) -> Option<Arc<Models>> {
        self.models.clone()
    }

    pub fn bind(&mut self, models: Arc<Models>) {
        if self
            .models
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &models))
        {
            return;
        }
        self.detach();
        self.models = Some(models);
        for id in self.touched.clone() {
            self.publish(&id);
        }
    }

    pub fn detach(&mut self) {
        if let Some(models) = self.models.take() {
            for (id, baseline) in std::mem::take(&mut self.baselines) {
                match baseline {
                    Some(provider) => models.set_provider(provider),
                    None => models.delete_provider(&id),
                }
            }
        }
    }

    pub fn apply(&mut self, change: CatalogChange) {
        let id = match change {
            CatalogChange::Provider(provider) => {
                let id = provider.id().to_owned();
                self.providers.insert(id.clone(), provider);
                id
            }
            CatalogChange::RemoveProvider(id) => {
                self.providers.remove(&id);
                self.virtual_models
                    .retain(|(provider, _), _| provider != &id);
                id
            }
            CatalogChange::Virtual(def) => {
                let id = def.provider.clone();
                self.virtual_models
                    .insert((id.clone(), def.id.clone()), def);
                id
            }
            CatalogChange::RemoveVirtual(provider, id) => {
                self.virtual_models.remove(&(provider.clone(), id));
                provider
            }
        };
        self.touched.insert(id.clone());
        self.publish(&id);
    }

    fn publish(&mut self, id: &str) {
        let Some(models) = &self.models else { return };
        let baseline = self
            .baselines
            .entry(id.to_owned())
            .or_insert_with(|| models.get_provider(id));
        let base = self.providers.get(id).cloned().or_else(|| baseline.clone());
        let virtuals: Vec<_> = self
            .virtual_models
            .values()
            .filter(|def| def.provider == id)
            .map(create_virtual_model)
            .collect();
        let provider = if virtuals.is_empty() {
            base
        } else {
            Some(with_virtual_models(id, base, virtuals))
        };
        match provider {
            Some(provider) => models.set_provider(provider),
            None => models.delete_provider(id),
        }
    }
}
