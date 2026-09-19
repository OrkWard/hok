use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::Fallible;
use crate::package::Package;
use crate::{Error, Session};

static SCRIPT_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug)]
pub(crate) enum Hook {
    PreInstall,
    PostInstall,
    PreUninstall,
    PostUninstall,
}

impl Hook {
    fn name(self) -> &'static str {
        match self {
            Self::PreInstall => "pre_install",
            Self::PostInstall => "post_install",
            Self::PreUninstall => "pre_uninstall",
            Self::PostUninstall => "post_uninstall",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct HookContext {
    pub dir: PathBuf,
    pub original_dir: PathBuf,
    pub persist_dir: PathBuf,
    pub command: String,
    pub architecture: String,
    root_dir: PathBuf,
    cache_dir: PathBuf,
    config_path: PathBuf,
    manifest_json: String,
}

impl HookContext {
    pub fn new(session: &Session, package: &Package, command: &str) -> Fallible<Self> {
        let config = session.config();
        let root_dir = config.root_path().to_owned();
        let original_dir = root_dir
            .join("apps")
            .join(package.name())
            .join(package.version());
        let dir = if config.no_junction() {
            original_dir.clone()
        } else {
            root_dir.join("apps").join(package.name()).join("current")
        };

        let context = Self {
            dir,
            original_dir,
            persist_dir: root_dir.join("persist").join(package.name()),
            command: command.to_owned(),
            architecture: current_architecture().to_owned(),
            cache_dir: config.cache_path().to_owned(),
            config_path: config.path.to_owned(),
            root_dir,
            manifest_json: std::fs::read_to_string(package.manifest().path())?,
        };
        Ok(context)
    }

    pub fn expand(&self, value: &str, package: &Package) -> String {
        value
            .replace("$original_dir", &self.original_dir.to_string_lossy())
            .replace("$persist_dir", &self.persist_dir.to_string_lossy())
            .replace("$architecture", &self.architecture)
            .replace("$version", package.version())
            .replace("$app", package.name())
            .replace("$cmd", &self.command)
            .replace("$global", "$false")
            .replace("$dir", &self.dir.to_string_lossy())
    }

    fn environment(&self, package: &Package) -> Fallible<Vec<(String, OsString)>> {
        let global_dir = std::env::var_os("SCOOP_GLOBAL")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("ProgramData").map(|path| PathBuf::from(path).join("scoop"))
            })
            .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData\scoop"));
        let old_scoop_dir = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join("scoop");
        let filenames = package
            .manifest()
            .url()
            .into_iter()
            .map(artifact_name)
            .collect::<Fallible<Vec<_>>>()?;

        Ok(vec![
            ("HOK_APP".into(), package.name().into()),
            ("HOK_VERSION".into(), package.version().into()),
            ("HOK_ARCHITECTURE".into(), self.architecture.clone().into()),
            ("HOK_COMMAND".into(), self.command.clone().into()),
            ("HOK_DIR".into(), self.dir.as_os_str().into()),
            (
                "HOK_ORIGINAL_DIR".into(),
                self.original_dir.as_os_str().into(),
            ),
            (
                "HOK_PERSIST_DIR".into(),
                self.persist_dir.as_os_str().into(),
            ),
            ("HOK_SCOOP_DIR".into(), self.root_dir.as_os_str().into()),
            ("HOK_GLOBAL_DIR".into(), global_dir.as_os_str().into()),
            ("HOK_CACHE_DIR".into(), self.cache_dir.as_os_str().into()),
            (
                "HOK_BUCKETS_DIR".into(),
                self.root_dir.join("buckets").into_os_string(),
            ),
            (
                "HOK_MODULES_DIR".into(),
                self.root_dir.join("modules").into_os_string(),
            ),
            (
                "HOK_CONFIG_PATH".into(),
                self.config_path.as_os_str().into(),
            ),
            ("HOK_OLD_SCOOP_DIR".into(), old_scoop_dir.into_os_string()),
            (
                "HOK_FILENAMES".into(),
                serde_json::to_string(&filenames)?.into(),
            ),
        ])
    }
}

