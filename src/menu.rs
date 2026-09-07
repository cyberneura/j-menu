//! Assembling the menu from the configuration and the built-in launchers.

use std::path::Path;

use crate::config::{Config, MenuItem};
use crate::launchers;

/// The entries to show, in order: everything the configuration files declare,
/// then one group per built-in launcher.
///
/// Configured entries come first because they are the ones somebody wrote on
/// purpose; the detected ones are a convenience and there can be many of them.
pub fn build(config: &Config, start_dir: &Path) -> Vec<MenuItem> {
    let mut items = config.menu.clone();

    if config.auto_launchers.any() {
        for group in launchers::discover(start_dir, &config.auto_launchers) {
            // A single group is flattened into the menu; several groups are
            // each collapsed into a submenu so the top level stays readable.
            items.push(MenuItem {
                title: Some(format!("{} ({} entries)", group.source, group.items.len())),
                submenu: group.items,
                ..Default::default()
            });
        }
    }

    // With nothing configured, a lone launcher group would just be a submenu
    // wrapping the whole menu, so unwrap it: `j` in a Node project is then
    // directly a list of npm scripts.
    if config.menu.is_empty() && items.len() == 1 {
        let only = items.remove(0);
        return only.submenu;
    }

    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::AutoLaunchers;
    use std::fs;
    use std::path::PathBuf;

    fn tempdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("j-menu-menu-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn config_with(menu: Vec<MenuItem>, auto_launchers: AutoLaunchers) -> Config {
        Config {
            menu,
            auto_launchers,
            sources: Vec::new(),
        }
    }

    #[test]
    fn keeps_configured_entries_first() {
        let dir = tempdir("order");
        fs::write(dir.join("package.json"), r#"{"scripts": {"dev": "vite"}}"#).unwrap();

        let config = config_with(
            vec![MenuItem::command("configured", "true")],
            AutoLaunchers::default(),
        );
        let items = build(&config, &dir);
        assert_eq!(items[0].label(), "configured");
        assert!(items[1].label().starts_with("package.json"));
    }

    #[test]
    fn flattens_a_lone_launcher_group_when_nothing_is_configured() {
        let dir = tempdir("flatten");
        fs::write(dir.join("package.json"), r#"{"scripts": {"dev": "vite"}}"#).unwrap();

        let items = build(&config_with(Vec::new(), AutoLaunchers::default()), &dir);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label(), "npm run dev");
    }

    #[test]
    fn keeps_several_launcher_groups_as_submenus() {
        let dir = tempdir("groups");
        fs::write(dir.join("package.json"), r#"{"scripts": {"dev": "vite"}}"#).unwrap();
        fs::write(dir.join("Makefile"), "build:\n\ttrue\n").unwrap();

        let items = build(&config_with(Vec::new(), AutoLaunchers::default()), &dir);
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|i| !i.submenu.is_empty()));
    }

    #[test]
    fn skips_the_launchers_when_they_are_turned_off() {
        let dir = tempdir("off");
        fs::write(dir.join("package.json"), r#"{"scripts": {"dev": "vite"}}"#).unwrap();

        let items = build(&config_with(Vec::new(), AutoLaunchers::All(false)), &dir);
        assert!(items.is_empty());
    }
}
