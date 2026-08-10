use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File},
    io::Write,
    path::PathBuf,
};

use crate::{consts::ZELLIJ_PLUGIN_PERMISSIONS_CACHE, data::PermissionType};

pub type GrantedPermission = HashMap<String, Vec<PermissionType>>;

/// Permissions granted declaratively in config.kdl, consulted before the interactive prompt.
///
/// Unlike [`PermissionCache`] this is never written back to disk: it is configuration, not runtime
/// state, so it cannot be pruned by a later deny the way cache entries can. Keys are plugin
/// locations exactly as zellij renders them - for a `file:` plugin that is the bare filesystem path.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PluginPermissions {
    pub granted: BTreeMap<String, Vec<PermissionType>>,
}

impl PluginPermissions {
    pub fn all_granted(&self, plugin_location: &str, requested: &[PermissionType]) -> bool {
        match self.granted.get(plugin_location) {
            Some(granted) => requested.iter().all(|p| granted.contains(p)),
            None => false,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.granted.is_empty()
    }
}

#[derive(Default, Debug)]
pub struct PermissionCache {
    path: PathBuf,
    granted: GrantedPermission,
}

impl PermissionCache {
    /// Add `permissions` to whatever this plugin has already been granted.
    ///
    /// This used to replace the entry outright, which silently dropped every previously granted
    /// permission the moment a plugin asked for a different set (eg. after a rebuild that added one
    /// permission).
    pub fn cache(&mut self, plugin_name: String, permissions: Vec<PermissionType>) {
        let granted = self.granted.entry(plugin_name).or_default();
        for permission in permissions {
            if !granted.contains(&permission) {
                granted.push(permission);
            }
        }
    }

    /// Remove only the listed permissions, leaving unrelated grants for this plugin intact.
    pub fn revoke(&mut self, plugin_name: String, permissions: &[PermissionType]) {
        if let Some(granted) = self.granted.get_mut(&plugin_name) {
            granted.retain(|permission| !permissions.contains(permission));
        }
    }

    pub fn get_permissions(&self, plugin_name: String) -> Option<&Vec<PermissionType>> {
        self.granted.get(&plugin_name)
    }

    pub fn check_permissions(
        &self,
        plugin_name: String,
        permissions_to_check: &Vec<PermissionType>,
    ) -> bool {
        if let Some(target) = self.granted.get(&plugin_name) {
            let mut all_granted = true;
            for permission in permissions_to_check {
                if !target.contains(permission) {
                    all_granted = false;
                }
            }
            return all_granted;
        }

        false
    }

    pub fn from_path_or_default(cache_path: Option<PathBuf>) -> Self {
        let cache_path = cache_path.unwrap_or(ZELLIJ_PLUGIN_PERMISSIONS_CACHE.to_path_buf());

        let granted = match fs::read_to_string(cache_path.clone()) {
            Ok(raw_string) => PermissionCache::from_string(raw_string).unwrap_or_default(),
            Err(e) => {
                log::error!("Failed to read permission cache file: {}", e);
                GrantedPermission::default()
            },
        };

        PermissionCache {
            path: cache_path,
            granted,
        }
    }

    pub fn write_to_file(&self) -> std::io::Result<()> {
        let mut f = File::create(&self.path)?;
        write!(f, "{}", PermissionCache::to_string(&self.granted))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caching_a_permission_keeps_earlier_grants() {
        let mut cache = PermissionCache::default();
        cache.cache(
            "plugin".to_owned(),
            vec![PermissionType::ReadApplicationState],
        );
        cache.cache("plugin".to_owned(), vec![PermissionType::RunCommands]);
        assert!(cache.check_permissions(
            "plugin".to_owned(),
            &vec![
                PermissionType::ReadApplicationState,
                PermissionType::RunCommands
            ]
        ));
    }

    #[test]
    fn caching_a_permission_does_not_duplicate_it() {
        let mut cache = PermissionCache::default();
        cache.cache("plugin".to_owned(), vec![PermissionType::RunCommands]);
        cache.cache("plugin".to_owned(), vec![PermissionType::RunCommands]);
        assert_eq!(
            cache.get_permissions("plugin".to_owned()),
            Some(&vec![PermissionType::RunCommands])
        );
    }

    #[test]
    fn revoking_a_permission_leaves_the_others_alone() {
        let mut cache = PermissionCache::default();
        cache.cache(
            "plugin".to_owned(),
            vec![
                PermissionType::ReadApplicationState,
                PermissionType::RunCommands,
            ],
        );
        cache.revoke("plugin".to_owned(), &[PermissionType::RunCommands]);
        assert_eq!(
            cache.get_permissions("plugin".to_owned()),
            Some(&vec![PermissionType::ReadApplicationState])
        );
    }

    #[test]
    fn config_grants_must_cover_every_requested_permission() {
        let mut granted = BTreeMap::new();
        granted.insert(
            "/plugins/foo.wasm".to_owned(),
            vec![PermissionType::ReadApplicationState],
        );
        let plugin_permissions = PluginPermissions { granted };
        assert!(plugin_permissions
            .all_granted("/plugins/foo.wasm", &[PermissionType::ReadApplicationState]));
        assert!(!plugin_permissions.all_granted(
            "/plugins/foo.wasm",
            &[
                PermissionType::ReadApplicationState,
                PermissionType::RunCommands
            ]
        ));
        assert!(!plugin_permissions
            .all_granted("/plugins/bar.wasm", &[PermissionType::ReadApplicationState]));
    }
}
