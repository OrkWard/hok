use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use crate::script::HookContext;
use crate::{config, error::Fallible, internal, package::Package, Error, Event, Session};

/// Set environment variables and append paths defined by a package.
pub fn add(session: &Session, package: &Package, context: &HookContext) -> Fallible<()> {
    let mut changed = false;
    if let Some(env_set) = package.manifest().env_set() {
        changed = true;
        for (key, raw_value) in env_set {
            let value = OsString::from(context.expand(raw_value, package));
            internal::env::set(key, Some(&value))?;
            std::env::set_var(key, &value);
        }
    }

    if let Some(env_add_path) = package.manifest().env_add_path() {
        changed = true;
        let env_path_name = env_path_name(session);
        let additions = env_add_path
            .into_iter()
            .map(safe_relative)
            .collect::<Fallible<Vec<_>>>()?
            .into_iter()
            .map(|path| internal::path::normalize_path(context.dir.join(path)))
            .collect::<Vec<_>>();

        let mut persisted_paths = internal::env::get_path_like_env(&env_path_name)?;
        append_unique_paths(&mut persisted_paths, &additions);
        let updated = std::env::join_paths(persisted_paths)
            .map_err(|error| Error::Custom(error.to_string()))?;
        internal::env::set(&env_path_name, Some(&updated))?;

        // The registry value only contains the user-level path. Preserve inherited
        // system and process entries when updating this running hok process.
        let mut process_paths = current_process_paths(&env_path_name);
        append_unique_paths(&mut process_paths, &additions);
        let updated_process = std::env::join_paths(process_paths)
            .map_err(|error| Error::Custom(error.to_string()))?;
        std::env::set_var(&env_path_name, updated_process);
    }

    if changed {
        internal::env::broadcast();
    }
    Ok(())
}

/// Unset all environment variables defined by a given package.
pub fn remove(session: &Session, package: &Package) -> Fallible<()> {
    assert!(package.is_installed());

    let mut changed = false;
    if let Some(env_set) = package.manifest().env_set() {
        changed = true;
        if let Some(tx) = session.emitter() {
            let _ = tx.send(Event::PackageEnvVarRemoveStart);
        }

        for key in env_set.keys() {
            internal::env::set(key, None)?;
            std::env::remove_var(key);
        }

        if let Some(tx) = session.emitter() {
            let _ = tx.send(Event::PackageEnvVarRemoveDone);
        }
    }

    if let Some(env_add_path) = package.manifest().env_add_path() {
        changed = true;
        let env_path_name = env_path_name(session);
        let mut paths = internal::env::get_path_like_env(&env_path_name)?;
        let config = session.config();
        let version = if config.no_junction() {
            package.installed_version().unwrap()
        } else {
            "current"
        };
        let app_path = config
            .root_path()
            .join("apps")
            .join(package.name())
            .join(version);
        drop(config);

        if let Some(tx) = session.emitter() {
            let _ = tx.send(Event::PackageEnvPathRemoveStart);
        }

        let removals = env_add_path
            .into_iter()
            .map(safe_relative)
            .collect::<Fallible<Vec<_>>>()?
            .into_iter()
            .map(|path| internal::path::normalize_path(app_path.join(path)))
            .collect::<Vec<_>>();
        paths.retain(|path| !removals.iter().any(|removal| paths_equal(path, removal)));

        let updated =
            std::env::join_paths(paths).map_err(|error| Error::Custom(error.to_string()))?;
        internal::env::set(&env_path_name, Some(&updated))?;

        let mut process_paths = current_process_paths(&env_path_name);
        process_paths.retain(|path| !removals.iter().any(|removal| paths_equal(path, removal)));
        let updated_process = std::env::join_paths(process_paths)
            .map_err(|error| Error::Custom(error.to_string()))?;
        std::env::set_var(&env_path_name, updated_process);

        if let Some(tx) = session.emitter() {
            let _ = tx.send(Event::PackageEnvPathRemoveDone);
        }
    }

    if changed {
        internal::env::broadcast();
    }
    Ok(())
}

fn env_path_name(session: &Session) -> String {
    match session.config().use_isolated_path() {
        Some(config::IsolatedPath::Named(name)) => name.to_owned(),
        Some(config::IsolatedPath::Boolean(true)) => "SCOOP_PATH".to_owned(),
        _ => "PATH".to_owned(),
    }
}

fn current_process_paths(name: &str) -> Vec<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(|value| std::env::split_paths(&value).collect())
        .unwrap_or_default()
}

fn append_unique_paths(paths: &mut Vec<PathBuf>, additions: &[PathBuf]) {
    for addition in additions {
        if !paths.iter().any(|existing| paths_equal(existing, addition)) {
            paths.push(addition.clone());
        }
    }
}

fn safe_relative(path: &str) -> Fallible<PathBuf> {
    let path = Path::new(path);
    if path.components().any(|component| {
        matches!(
            component,
            Component::Prefix(_) | Component::RootDir | Component::ParentDir
        )
    }) {
        return Err(Error::Custom(format!(
            "unsafe environment path '{}'",
            path.display()
        )));
    }
    Ok(path.to_owned())
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::{append_unique_paths, safe_relative};
    use std::path::PathBuf;

    #[test]
    fn environment_paths_must_be_relative() {
        assert!(safe_relative("bin").is_ok());
        assert!(safe_relative("tools\\bin").is_ok());
        assert!(safe_relative("..\\outside").is_err());
        assert!(safe_relative("C:\\outside").is_err());
    }

    #[test]
    fn environment_paths_are_appended_once_case_insensitively() {
        let mut paths = vec![PathBuf::from(r"C:\Tools")];
        append_unique_paths(
            &mut paths,
            &[PathBuf::from(r"c:\tools"), PathBuf::from(r"C:\Other")],
        );
        assert_eq!(
            paths,
            [PathBuf::from(r"C:\Tools"), PathBuf::from(r"C:\Other")]
        );
    }
}
