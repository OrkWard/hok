use once_cell::sync::Lazy;
use std::path::{Component, Path, PathBuf};

use crate::script::{self, HookContext};
use crate::{error::Fallible, internal, package::Package, Error, Event, Session};

static SCOOP_SHORTCUT_DIR: Lazy<PathBuf> = Lazy::new(shortcut_dir);

/// Return the path to the shortcut directory.
///
/// `~\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\Scoop Apps`
fn shortcut_dir() -> PathBuf {
    let mut dir = dirs::config_dir().unwrap();
    dir.push("Microsoft/Windows/Start Menu/Programs/Scoop Apps");
    internal::path::normalize_path(dir)
}

pub fn add(session: &Session, package: &Package, context: &HookContext) -> Fallible<()> {
    let Some(shortcuts) = package.manifest().shortcuts() else {
        return Ok(());
    };

    if let Some(tx) = session.emitter() {
        let _ = tx.send(Event::PackageShortcutAddStart);
    }

    for definition in shortcuts {
        if !(2..=4).contains(&definition.len()) {
            return Err(Error::Custom(format!(
                "invalid shortcut definition for '{}'",
                package.name()
            )));
        }

        let target = context.dir.join(safe_relative(definition[0])?);
        if !target.is_file() {
            return Err(Error::Custom(format!(
                "shortcut target '{}' does not exist",
                target.display()
            )));
        }

        let shortcut_relative = shortcut_relative(definition[1])?;
        let shortcut_path = SCOOP_SHORTCUT_DIR.join(&shortcut_relative);
        internal::fs::ensure_dir(shortcut_path.parent().unwrap())?;

        let arguments = definition
            .get(2)
            .map(|value| context.expand(value, package))
            .unwrap_or_default();
        let icon = definition
            .get(3)
            .map(|value| safe_relative(value).map(|path| context.dir.join(path)))
            .transpose()?;
        if let Some(icon) = icon.as_ref() {
            if !icon.is_file() {
                return Err(Error::Custom(format!(
                    "shortcut icon '{}' does not exist",
                    icon.display()
                )));
            }
        }

        let environment = vec![
            ("HOK_SHORTCUT_PATH".into(), shortcut_path.as_os_str().into()),
            ("HOK_SHORTCUT_TARGET".into(), target.as_os_str().into()),
            ("HOK_SHORTCUT_ARGUMENTS".into(), arguments.into()),
            (
                "HOK_SHORTCUT_ICON".into(),
                icon.as_ref()
                    .map(|path| path.as_os_str().to_owned())
                    .unwrap_or_default(),
            ),
        ];
        script::run_powershell("create-shortcut", CREATE_SHORTCUT_SCRIPT, &environment)?;

        if let Some(tx) = session.emitter() {
            let _ = tx.send(Event::PackageShortcutAddProgress(
                shortcut_relative.to_string_lossy().to_string(),
            ));
        }
    }

    if let Some(tx) = session.emitter() {
        let _ = tx.send(Event::PackageShortcutAddDone);
    }
    Ok(())
}

/// Remove shortcut(s) for a given package.
pub fn remove(session: &Session, package: &Package) -> Fallible<()> {
    assert!(package.is_installed());

    if let Some(shortcuts) = package.manifest().shortcuts() {
        if let Some(tx) = session.emitter() {
            let _ = tx.send(Event::PackageShortcutRemoveStart);
        }

        for definition in shortcuts {
            if definition.len() < 2 {
                continue;
            }
            let relative = shortcut_relative(definition[1])?;
            let path = SCOOP_SHORTCUT_DIR.join(&relative);

            if let Some(tx) = session.emitter() {
                let _ = tx.send(Event::PackageShortcutRemoveProgress(
                    relative.to_string_lossy().to_string(),
                ));
            }

            let _ = std::fs::remove_file(path);
        }

        if let Some(tx) = session.emitter() {
            let _ = tx.send(Event::PackageShortcutRemoveDone);
        }
    }
    Ok(())
}

fn shortcut_relative(name: &str) -> Fallible<PathBuf> {
    let path = safe_relative(name)?;
    let file_name = path
        .file_name()
        .ok_or_else(|| Error::Custom("shortcut name cannot be empty".into()))?;
    let mut output = path.parent().unwrap_or_else(|| Path::new("")).to_owned();
    output.push(format!("{}.lnk", file_name.to_string_lossy()));
    Ok(output)
}

fn safe_relative(path: &str) -> Fallible<PathBuf> {
    let path = Path::new(path);
    if path.as_os_str().is_empty()
        || path.components().any(|component| {
            matches!(
                component,
                Component::Prefix(_) | Component::RootDir | Component::ParentDir
            )
        })
    {
        return Err(Error::Custom(format!(
            "unsafe shortcut path '{}'",
            path.display()
        )));
    }
    Ok(path.to_owned())
}

const CREATE_SHORTCUT_SCRIPT: &str = r#"$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$wsh = New-Object -ComObject WScript.Shell
$shortcut = $wsh.CreateShortcut($env:HOK_SHORTCUT_PATH)
$shortcut.TargetPath = $env:HOK_SHORTCUT_TARGET
$shortcut.WorkingDirectory = Split-Path -Parent $env:HOK_SHORTCUT_TARGET
if ($env:HOK_SHORTCUT_ARGUMENTS) { $shortcut.Arguments = $env:HOK_SHORTCUT_ARGUMENTS }
if ($env:HOK_SHORTCUT_ICON) { $shortcut.IconLocation = $env:HOK_SHORTCUT_ICON }
$shortcut.Save()
"#;

#[cfg(test)]
mod tests {
    use super::shortcut_relative;
    use std::path::PathBuf;

    #[test]
    fn shortcut_extension_is_appended() {
        assert_eq!(
            shortcut_relative("Tools\\App.Name").unwrap(),
            PathBuf::from("Tools\\App.Name.lnk")
        );
    }

    #[test]
    fn shortcut_path_cannot_escape_start_menu() {
        assert!(shortcut_relative("..\\Outside").is_err());
        assert!(shortcut_relative("C:\\Outside").is_err());
    }
}
