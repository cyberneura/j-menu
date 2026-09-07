//! Entries for a Gradle project.
//!
//! Gradle can list a project's real tasks, but only by running
//! `./gradlew tasks`, which starts a JVM and evaluates the build script. That
//! is far too slow and too side-effecting to do while opening a menu, so the
//! tasks are inferred from the plugins the build script declares instead.
//!
//! Assuming the lifecycle tasks exist is not safe: a build that applies no
//! plugin has none of them. Gradle 8 fails outright on `clean`, `assemble`,
//! `check` and `test` there and — worse — resolves `build` to the unrelated
//! built-in `buildEnvironment` task and exits successfully. So a lifecycle
//! task is offered only when a plugin defining it can be identified, and a
//! build with nothing recognisable is left with `tasks`, which is always
//! present and is how Gradle itself tells you what a project can do.

use std::path::Path;

use super::{LauncherGroup, in_dir};
use crate::config::MenuItem;

/// What a build has to apply for a task to exist.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Needs {
    /// Nothing: one of Gradle's built-in help tasks.
    Nothing,
    /// The `base` plugin, which every language plugin applies.
    Base,
    /// A JVM language plugin.
    Jvm,
}

/// The offered tasks in menu order, each with what defines it.
const TASKS: &[(&str, Needs)] = &[
    ("build", Needs::Base),
    ("test", Needs::Jvm),
    ("clean", Needs::Base),
    ("assemble", Needs::Base),
    ("check", Needs::Base),
    ("tasks", Needs::Nothing),
];

/// Plugin ids bringing the JVM tasks, and with them everything `base`
/// defines. Matched exactly: a plugin has to be recognised to count, and
/// `org.jetbrains.kotlin.plugin.serialization` is not the Kotlin JVM plugin
/// however much of the name it shares.
const JVM_PLUGINS: &[&str] = &[
    "java",
    "java-library",
    "groovy",
    "scala",
    "war",
    "application",
    "org.jetbrains.kotlin.jvm",
    "org.jetbrains.kotlin.android",
    "com.android.application",
    "com.android.library",
    "com.android.test",
    "com.android.dynamic-feature",
];

/// Plugin ids bringing `base` — the lifecycle tasks — but no `test`. Kotlin
/// Multiplatform is here because it names its test tasks per target
/// (`jvmTest`, `allTests`) and defines no plain `test`.
const BASE_PLUGINS: &[&str] = &[
    "base",
    "distribution",
    // The Ear plugin applies Base, not Java, so there is no `test`.
    "ear",
    "cpp-application",
    "cpp-library",
    "swift-application",
    "swift-library",
    "org.jetbrains.kotlin.multiplatform",
    "org.jetbrains.kotlin.js",
];

/// Produce the Gradle entries, preferring the wrapper when it is present.
///
/// `wrapper` and `script` are looked up independently by the caller: in a
/// multi-project build the wrapper sits at the root while the subproject has
/// only its own build script, so the nearest of each can be in different
/// directories.
///
/// `start_dir` is where the menu was opened. Gradle takes the project
/// directory from the working directory, and a directory that is not part of
/// the build is rejected, so the command is run from the directory holding the
/// build script.
pub fn scan(
    wrapper: Option<&Path>,
    script: Option<&Path>,
    start_dir: &Path,
) -> Option<LauncherGroup> {
    // The wrapper pins the Gradle version for the project, so use it when
    // available and fall back to a Gradle on PATH otherwise.
    let (runner, source) = match wrapper {
        Some(wrapper) => (super::quote(&wrapper.to_string_lossy()), "gradlew"),
        // No wrapper: there has to be a build script, or the caller would not
        // have asked.
        None => {
            script?;
            ("gradle".to_string(), "build.gradle")
        }
    };

    // The build script marks the project directory; without one, the wrapper's
    // own directory is the root of the build.
    let project_dir = script
        .and_then(Path::parent)
        .or_else(|| wrapper.and_then(Path::parent))?;

    // A subproject usually gets its plugins from the root, so the root build
    // script counts too — but only what it applies to every project. A plugin
    // the root applies to itself says nothing about the subproject.
    //
    // The settings file is what marks the root of a build, and it is there
    // whether or not the build ships a wrapper. Its own directory is the
    // fallback for the malformed case of a wrapper with no settings file.
    let root_dir = super::find_up(start_dir, &["settings.gradle", "settings.gradle.kts"])
        .as_deref()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .or_else(|| wrapper.and_then(Path::parent).map(Path::to_path_buf));
    let inherited = root_dir
        .filter(|dir| Some(dir.as_path()) != script.and_then(Path::parent))
        .and_then(|dir| root_script(&dir))
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| capabilities_of(&text, Scope::Shared))
        .unwrap_or_default();
    let available = inherited.union(
        script
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| capabilities_of(&text, Scope::Anywhere))
            .unwrap_or_default(),
    );

    let items = TASKS
        .iter()
        .filter(|(_, needs)| available.covers(*needs))
        .map(|(task, _)| {
            let label = if source == "gradlew" {
                format!("./gradlew {task}")
            } else {
                format!("gradle {task}")
            };
            let command = in_dir(project_dir, &format!("{runner} {task}"), start_dir);
            MenuItem::command(label, command)
        })
        .collect();

    Some(LauncherGroup {
        source: source.to_string(),
        items,
    })
}