pub(crate) fn run_hook(
    session: &Session,
    package: &Package,
    hook: Hook,
    context: &HookContext,
) -> Fallible<()> {
    let lines = match hook {
        Hook::PreInstall => package.manifest().pre_install(),
        Hook::PostInstall => package.manifest().post_install(),
        Hook::PreUninstall => package.manifest().pre_uninstall(),
        Hook::PostUninstall => package.manifest().post_uninstall(),
    };
    let Some(lines) = lines else {
        return Ok(());
    };

    let mut source = String::from(POWERSHELL_PRELUDE);
    source.push_str("\r\n$manifest = '");
    source.push_str(&context.manifest_json.replace('\'', "''"));
    source.push_str("' | ConvertFrom-Json\r\n");
    source.push_str(&lines.join("\r\n"));
    source.push_str("\r\n");

    run_powershell(
        &format!("{}-{}", package.name(), hook.name()),
        &source,
        &context.environment(package)?,
    )
    .map_err(|error| {
        Error::Custom(format!(
            "{} hook failed for '{}': {error}",
            hook.name(),
            package.name()
        ))
    })
}

pub(crate) fn run_powershell(
    label: &str,
    source: &str,
    environment: &[(String, OsString)],
) -> Fallible<()> {
    let id = SCRIPT_ID.fetch_add(1, Ordering::Relaxed);
    let script_path = std::env::temp_dir().join(format!(
        "hok-{}-{}-{}.ps1",
        crate::internal::fs::filenamify(label),
        std::process::id(),
        id
    ));

    let mut bytes = Vec::with_capacity(source.len() + 3);
    bytes.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
    bytes.extend_from_slice(source.as_bytes());
    std::fs::write(&script_path, bytes)?;

    let result = Command::new(powershell_executable())
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&script_path)
        .envs(environment.iter().map(|(key, value)| (key, value)))
        .status();
    let _ = std::fs::remove_file(&script_path);

    let status = result.map_err(|error| {
        Error::Custom(format!("could not start PowerShell for {label}: {error}"))
    })?;
    if !status.success() {
        return Err(Error::Custom(format!(
            "PowerShell for {label} exited with {status}"
        )));
    }

    Ok(())
}

fn artifact_name(url: &str) -> Fallible<String> {
    if let Some((_, fragment)) = url.split_once('#') {
        let name = fragment.trim_start_matches('/');
        if let Some(name) = name.rsplit('/').next().filter(|name| !name.is_empty()) {
            return Ok(name.to_owned());
        }
    }

    let clean = url.split(['?', '#']).next().unwrap_or(url);
    clean
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| Error::Custom(format!("could not determine filename from '{url}'")))
}

fn current_architecture() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "64bit"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "32bit"
    }
}

#[cfg(windows)]
fn powershell_executable() -> &'static str {
    "powershell.exe"
}

#[cfg(not(windows))]
fn powershell_executable() -> &'static str {
    "pwsh"
}

const POWERSHELL_PRELUDE: &str = r#"$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$OutputEncoding = [System.Text.UTF8Encoding]::new($false)

$app = $env:HOK_APP
$version = $env:HOK_VERSION
$architecture = $env:HOK_ARCHITECTURE
$cmd = $env:HOK_COMMAND
$global = $false
$dir = $env:HOK_DIR
$original_dir = $env:HOK_ORIGINAL_DIR
$persist_dir = $env:HOK_PERSIST_DIR
$scoopdir = $env:HOK_SCOOP_DIR
$globaldir = $env:HOK_GLOBAL_DIR
$cachedir = $env:HOK_CACHE_DIR
$bucketsdir = $env:HOK_BUCKETS_DIR
$modulesdir = $env:HOK_MODULES_DIR
$cfgpath = $env:HOK_CONFIG_PATH
$oldscoopdir = $env:HOK_OLD_SCOOP_DIR
$cfg = if (Test-Path -LiteralPath $cfgpath) { Get-Content -LiteralPath $cfgpath -Raw -Encoding UTF8 | ConvertFrom-Json } else { [pscustomobject]@{} }
$fname = @($env:HOK_FILENAMES | ConvertFrom-Json)
if ($fname.Count -eq 1) { $fname = $fname[0] }

