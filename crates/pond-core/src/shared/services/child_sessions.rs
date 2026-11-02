//! Which parent session a live child agent works for, and at what scope, keyed by the child's
//! engine session id. A tool server reads the id Goose stamps into `_meta`, so it finds a helper's
//! work and whose data it may touch without the model naming either. Leases unbind on drop; never
//! persisted.
use crate::user_data::domain::profile::ProfileScope;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock, Weak};

struct LiveChild {
    parent: String,
    scope: ProfileScope,
}

/// Live children, by child engine session id.
#[derive(Default)]
pub struct ChildSessionRegistry {
    children: RwLock<HashMap<String, LiveChild>>,
}

impl ChildSessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records that `child` works for `parent` at `scope`, the child's own clamped scope rather
    /// than the parent's, until the returned lease drops.
    pub fn bind(
        self: &Arc<Self>,
        child: &str,
        parent: &str,
        scope: ProfileScope,
    ) -> ChildSessionLease {
        self.children
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                child.to_string(),
                LiveChild {
                    parent: parent.to_string(),
                    scope,
                },
            );
        ChildSessionLease {
            registry: Arc::downgrade(self),
            child: child.to_string(),
        }
    }

    /// The parent a live child works for; `None` for anything that is not a live child.
    pub fn parent_of(&self, child: &str) -> Option<String> {
        self.children
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(child)
            .map(|live| live.parent.clone())
    }

    /// The scope a live child runs at; `None` for anything that is not one, which must refuse.
    pub fn scope_of(&self, child: &str) -> Option<ProfileScope> {
        self.children
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(child)
            .map(|live| live.scope.clone())
    }

    pub fn live_children(&self) -> usize {
        self.children
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }
}

/// Unbinds its child on drop, so a finished helper's id stops resolving.
pub struct ChildSessionLease {
    registry: Weak<ChildSessionRegistry>,
    child: String,
}

impl Drop for ChildSessionLease {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.upgrade() {
            registry
                .children
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&self.child);
        }
    }
}

/// The process's one registry: the orchestrator binds children and tool servers read them.
pub fn process_child_sessions() -> Arc<ChildSessionRegistry> {
    static REGISTRY: OnceLock<Arc<ChildSessionRegistry>> = OnceLock::new();
    REGISTRY
        .get_or_init(|| Arc::new(ChildSessionRegistry::new()))
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::profile::EXEMPLAR_OWNER_ID;

    #[test]
    fn a_child_resolves_to_its_parent_only_while_its_lease_lives() {
        let registry = Arc::new(ChildSessionRegistry::new());
        let lease = registry.bind(
            "child-1",
            "sched-long-task-a",
            ProfileScope::Owner(EXEMPLAR_OWNER_ID.into()),
        );
        assert_eq!(
            registry.parent_of("child-1").as_deref(),
            Some("sched-long-task-a")
        );
        assert_eq!(
            registry.scope_of("child-1"),
            Some(ProfileScope::Owner(EXEMPLAR_OWNER_ID.into()))
        );
        assert_eq!(
            registry.parent_of("child-2"),
            None,
            "an unknown id resolves to nothing"
        );
        drop(lease);
        assert_eq!(
            registry.parent_of("child-1"),
            None,
            "a finished child must stop resolving, or a stale id reaches its parent's work"
        );
        assert_eq!(
            registry.scope_of("child-1"),
            None,
            "a finished child must stop resolving, or a stale id keeps its member's data"
        );
        assert_eq!(registry.live_children(), 0);
    }
}