/// The build script of the root project, in either DSL.
fn root_script(dir: &Path) -> Option<std::path::PathBuf> {
    ["build.gradle", "build.gradle.kts"]
        .iter()
        .map(|name| dir.join(name))
        .find(|path| path.is_file())
}

/// What the plugins of a build script make available.
#[derive(Clone, Copy, Default)]
struct Capabilities {
    base: bool,
    jvm: bool,
}

impl Capabilities {
    fn union(self, other: Self) -> Self {
        Self {
            base: self.base || other.base,
            jvm: self.jvm || other.jvm,
        }
    }

    fn covers(self, needs: Needs) -> bool {
        match needs {
            Needs::Nothing => true,
            // A JVM plugin applies `base`, so it covers the lifecycle tasks too.
            Needs::Base => self.base || self.jvm,
            Needs::Jvm => self.jvm,
        }
    }
}

/// Where in a build script a declaration has to be for it to count.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// Anywhere: the script belongs to the project the menu was opened in.
    Anywhere,
    /// Only inside an `allprojects` or `subprojects` block. A `plugins` block
    /// in the root script configures the root project alone, so it says
    /// nothing about the subproject the menu was opened in.
    Shared,
}

/// Classify the plugins a build script applies within `scope`.
fn capabilities_of(text: &str, scope: Scope) -> Capabilities {
    let mut found = Capabilities::default();
    for plugin in applied_plugins(text, scope) {
        found.jvm |= JVM_PLUGINS.contains(&plugin.as_str());
        found.base |= BASE_PLUGINS.contains(&plugin.as_str());
    }
    found
}