function appdir($name, $isGlobal = $false) {
    $base = if ($isGlobal) { $globaldir } else { $scoopdir }
    Join-Path (Join-Path $base 'apps') $name
}
function versiondir($name, $targetVersion, $isGlobal = $false) {
    Join-Path (appdir $name $isGlobal) $targetVersion
}
function persistdir($name, $isGlobal = $false) {
    $base = if ($isGlobal) { $globaldir } else { $scoopdir }
    Join-Path (Join-Path $base 'persist') $name
}
function ensure($path) {
    if (!(Test-Path -LiteralPath $path)) { New-Item -ItemType Directory -Path $path -Force | Out-Null }
    Convert-Path -LiteralPath $path
}
function fname($path) { Split-Path -Leaf $path }
function strip_ext($path) { [System.IO.Path]::GetFileNameWithoutExtension($path) }
function info { Write-Host ($args -join ' ') }
function warn { Write-Warning ($args -join ' ') }
function error { Write-Error ($args -join ' ') }
function success { Write-Host ($args -join ' ') -ForegroundColor Green }
function abort { throw ($args -join ' ') }

if (Test-Path -LiteralPath $dir) { Set-Location -LiteralPath $dir }
"#;

#[cfg(test)]
mod tests {
    use super::{artifact_name, run_powershell, POWERSHELL_PRELUDE};
    use std::ffi::OsString;

    #[test]
    fn artifact_name_honors_scoop_fragment() {
        assert_eq!(
            artifact_name("https://example.test/setup.exe#/dl.7z").unwrap(),
            "dl.7z"
        );
    }

    #[test]
    fn artifact_name_ignores_query() {
        assert_eq!(
            artifact_name("https://example.test/tool.zip?download=1").unwrap(),
            "tool.zip"
        );
    }

    #[cfg(windows)]
    #[test]
    fn powershell_hook_prelude_runs() {
        let temp = std::env::temp_dir();
        let missing_config = temp.join("hok-missing-config.json");
        let environment = vec![
            ("HOK_APP".into(), OsString::from("test-app")),
            ("HOK_VERSION".into(), OsString::from("1.0.0")),
            ("HOK_ARCHITECTURE".into(), OsString::from("64bit")),
            ("HOK_COMMAND".into(), OsString::from("install")),
            ("HOK_DIR".into(), temp.as_os_str().into()),
            ("HOK_ORIGINAL_DIR".into(), temp.as_os_str().into()),
            ("HOK_PERSIST_DIR".into(), temp.as_os_str().into()),
            ("HOK_SCOOP_DIR".into(), temp.as_os_str().into()),
            ("HOK_GLOBAL_DIR".into(), temp.as_os_str().into()),
            ("HOK_CACHE_DIR".into(), temp.as_os_str().into()),
            ("HOK_BUCKETS_DIR".into(), temp.as_os_str().into()),
            ("HOK_MODULES_DIR".into(), temp.as_os_str().into()),
            ("HOK_CONFIG_PATH".into(), missing_config.into_os_string()),
            ("HOK_OLD_SCOOP_DIR".into(), temp.as_os_str().into()),
            ("HOK_FILENAMES".into(), OsString::from("[\"test.zip\"]")),
        ];
        let source = format!(
            "{POWERSHELL_PRELUDE}\r\n$manifest = '{{}}' | ConvertFrom-Json\r\nif ($app -ne 'test-app' -or $fname -ne 'test.zip' -or $global) {{ exit 7 }}"
        );

        run_powershell("prelude-test", &source, &environment).unwrap();
    }
}