/// The plugin ids a build script applies.
///
/// This recognises declarations in a token stream rather than parsing Groovy
/// or Kotlin: `id 'java'`, `id("java")`, `apply plugin: 'java'`,
/// `apply(plugin = "java")` and `kotlin("jvm")` all yield the id, wherever
/// they appear. A declaration carrying `apply false` names a plugin for the
/// subprojects to apply rather than applying it here, so it does not count.
///
/// A plugin referenced through a version catalog (`alias(libs.plugins.foo)`)
/// is deliberately not resolved: the alias is a key in `libs.versions.toml`,
/// so the id behind it cannot be established from the build script alone, and
/// guessing it from the alias would be how a task that does not exist ends up
/// in the menu.
fn applied_plugins(text: &str, scope: Scope) -> Vec<String> {
    let tokens = tokenize(text);
    let mut plugins = Vec::new();

    let mut depth = 0usize;
    // The depths at which the innermost enclosing `allprojects` /
    // `subprojects` and `plugins` blocks were opened, while inside one.
    let mut shared_from: Option<usize> = None;
    let mut plugins_from: Option<usize> = None;
    // Set by the word naming a block, and only honoured when the very next
    // token opens one — `println("subprojects")` must not count.
    let mut opens_shared = false;
    let mut opens_plugins = false;

    for (index, token) in tokens.iter().enumerate() {
        match token {
            Token::Symbol('{') => {
                depth += 1;
                if opens_shared && shared_from.is_none() {
                    shared_from = Some(depth);
                }
                if opens_plugins && plugins_from.is_none() {
                    plugins_from = Some(depth);
                }
            }
            Token::Symbol('}') => {
                if shared_from == Some(depth) {
                    shared_from = None;
                }
                if plugins_from == Some(depth) {
                    plugins_from = None;
                }
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
        opens_shared = matches!(token, Token::Word("allprojects" | "subprojects"));
        opens_plugins = matches!(token, Token::Word("plugins"));

        if scope == Scope::Shared && shared_from.is_none() {
            continue;
        }
        let rest = &tokens[index + 1..];
        let (id, rest) = match token {
            // `id 'java'`, `id("java")`. Only inside a `plugins` block: `id`
            // is an ordinary name elsewhere, and a build defining its own
            // `id` closure must not be read as applying a plugin.
            Token::Word("id") if plugins_from.is_some() => match argument(rest, &['(']) {
                Some((id, rest)) => (id.to_string(), rest),
                None => continue,
            },
            // `apply plugin: 'java'`, `apply(plugin = "java")`. Both
            // spellings put `plugin` right after `apply`, which is what
            // separates them from an ordinary `def plugin = 'java'`.
            Token::Word("plugin") if follows_apply(&tokens[..index]) => {
                match argument(rest, &['(', ':', '=']) {
                    Some((id, rest)) => (id.to_string(), rest),
                    None => continue,
                }
            }
            // `kotlin("jvm")` is shorthand for the qualified plugin id, and
            // is likewise only a declaration inside a `plugins` block.
            Token::Word("kotlin") if plugins_from.is_some() => match argument(rest, &['(']) {
                Some((id, rest)) => (format!("org.jetbrains.kotlin.{id}"), rest),
                None => continue,
            },
            _ => continue,
        };
        if !is_deferred(rest) {
            plugins.push(id);
        }
    }
    plugins
}

/// Whether these tokens end in the `apply` that `apply plugin:` starts with,
/// in either DSL (`apply plugin: 'java'`, `apply(plugin = "java")`).
fn follows_apply(before: &[Token]) -> bool {
    match before.last() {
        Some(Token::Word("apply")) => true,
        // The Kotlin spelling puts the opening parenthesis in between.
        Some(Token::Symbol('(')) => matches!(
            before.len().checked_sub(2).and_then(|i| before.get(i)),
            Some(Token::Word("apply"))
        ),
        _ => false,
    }
}

/// The string a declaration takes as its argument, along with what follows it.
///
/// Only the punctuation in `skippable` may come first, so a bare `id` picks up
/// nothing and the next declaration is free to match instead.
fn argument<'a, 'b>(
    tokens: &'b [Token<'a>],
    skippable: &[char],
) -> Option<(&'a str, &'b [Token<'a>])> {
    let start = tokens
        .iter()
        .position(|token| !matches!(token, Token::Symbol(c) if skippable.contains(c)))?;
    match tokens[start] {
        Token::Text(id) => Some((id, &tokens[start + 1..])),
        _ => None,
    }
}

/// Whether the declaration these tokens continue carries `apply false`, which
/// makes it a version declaration rather than an application of the plugin.
///
/// The search stops at the end of the declaration, so that `apply false` on a
/// later one does not disable this plugin as well.
fn is_deferred(tokens: &[Token]) -> bool {
    let declaration = tokens
        .iter()
        .position(|token| {
            matches!(
                token,
                Token::Symbol(';' | '{' | '}') | Token::Word("id" | "plugin" | "kotlin")
            )
        })
        .map_or(tokens, |end| &tokens[..end]);
    declaration
        .windows(2)
        .any(|pair| matches!(pair, [Token::Word("apply"), Token::Word("false")]))
}

/// One meaningful piece of a build script.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Token<'a> {
    /// An identifier or a number.
    Word(&'a str),
    /// The contents of a string literal, without its quotes.
    Text(&'a str),
    /// Any other single character.
    Symbol(char),
}

/// Split a build script into tokens, dropping comments and whitespace.
///
/// Both DSLs are close enough to C here: `//` and `/* */` comments, and
/// single, double and triple quoted strings with backslash escapes. Knowing
/// where the strings are is the point — `id` inside one is prose, not a
/// declaration, and a `//` inside one is a URL, not a comment.
fn tokenize(text: &str) -> Vec<Token<'_>> {
    let bytes = text.as_bytes();
    let is_word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let at = |index: usize| bytes.get(index).copied();
    let mut tokens = Vec::new();
    let mut index = 0;

    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_whitespace() {
            index += 1;
        } else if byte == b'/' && at(index + 1) == Some(b'/') {
            index = text[index..]
                .find('\n')
                .map_or(bytes.len(), |end| index + end);
        } else if byte == b'/' && at(index + 1) == Some(b'*') {
            index = text[index + 2..]
                .find("*/")
                .map_or(bytes.len(), |end| index + 2 + end + 2);
        } else if byte == b'\'' || byte == b'"' {
            let quote = byte;
            // A triple quote runs to the next triple quote; anything else
            // ends at the next unescaped quote of the same kind.
            let triple = at(index + 1) == Some(quote) && at(index + 2) == Some(quote);
            let terminator = if triple { 3 } else { 1 };
            let start = index + terminator;
            let mut end = start;
            loop {
                match at(end) {
                    None => break,
                    Some(b'\\') => end += 2,
                    Some(current) if current == quote => {
                        if !triple || (at(end + 1) == Some(quote) && at(end + 2) == Some(quote)) {
                            break;
                        }
                        end += 1;
                    }
                    Some(_) => end += 1,
                }
            }
            let end = end.min(bytes.len());
            tokens.push(Token::Text(&text[start.min(end)..end]));
            index = (end + terminator).min(bytes.len());
        } else if is_word(byte) {
            let start = index;
            while index < bytes.len() && is_word(bytes[index]) {
                index += 1;
            }
            tokens.push(Token::Word(&text[start..index]));
        } else {
            // A multi-byte character is never part of a declaration, so it
            // only has to be stepped over as a whole.
            let character = text[index..].chars().next().unwrap_or('?');
            tokens.push(Token::Symbol(character));
            index += character.len_utf8();
        }
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A build script applying a plugin, so the lifecycle tasks are offered.
    const JAVA: &str = "plugins {\n    id 'java'\n}\n";

    fn tempdir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("j-menu-gradle-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn labels(group: &LauncherGroup) -> Vec<String> {
        group.items.iter().map(|i| i.label()).collect()
    }

    #[test]
    fn prefers_the_wrapper_when_present() {
        let dir = tempdir("wrapper");
        let wrapper = dir.join("gradlew");
        fs::write(&wrapper, "#!/bin/sh\n").unwrap();
        let script = dir.join("build.gradle");
        fs::write(&script, JAVA).unwrap();
        let group = scan(Some(&wrapper), Some(&script), &dir).unwrap();
        assert_eq!(group.source, "gradlew");
        assert_eq!(
            group.items[0].script().unwrap(),
            format!("'{}' build", wrapper.display())
        );
    }

    #[test]
    fn falls_back_to_gradle_on_path_without_a_wrapper() {
        let dir = tempdir("no-wrapper");
        let script = dir.join("build.gradle");
        fs::write(&script, JAVA).unwrap();
        let group = scan(None, Some(&script), &dir).unwrap();
        assert_eq!(group.source, "build.gradle");
        assert_eq!(group.items[0].script().unwrap(), "gradle build");
    }

    #[test]
    fn uses_a_root_wrapper_with_a_subproject_build_script() {
        // The multi-project layout: gradlew at the root, build.gradle in the
        // subproject the menu was opened in.
        let root = tempdir("multi-project");
        let wrapper = root.join("gradlew");
        fs::write(&wrapper, "#!/bin/sh\n").unwrap();
        let sub = root.join("app");
        fs::create_dir_all(&sub).unwrap();
        let script = sub.join("build.gradle");
        fs::write(&script, JAVA).unwrap();

        let group = scan(Some(&wrapper), Some(&script), &sub).unwrap();
        assert_eq!(group.source, "gradlew");
        assert_eq!(
            group.items[0].script().unwrap(),
            format!("'{}' build", wrapper.display())
        );
    }

    #[test]
    fn runs_from_the_project_directory_when_opened_further_down() {
        // Gradle rejects a working directory that is not part of the build,
        // so a `cd` to the project directory is required.
        let root = tempdir("below-project");
        let wrapper = root.join("gradlew");
        fs::write(&wrapper, "#!/bin/sh\n").unwrap();
        let script = root.join("build.gradle");
        fs::write(&script, JAVA).unwrap();
        let deep = root.join("src/main/java");
        fs::create_dir_all(&deep).unwrap();

        let group = scan(Some(&wrapper), Some(&script), &deep).unwrap();
        assert_eq!(
            group.items[0].script().unwrap(),
            format!("cd '{}' && '{}' build", root.display(), wrapper.display())
        );
    }

    #[test]
    fn produces_nothing_without_a_wrapper_or_a_build_script() {
        assert!(scan(None, None, Path::new("/tmp")).is_none());
    }

    #[test]
    fn offers_only_tasks_when_no_plugin_is_applied() {
        // An empty build script defines none of the lifecycle tasks: `clean`,
        // `assemble`, `check` and `test` fail, and `build` silently resolves
        // to the unrelated `buildEnvironment`.
        let dir = tempdir("no-plugin");
        let script = dir.join("build.gradle");
        fs::write(&script, "// nothing applied\n").unwrap();
        let group = scan(None, Some(&script), &dir).unwrap();
        assert_eq!(labels(&group), vec!["gradle tasks"]);
    }

    #[test]
    fn offers_only_tasks_for_a_wrapper_without_a_build_script() {
        // Nothing to read means nothing can be established.
        let dir = tempdir("wrapper-only");
        let wrapper = dir.join("gradlew");
        fs::write(&wrapper, "#!/bin/sh\n").unwrap();
        let group = scan(Some(&wrapper), None, &dir).unwrap();
        assert_eq!(labels(&group), vec!["./gradlew tasks"]);
    }

    #[test]
    fn offers_the_full_lifecycle_for_a_jvm_plugin() {
        let dir = tempdir("jvm");
        let script = dir.join("build.gradle");
        fs::write(&script, JAVA).unwrap();
        let group = scan(None, Some(&script), &dir).unwrap();
        assert_eq!(
            labels(&group),
            vec![
                "gradle build",
                "gradle test",
                "gradle clean",
                "gradle assemble",
                "gradle check",
                "gradle tasks",
            ]
        );
    }

    #[test]
    fn omits_test_for_a_plugin_that_only_brings_base() {
        // `base` defines the lifecycle tasks; `test` comes with a language
        // plugin.
        let dir = tempdir("base-only");
        let script = dir.join("build.gradle");
        fs::write(&script, "plugins {\n    id 'base'\n}\n").unwrap();
        let group = scan(None, Some(&script), &dir).unwrap();
        assert_eq!(
            labels(&group),
            vec![
                "gradle build",
                "gradle clean",
                "gradle assemble",
                "gradle check",
                "gradle tasks",
            ]
        );
    }

    #[test]
    fn takes_plugins_applied_to_a_subproject_from_the_root() {
        // `subprojects { apply plugin: ... }` at the root is how a
        // multi-project build usually configures its modules, so a subproject
        // script of its own can be empty.
        let root = tempdir("root-plugins");
        let wrapper = root.join("gradlew");
        fs::write(&wrapper, "#!/bin/sh\n").unwrap();
        fs::write(
            root.join("build.gradle"),
            "subprojects {\n    apply plugin: 'java'\n}\n",
        )
        .unwrap();
        let sub = root.join("app");
        fs::create_dir_all(&sub).unwrap();
        let script = sub.join("build.gradle");
        fs::write(&script, "dependencies {\n}\n").unwrap();

        let group = scan(Some(&wrapper), Some(&script), &sub).unwrap();
        assert!(labels(&group).contains(&"./gradlew test".to_string()));
    }

    #[test]
    fn recognises_the_kotlin_dsl_shorthand() {
        let dir = tempdir("kotlin-dsl");
        let script = dir.join("build.gradle.kts");
        fs::write(
            &script,
            "plugins {\n    kotlin(\"jvm\") version \"2.0.0\"\n}\n",
        )
        .unwrap();
        assert!(labels(&scan(None, Some(&script), &dir).unwrap()).contains(&"gradle test".into()));
    }

    #[test]
    fn does_not_guess_at_a_version_catalog_alias() {
        // The alias is a key in `libs.versions.toml`, which is not read, so
        // the plugin behind it cannot be established.
        let dir = tempdir("catalog");
        let script = dir.join("build.gradle.kts");
        fs::write(
            &script,
            "plugins {\n    alias(libs.plugins.android.application)\n}\n",
        )
        .unwrap();
        assert_eq!(
            labels(&scan(None, Some(&script), &dir).unwrap()),
            vec!["gradle tasks"]
        );
    }

    #[test]
    fn ignores_a_declaration_that_is_only_part_of_a_string() {
        let dir = tempdir("in-a-string");
        let script = dir.join("build.gradle.kts");
        fs::write(
            &script,
            "tasks.register(\"sample\") {\n    doLast { println(\"add id(\\\"java\\\") to build.gradle\") }\n}\n",
        )
        .unwrap();
        assert_eq!(
            labels(&scan(None, Some(&script), &dir).unwrap()),
            vec!["gradle tasks"]
        );
    }

    #[test]
    fn keeps_a_plugin_declared_beside_a_deferred_one() {
        // `apply false` belongs to the declaration it follows, not to the
        // whole line.
        let dir = tempdir("mixed-line");
        let script = dir.join("build.gradle.kts");
        fs::write(
            &script,
            "plugins { id(\"java\"); id(\"com.example.other\") version \"1.0\" apply false }\n",
        )
        .unwrap();
        assert!(labels(&scan(None, Some(&script), &dir).unwrap()).contains(&"gradle test".into()));
    }

    #[test]
    fn finds_the_root_through_the_settings_file_without_a_wrapper() {
        // A multi-project build using a Gradle on PATH still has a settings
        // file marking its root, which is where the shared plugins are.
        let root = tempdir("settings-root");
        fs::write(root.join("settings.gradle"), "include 'app'\n").unwrap();
        fs::write(
            root.join("build.gradle"),
            "subprojects {\n    apply plugin: 'java'\n}\n",
        )
        .unwrap();
        let sub = root.join("app");
        fs::create_dir_all(&sub).unwrap();
        let script = sub.join("build.gradle");
        fs::write(&script, "dependencies {\n}\n").unwrap();

        let group = scan(None, Some(&script), &sub).unwrap();
        assert!(labels(&group).contains(&"gradle test".to_string()));
    }

    #[test]
    fn does_not_take_a_string_named_subprojects_for_a_shared_block() {
        // Only a real `subprojects { ... }` block configures the subprojects.
        let root = tempdir("named-subprojects");
        let wrapper = root.join("gradlew");
        fs::write(&wrapper, "#!/bin/sh\n").unwrap();
        fs::write(
            root.join("build.gradle"),
            "tasks.register(\"subprojects\") {\n    apply plugin: 'java'\n}\n",
        )
        .unwrap();
        let sub = root.join("app");
        fs::create_dir_all(&sub).unwrap();
        let script = sub.join("build.gradle");
        fs::write(&script, "dependencies {\n}\n").unwrap();

        let group = scan(Some(&wrapper), Some(&script), &sub).unwrap();
        assert_eq!(labels(&group), vec!["./gradlew tasks"]);
    }

    #[test]
    fn ignores_a_plugin_name_that_is_only_mentioned_in_a_comment() {
        for (name, body) in [
            ("line-comment", "plugins {\n    // id 'java'\n}\n"),
            ("block-comment", "/*\nid 'java'\n*/\nplugins {\n}\n"),
            ("trailing-block", "plugins {\n} /* id(\"java\") */\n"),
        ] {
            let dir = tempdir(name);
            let script = dir.join("build.gradle");
            fs::write(&script, body).unwrap();
            assert_eq!(
                labels(&scan(None, Some(&script), &dir).unwrap()),
                vec!["gradle tasks"],
                "{name}"
            );
        }
    }

    #[test]
    fn recognises_a_declaration_that_shares_its_line() {
        let dir = tempdir("one-liner");
        let script = dir.join("build.gradle.kts");
        fs::write(&script, "plugins { id(\"java\") }\n").unwrap();
        assert!(labels(&scan(None, Some(&script), &dir).unwrap()).contains(&"gradle test".into()));
    }

    #[test]
    fn ignores_a_plugin_declared_with_apply_false() {
        // The root of a multi-project build pins versions this way without
        // applying anything, so its tasks do not exist.
        let dir = tempdir("apply-false");
        let script = dir.join("build.gradle.kts");
        fs::write(
            &script,
            "plugins {\n    id(\"java\") version \"1.0\" apply false\n}\n",
        )
        .unwrap();
        assert_eq!(
            labels(&scan(None, Some(&script), &dir).unwrap()),
            vec!["gradle tasks"]
        );
    }

    #[test]
    fn does_not_hand_a_subproject_the_plugins_the_root_applies_to_itself() {
        // A `plugins` block at the root configures the root project only.
        let root = tempdir("root-only-plugins");
        let wrapper = root.join("gradlew");
        fs::write(&wrapper, "#!/bin/sh\n").unwrap();
        fs::write(root.join("build.gradle"), JAVA).unwrap();
        let sub = root.join("app");
        fs::create_dir_all(&sub).unwrap();
        let script = sub.join("build.gradle");
        fs::write(&script, "dependencies {\n}\n").unwrap();

        let group = scan(Some(&wrapper), Some(&script), &sub).unwrap();
        assert_eq!(labels(&group), vec!["./gradlew tasks"]);
    }

    #[test]
    fn ignores_an_id_call_outside_a_plugins_block() {
        // A build is free to define its own `id` function; calling it is not
        // a plugin declaration.
        let dir = tempdir("own-id");
        let script = dir.join("build.gradle");
        fs::write(&script, "def id = { value -> }\nid('java')\n").unwrap();
        assert_eq!(
            labels(&scan(None, Some(&script), &dir).unwrap()),
            vec!["gradle tasks"]
        );
    }

    #[test]
    fn takes_an_apply_plugin_call_outside_a_plugins_block() {
        // Unlike `id`, `apply plugin:` really does apply from anywhere.
        let dir = tempdir("apply-anywhere");
        let script = dir.join("build.gradle");
        fs::write(&script, "apply plugin: 'java'\n").unwrap();
        assert!(labels(&scan(None, Some(&script), &dir).unwrap()).contains(&"gradle test".into()));
    }

    #[test]
    fn ignores_a_variable_that_happens_to_be_called_plugin() {
        // `def plugin = 'java'` assigns a string; it applies nothing.
        let dir = tempdir("plugin-variable");
        let script = dir.join("build.gradle");
        fs::write(&script, "def plugin = 'java'\n").unwrap();
        assert_eq!(
            labels(&scan(None, Some(&script), &dir).unwrap()),
            vec!["gradle tasks"]
        );
    }

    #[test]
    fn takes_the_kotlin_dsl_spelling_of_apply_plugin() {
        let dir = tempdir("apply-kotlin");
        let script = dir.join("build.gradle.kts");
        fs::write(&script, "apply(plugin = \"java\")\n").unwrap();
        assert!(labels(&scan(None, Some(&script), &dir).unwrap()).contains(&"gradle test".into()));
    }

    #[test]
    fn does_not_take_a_plugin_for_another_one_sharing_a_word() {
        // Kotlin's serialization plugin adds no task of its own, so matching
        // it as "kotlin" would offer tasks that are not there.
        let dir = tempdir("serialization");
        let script = dir.join("build.gradle.kts");
        fs::write(
            &script,
            "plugins {\n    id(\"org.jetbrains.kotlin.plugin.serialization\")\n}\n",
        )
        .unwrap();
        assert_eq!(
            labels(&scan(None, Some(&script), &dir).unwrap()),
            vec!["gradle tasks"]
        );
    }
}
