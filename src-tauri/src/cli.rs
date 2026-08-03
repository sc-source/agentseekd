// CLI runtime detection: version comparison, dependency checking,
// agentseek/uv program resolution, and template parsing.

fn display_name(template_id: &str) -> String {
    template_id
        .split('/')
        .next_back()
        .unwrap_or(template_id)
        .split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn runtime_path() -> std::ffi::OsString {
    let mut paths = Vec::new();
    if let Some(runtime_root) = env::var_os("AGENTSEEK_DESKTOP_RUNTIME_DIR") {
        let versions_dir = PathBuf::from(runtime_root).join("nvm/versions/node");
        let mut managed_bins = fs::read_dir(versions_dir)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.join("bin/node").is_file())
            .collect::<Vec<_>>();
        managed_bins.sort_by_key(|path| {
            path.file_name()
                .map(|name| numeric_version(&name.to_string_lossy()))
                .unwrap_or_default()
        });
        paths.extend(managed_bins.into_iter().rev().map(|path| path.join("bin")));
    }
    if let Some(managed_node_bin) = env::var_os("AGENTSEEK_DESKTOP_NODE_BIN") {
        paths.push(PathBuf::from(managed_node_bin));
    }
    if let Some(home) = env::var_os("HOME") {
        paths.push(PathBuf::from(&home).join(".local/bin"));
        paths.push(PathBuf::from(&home).join(".cargo/bin"));
        paths.push(PathBuf::from(&home).join(".pyenv/shims"));
        paths.push(PathBuf::from(home).join(".pyenv/bin"));
    }
    paths.extend([
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
        PathBuf::from("/Library/Frameworks/Python.framework/Versions/3.9/bin"),
    ]);
    if let Some(existing) = env::var_os("PATH") {
        paths.extend(env::split_paths(&existing));
    }
    env::join_paths(paths).unwrap_or_default()
}

fn managed_runtime_root() -> Option<PathBuf> {
    env::var_os("AGENTSEEK_DESKTOP_RUNTIME_DIR").map(PathBuf::from)
}

fn managed_node_bin(runtime_root: &Path, node_version: &str) -> PathBuf {
    if cfg!(windows) {
        let architecture = if cfg!(target_arch = "aarch64") {
            "win-arm64"
        } else {
            "win-x64"
        };
        runtime_root.join(format!("node-v{node_version}-{architecture}"))
    } else {
        let versions_dir = runtime_root.join("nvm").join("versions").join("node");
        let exact = versions_dir.join(format!("v{node_version}")).join("bin");
        if exact.join("node").is_file() {
            return exact;
        }
        let major = numeric_version(node_version).first().copied();
        let mut candidates = fs::read_dir(&versions_dir)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.join("bin/node").is_file()
                    && path
                        .file_name()
                        .map(|name| {
                            numeric_version(&name.to_string_lossy()).first().copied() == major
                        })
                        .unwrap_or(false)
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|path| {
            path.file_name()
                .map(|name| numeric_version(&name.to_string_lossy()))
                .unwrap_or_default()
        });
        candidates
            .pop()
            .map(|path| path.join("bin"))
            .unwrap_or(exact)
    }
}

fn configured_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    command.env("PATH", runtime_path());
    // Prevent Python from importing a local agentseek source tree
    // that may shadow the installed package when CWD contains agentseek/.
    command.current_dir(std::env::temp_dir());
    // Clear Python env vars that may leak from conda/venv and cause agentseek
    // to import from the wrong environment.
    command.env_remove("PYTHONPATH");
    command.env_remove("PYTHONHOME");
    command.env_remove("VIRTUAL_ENV");
    command.env_remove("CONDA_PREFIX");
    command.env_remove("CONDA_DEFAULT_ENV");
    command.env_remove("CONDA_PROMPT_MODIFIER");
    command
}

fn curl_program() -> &'static str {
    if cfg!(target_os = "macos") {
        "/usr/bin/curl"
    } else {
        "curl"
    }
}

fn command_version(program: &str, arg: &str) -> Option<String> {
    let output = configured_command(program).arg(arg).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    combined
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().to_string())
}

fn version_at_least(value: &str, minimum: &[u64]) -> bool {
    let current = numeric_version(value);
    if current.is_empty() {
        return false;
    }
    for index in 0..minimum.len().max(current.len()) {
        let current_part = current.get(index).copied().unwrap_or(0);
        let minimum_part = minimum.get(index).copied().unwrap_or(0);
        if current_part != minimum_part {
            return current_part > minimum_part;
        }
    }
    true
}

fn meets_requirement(value: &str, minimum: &str) -> bool {
    version_at_least(value, &numeric_version(minimum))
}

fn platform_id() -> String {
    if cfg!(target_os = "macos") {
        return "macos".to_string();
    }
    if cfg!(target_os = "windows") {
        return "windows".to_string();
    }
    if cfg!(target_os = "linux") {
        let distribution = fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|content| {
                content.lines().find_map(|line| {
                    line.strip_prefix("ID=")
                        .map(|value| value.trim_matches(['\"', '\'']).to_lowercase())
                })
            })
            .unwrap_or_else(|| "linux".to_string());
        return match distribution.as_str() {
            "ubuntu" | "debian" | "linuxmint" | "pop" => "debian".to_string(),
            "centos" | "rhel" | "fedora" | "rocky" | "almalinux" | "ol" => "rhel".to_string(),
            _ => "linux".to_string(),
        };
    }
    "unknown".to_string()
}

fn dependency_commands(
    requirements: &RuntimeRequirements,
    platform: &str,
    managed_runtime_root: Option<&Path>,
    uv_available: bool,
    git_available: bool,
) -> HashMap<String, String> {
    let mut commands = HashMap::new();
    let node_version = &requirements.versions.node.managed;
    let node_major = numeric_version(node_version)
        .first()
        .copied()
        .unwrap_or_default();
    let nvm_version = &requirements.versions.nvm.managed;
    let nvm_installer = requirements
        .sources
        .nvm_installer_template
        .replace("{version}", nvm_version);
    commands.insert(
        "uv".to_string(),
        if uv_available {
            "uv self update".to_string()
        } else {
            format!("curl -LsSf {} | sh", requirements.sources.uv_installer)
        },
    );
    let managed_root = managed_runtime_root
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_else(|| "<AgentSeek data>/runtime".to_string());
    let managed_nvm = PathBuf::from(&managed_root).join("nvm");
    let managed_node_command = match platform {
        "macos" | "debian" | "rhel" | "linux" => format!(
            "unset npm_config_prefix && export NVM_DIR=\"{}\" PROFILE=/dev/null && curl -o- {} | bash && . \"{}/nvm.sh\" && nvm install {} && node --version && npm --version",
            managed_nvm.to_string_lossy(),
            nvm_installer,
            managed_nvm.to_string_lossy(),
            node_major,
        ),
        "windows" => format!(
            "Downloading Node.js {} official ZIP to {} (for AgentSeek Desktop only)",
            node_version, managed_root
        ),
        _ => format!(
            "Installing Node.js {} to AgentSeek Desktop private runtime directory {}",
            node_version, managed_root
        ),
    };
    let git = match platform {
        "macos" => {
            if git_available {
                "brew upgrade git"
            } else {
                "brew install git"
            }
        }
        "debian" => "sudo apt-get update && sudo apt-get install -y git",
        "rhel" => "sudo dnf install -y git",
        "windows" => "winget install --id Git.Git",
        _ => "Please install the required Git version using your system package manager",
    };
    commands.insert("node".to_string(), managed_node_command.clone());
    commands.insert("npm".to_string(), managed_node_command);
    commands.insert("git".to_string(), git.to_string());
    commands.insert(
        "agentseek".to_string(),
        "uv tool install --upgrade agentseek".to_string(),
    );
    commands
}

fn program_from_login_shell(program: &str) -> Option<String> {
    if program.is_empty()
        || !program.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
    {
        return None;
    }
    #[cfg(windows)]
    let output = Command::new("where.exe").arg(program).output().ok()?;
    #[cfg(not(windows))]
    let output = {
        let shell = env::var_os("SHELL")
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .unwrap_or_else(|| {
                if cfg!(target_os = "macos") {
                    PathBuf::from("/bin/zsh")
                } else {
                    PathBuf::from("/bin/bash")
                }
            });
        Command::new(shell)
            .args(["-lic", &format!("command -v {program}")])
            .stderr(Stdio::null())
            .output()
            .ok()?
    };
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .rev()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .find(|line| Path::new(line).is_file())
        .map(str::to_string)
}

fn resolved_program(program: &str, version_arg: &str) -> Option<String> {
    if command_version(program, version_arg).is_some() {
        return Some(program.to_string());
    }
    let resolved = program_from_login_shell(program)?;
    command_version(&resolved, version_arg).map(|_| resolved)
}

fn uv_program() -> Option<String> {
    if let Ok(program) = env::var("AGENTSEEK_DESKTOP_UV") {
        if command_version(&program, "--version").is_some() {
            return Some(program);
        }
    }
    let mut candidates = Vec::new();
    if let Some(home) = env::var_os("HOME") {
        candidates.push(PathBuf::from(&home).join(".local/bin/uv"));
        candidates.push(PathBuf::from(&home).join(".cargo/bin/uv"));
        candidates.push(PathBuf::from(home).join(".pyenv/shims/uv"));
    }
    candidates.extend([
        PathBuf::from("/opt/homebrew/bin/uv"),
        PathBuf::from("/usr/local/bin/uv"),
        PathBuf::from("/Library/Frameworks/Python.framework/Versions/3.9/bin/uv"),
    ]);
    let located = candidates
        .into_iter()
        .find(|candidate| command_version(&candidate.to_string_lossy(), "--version").is_some())
        .map(|candidate| candidate.to_string_lossy().to_string());
    if located.is_some() {
        return located;
    }
    resolved_program("uv", "--version")
}

fn uv_tool_bin_dir() -> String {
    uv_program()
        .and_then(|uv| {
            configured_command(&uv)
                .args(["tool", "dir", "--bin"])
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        })
        .unwrap_or_default()
}

fn agentseek_program() -> String {
    if let Ok(program) = env::var("AGENTSEEK_CLI") {
        return program;
    }
    if let Some(uv) = uv_program() {
        let output = configured_command(uv)
            .args(["tool", "dir", "--bin"])
            .output();
        if let Ok(output) = output {
            if output.status.success() {
                let bin_dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !bin_dir.is_empty() {
                    let executable = if cfg!(windows) {
                        "agentseek.exe"
                    } else {
                        "agentseek"
                    };
                    let candidate = PathBuf::from(bin_dir).join(executable);
                    if candidate.is_file() {
                        return candidate.to_string_lossy().to_string();
                    }
                }
            }
        }
    }
    if let Some(program) = resolved_program("agentseek", "--help") {
        return program;
    }
    "agentseek".to_string()
}

fn parse_uv_tool_version(content: &str, tool_name: &str) -> Option<String> {
    content.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let name = parts.next()?;
        let version = parts.next()?;
        if name == tool_name && version.starts_with('v') {
            Some(format!("{tool_name} {}", version.trim_start_matches('v')))
        } else {
            None
        }
    })
}

fn parse_agentseek_version(content: &str) -> Option<String> {
    content.lines().find_map(|line| {
        let trimmed = line.trim();
        let normalized = trimmed.to_ascii_lowercase();
        if normalized.starts_with("agentseek v") && !numeric_version(trimmed).is_empty() {
            Some(trimmed.to_string())
        } else {
            None
        }
    })
}

fn agentseek_command_version(program: &str) -> Option<String> {
    let output = configured_command(program).arg("version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let content = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    parse_agentseek_version(&content)
}

fn uv_tool_version(tool_name: &str) -> Option<String> {
    let uv = uv_program()?;
    let output = configured_command(uv)
        .args(["tool", "list"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let content = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    parse_uv_tool_version(&content, tool_name)
}

fn parse_agentseek_package_version(content: &[u8]) -> Result<String, String> {
    let metadata: serde_json::Value = serde_json::from_slice(content)
        .map_err(|error| format!("AgentSeek package metadata format error: {error}"))?;
    let version = metadata
        .get("info")
        .and_then(|info| info.get("version"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if numeric_version(&version).is_empty() {
        Err("AgentSeek package metadata has no valid latest version".to_string())
    } else {
        Ok(version)
    }
}

fn latest_agentseek_version(requirements: &RuntimeRequirements) -> Result<String, String> {
    if let Ok(version) = env::var("AGENTSEEK_DESKTOP_AGENTSEEK_LATEST_VERSION") {
        if !numeric_version(&version).is_empty() {
            return Ok(version);
        }
    }
    let output = configured_command(curl_program())
        .args([
            "-fsSL",
            "--connect-timeout",
            "5",
            "--max-time",
            "15",
            "--retry",
            "2",
            &requirements.sources.agentseek_package_metadata,
        ])
        .output()
        .map_err(|error| format!("Failed to query AgentSeek latest version: {error}"))?;
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !output.status.success() {
        let status = output
            .status
            .code()
            .map(|code| code.to_string())
            .unwrap_or_else(|| "terminated by system".to_string());
        return Err(if stderr.is_empty() {
            format!("Failed to query AgentSeek latest version (curl exit status: {status})")
        } else {
            format!("Failed to query AgentSeek latest version (curl exit status: {status}): {stderr}")
        });
    }
    parse_agentseek_package_version(&output.stdout)
}

fn cli_parts() -> (String, Vec<String>) {
    (agentseek_program(), Vec::new())
}

fn agentseek_update_available(installed: &str, latest: Option<&str>, available: bool) -> bool {
    available && latest.is_some_and(|latest_version| !meets_requirement(installed, latest_version))
}

fn current_cli_status(check_latest: bool) -> Result<CliStatus, String> {
    let requirements = load_runtime_requirements(DEFAULT_RUNTIME_REQUIREMENTS)?;
    let uv = uv_program();
    let uv_version = uv
        .as_deref()
        .and_then(|program| command_version(program, "--version"))
        .unwrap_or_default();
    let uv_path = uv.clone().unwrap_or_default();
    let program = agentseek_program();
    let cli_version = agentseek_command_version(&program)
        .or_else(|| command_version(&program, "--version"))
        .or_else(|| uv_tool_version("agentseek"))
        .unwrap_or_default();
    let cli_available = command_version(&program, "--help").is_some();
    let node_version = resolved_program("node", "--version")
        .and_then(|program| command_version(&program, "--version"))
        .unwrap_or_default();
    let npm_version = resolved_program("npm", "--version")
        .and_then(|program| command_version(&program, "--version"))
        .unwrap_or_default();
    let git_version = resolved_program("git", "--version")
        .and_then(|program| command_version(&program, "--version"))
        .unwrap_or_default();
    let uv_compatible = meets_requirement(&uv_version, &requirements.versions.uv.minimum);
    let node_compatible = meets_requirement(&node_version, &requirements.versions.node.minimum);
    let npm_compatible = meets_requirement(&npm_version, &requirements.versions.npm.minimum);
    let git_compatible = meets_requirement(&git_version, &requirements.versions.git.minimum);
    let cli_latest_version = check_latest
        .then(|| latest_agentseek_version(&requirements).ok())
        .flatten();
    let cli_latest_version_checked = cli_latest_version.is_some();
    let cli_compatible = meets_requirement(&cli_version, &requirements.versions.agentseek.minimum);
    let cli_update_available =
        agentseek_update_available(&cli_version, cli_latest_version.as_deref(), cli_available);
    let platform = platform_id();
    let runtime_root = managed_runtime_root();
    let dependency_commands = dependency_commands(
        &requirements,
        &platform,
        runtime_root.as_deref(),
        !uv_version.is_empty(),
        !git_version.is_empty(),
    );
    let node_managed = runtime_root
        .as_deref()
        .map(|root| managed_node_bin(root, &requirements.versions.node.managed))
        .map(|bin| {
            bin.join(if cfg!(windows) { "node.exe" } else { "node" })
                .is_file()
        })
        .unwrap_or(false);
    let prerequisites_ready = uv_compatible && node_compatible && npm_compatible && cli_compatible;
    let minimum_versions = [
        ("uv", requirements.versions.uv.minimum.as_str()),
        ("node", requirements.versions.node.minimum.as_str()),
        ("npm", requirements.versions.npm.minimum.as_str()),
        ("git", requirements.versions.git.minimum.as_str()),
        (
            "agentseek",
            requirements.versions.agentseek.minimum.as_str(),
        ),
    ]
    .into_iter()
    .map(|(name, version)| (name.to_string(), version.to_string()))
    .collect();
    Ok(CliStatus {
        platform,
        dependency_commands,
        minimum_versions,
        node_managed,
        uv_available: !uv_version.is_empty(),
        uv_path,
        cli_available,
        cli_compatible,
        cli_update_available,
        cli_latest_version: cli_latest_version.unwrap_or_default(),
        cli_latest_version_checked,
        uv_version,
        cli_version,
        node_available: !node_version.is_empty(),
        node_compatible,
        node_version,
        npm_available: !npm_version.is_empty(),
        npm_compatible,
        npm_version,
        git_available: !git_version.is_empty(),
        git_compatible,
        git_version,
        uv_compatible,
        prerequisites_ready,
        install_command: "uv tool install agentseek".to_string(),
    })
}

fn run_cli(args: &[&str], cwd: Option<&Path>) -> Result<CommandResult, String> {
    let (program, prefix) = cli_parts();
    let mut command = configured_command(&program);
    command.args(&prefix).args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let printable = std::iter::once(program.as_str())
        .chain(prefix.iter().map(String::as_str))
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ");
    let output = command
        .output()
        .map_err(|error| format!("Unable to execute {printable}: {error}"))?;
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(CommandResult {
        code: output.status.code().unwrap_or(1),
        output: combined.trim().to_string(),
        command: printable,
    })
}

fn parse_templates(output: &str) -> Vec<TemplateInfo> {
    let mut templates = Vec::new();
    let mut current: Option<usize> = None;
    for line in output.lines() {
        let trimmed = line.trim();
        let is_template = trimmed.contains('/')
            && !trimmed.contains(' ')
            && !trimmed.starts_with("http")
            && trimmed.split('/').count() == 2;
        if is_template {
            let framework = trimmed.split('/').next().unwrap_or_default().to_string();
            templates.push(TemplateInfo {
                id: trimmed.to_string(),
                name: display_name(trimmed),
                description: String::new(),
                framework,
            });
            current = Some(templates.len() - 1);
        } else if let Some(index) = current {
            if !trimmed.is_empty()
                && !trimmed.chars().all(|character| character == '─')
                && !trimmed.contains("templates)")
            {
                templates[index].description = trimmed.to_string();
                current = None;
            }
        }
    }
    templates
}

/// Return the template cache directory path.
fn template_cache_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(|home| Path::new(&home).join(".cookiecutters").join("agentseek"))
}

/// Parsed template source from a URL.
#[derive(Debug, PartialEq)]
enum TemplateSource {
    /// Git tree URL: https://github.com/org/repo/tree/branch/path
    Tree {
        repo_url: String,
        branch: String,
        sub_path: String,
    },
    /// GitHub releases URL (fetch latest release tag dynamically)
    Releases {
        repo_url: String,
    },
    /// Plain git repository URL (clone entire repo)
    Repo { repo_url: String },
}

impl TemplateSource {
    /// Returns the GitHub releases API URL and the latest release tag.
    fn releases_info(&self) -> Option<(String, String)> {
        let repo_url = match self {
            TemplateSource::Tree { repo_url, .. } => repo_url,
            TemplateSource::Releases { repo_url } => repo_url,
            TemplateSource::Repo { repo_url } => repo_url,
        };
        let api_url = github_releases_api(repo_url)?;
        Some((api_url, repo_url.clone()))
    }

    /// Derive just the repo URL from the source (for backward compatibility).
    pub(crate) fn repo_url(&self) -> Option<String> {
        match self {
            TemplateSource::Tree { repo_url, .. } => Some(repo_url.clone()),
            TemplateSource::Releases { repo_url } => Some(repo_url.clone()),
            TemplateSource::Repo { repo_url } => Some(repo_url.clone()),
        }
    }
}

/// Derive GitHub releases API URL from a repository URL.
/// e.g. "https://github.com/org/repo" -> "https://api.github.com/repos/org/repo/releases/latest"
fn github_releases_api(repo_url: &str) -> Option<String> {
    let repo_url = repo_url.trim_end_matches('/').trim_end_matches(".git");
    // Extract org/repo from URL like https://github.com/org/repo or git@github.com:org/repo.git
    let path = if let Some(rest) = repo_url.strip_prefix("https://github.com/") {
        rest
    } else if let Some(rest) = repo_url.strip_prefix("http://github.com/") {
        rest
    } else if let Some(rest) = repo_url.strip_prefix("git@github.com:") {
        rest
    } else {
        return None;
    };
    let parts: Vec<&str> = path.split('/').take(2).collect();
    if parts.len() != 2 {
        return None;
    }
    Some(format!("https://api.github.com/repos/{}/{}/releases/latest", parts[0], parts[1]))
}

/// Query the GitHub releases API for the latest release tag of the template repository.
fn latest_template_release_tag(api_url: &str) -> Option<String> {
    let output = configured_command(curl_program())
        .args([
            "-fsSL",
            "--connect-timeout",
            "5",
            "--max-time",
            "15",
            "--retry",
            "2",
            "-H",
            "Accept: application/vnd.github+json",
            api_url,
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    json.get("tag_name")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Return the currently checked-out template version (git tag) from the cache directory.
fn current_template_version() -> Option<String> {
    let cache_dir = template_cache_dir()?;
    if !cache_dir.is_dir() {
        return None;
    }
    let output = configured_command("git")
        .args(["describe", "--tags", "--exact-match", "HEAD"])
        .current_dir(&cache_dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let tag = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if tag.is_empty() { None } else { Some(tag) }
}

/// Parse a template source URL into a TemplateSource variant.
fn parse_template_source_url(url: &str) -> Result<TemplateSource, String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("Template URL cannot be empty".to_string());
    }
    // Disallow insecure HTTP URLs
    if url.starts_with("http://") && !url.starts_with("http://localhost") {
        return Err("Insecure HTTP URLs are not supported, use HTTPS instead".to_string());
    }
    // Tree URL: https://github.com/org/repo/tree/branch/path
    if url.starts_with("https://") || url.starts_with("http://") {
        if let Some(pos) = url.find("/tree/") {
            let repo_url = url[..pos].to_string();
            let remainder = &url[pos + 6..]; // skip "/tree/"
            let (branch, sub_path) = match remainder.find('/') {
                Some(slash_pos) => (
                    remainder[..slash_pos].to_string(),
                    remainder[slash_pos + 1..].to_string(),
                ),
                None => (remainder.to_string(), String::new()),
            };
            if sub_path.contains("..") {
                return Err("Path traversal is not allowed in sub_path".to_string());
            }
            if branch.is_empty() {
                return Err("Tree URL has empty branch".to_string());
            }
            return Ok(TemplateSource::Tree { repo_url, branch, sub_path });
        }
        // Releases URL: https://github.com/org/repo/releases or https://github.com/org/repo/releases/latest
        if url.ends_with("/releases") || url.ends_with("/releases/latest") {
            let repo_url = url.strip_suffix("/releases").or_else(|| url.strip_suffix("/releases/latest"))
                .unwrap_or(url).to_string();
            return Ok(TemplateSource::Releases { repo_url });
        }
    }
    // Plain repo URL
    if url.starts_with("https://") || url.starts_with("git@") {
        return Ok(TemplateSource::Repo { repo_url: url.to_string() });
    }
    Err(format!("Invalid template URL: {url}"))
}


/// Replace the cache directory with a subdirectory's content.
/// Moves `sub_path` inside `cache_dir` to become the new cache root.
fn promote_subdirectory(cache_dir: &Path, sub_path: &str) -> Result<(), String> {
    let source_path = cache_dir.join(sub_path);
    if !source_path.is_dir() {
        return Err(format!("Path '{sub_path}' not found in downloaded content"));
    }
    let temp_dir = cache_dir.with_extension("promote-temp");
    if temp_dir.exists() {
        let _ = fs::remove_dir_all(&temp_dir);
    }
    fs::rename(&source_path, &temp_dir)
        .map_err(|e| format!("Failed to move '{sub_path}': {e}"))?;
    if let Err(e) = fs::remove_dir_all(cache_dir) {
        // Attempt to restore on failure
        let _ = fs::rename(&temp_dir, &source_path);
        return Err(format!("Failed to clean cache directory: {e}"));
    }
    fs::rename(&temp_dir, cache_dir)
        .map_err(|e| format!("Failed to finalize cache directory: {e}"))
}

/// Fetch templates from a parsed source URL into the cache directory.
fn fetch_templates(cache_dir: &Path, source: &TemplateSource) -> Result<(), String> {
    let parent = cache_dir.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .map_err(|e| format!("Failed to create parent directory: {e}"))?;
    match source {
        TemplateSource::Tree { repo_url, branch, sub_path } => {
            fetch_from_tree(cache_dir, repo_url, branch, sub_path)
        }
        TemplateSource::Releases { .. } | TemplateSource::Repo { .. } => {
            fetch_from_repo(cache_dir, "", source)
        }
    }
}

/// Clone a git repo at a specific branch, optionally promoting a subdirectory.
fn fetch_from_tree(cache_dir: &Path, repo_url: &str, branch: &str, sub_path: &str) -> Result<(), String> {
    eprintln!("[templates] Cloning {repo_url} (branch: {branch})...");
    if cache_dir.is_dir() {
        fs::remove_dir_all(cache_dir)
            .map_err(|e| format!("Failed to remove template cache: {e}"))?;
    }
    let clone_ok = configured_command("git")
        .args(["clone", "--quiet", "--depth", "1", "--branch", branch, repo_url, cache_dir.to_str().unwrap_or("")])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !clone_ok {
        return Err(format!("Failed to clone branch '{branch}' from {repo_url}"));
    }
    if !sub_path.is_empty() {
        promote_subdirectory(cache_dir, sub_path)?;
    }
    eprintln!("[templates] Tree source ready at {}", cache_dir.display());
    Ok(())
}

/// Clone the entire repository, optionally checking out the latest release tag.
fn fetch_from_repo(cache_dir: &Path, _repo_url_arg: &str, source: &TemplateSource) -> Result<(), String> {
    let repo_url = source.repo_url().unwrap_or_default();
    eprintln!("[templates] Cloning repository {}...", repo_url);
    if cache_dir.is_dir() {
        fs::remove_dir_all(cache_dir)
            .map_err(|e| format!("Failed to remove template cache: {e}"))?;
    }
    // Determine git URL and optional tag
    let (git_url, checkout_tag) = match source {
        TemplateSource::Releases { .. } => {
            // For Releases type: clone without --branch first, then fetch + checkout tag
            let api_url = source.releases_info().and_then(|(api, _)| {
                latest_template_release_tag(&api).map(|tag| (api, tag))
            }).map(|(api, _)| api);
            match api_url {
                Some(api) => {
                    let tag = latest_template_release_tag(&api).unwrap_or_default();
                    (Some(repo_url.clone()), if tag.is_empty() { None } else { Some(tag) })
                }
                None => (Some(repo_url.clone()), None),
            }
        }
        TemplateSource::Repo { .. } => (Some(repo_url.clone()), None),
        _ => return Err("Invalid source type for fetch_from_repo".to_string()),
    };
    let url = git_url.as_ref().map(|s| s.as_str()).unwrap_or("");
    if url.is_empty() {
        return Err("Repository URL is empty".to_string());
    }
    let clone_ok = configured_command("git")
        .args(["clone", "--quiet", url, cache_dir.to_str().unwrap_or("")])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !clone_ok {
        return Err(format!("Failed to clone template repository: {url}"));
    }
    // Checkout the latest release tag if available
    if let Some(ref tag) = checkout_tag {
        eprintln!("[templates] Checking out release tag: {tag}");
        let _ = configured_command("git")
            .args(["fetch", "origin", "--quiet", "--tags"])
            .current_dir(cache_dir)
            .output();
        let _ = configured_command("git")
            .args(["checkout", tag, "--quiet"])
            .current_dir(cache_dir)
            .output();
    }
    eprintln!("[templates] Repository cloned to {}", cache_dir.display());
    Ok(())
}

/// Resolve the effective template URL: user override takes precedence over the default.
fn resolve_template_url(user_url: &str) -> String {
    let user_url = user_url.trim();
    if !user_url.is_empty() {
        return user_url.to_string();
    }
    load_runtime_requirements(DEFAULT_RUNTIME_REQUIREMENTS)
        .map(|r| r.sources.template_repo_url)
        .unwrap_or_else(|_| "https://github.com/agentseek-ai/agentseek-templates/releases".to_string())
}

/// Delete the template cache and re-fetch templates from the configured URL.
fn update_template_cache(template_url: &str) -> Result<(), String> {
    let cache_dir = template_cache_dir()
        .ok_or_else(|| "HOME environment variable is not set".to_string())?;
    let url = resolve_template_url(template_url);
    let source = parse_template_source_url(&url)?;
    fetch_templates(&cache_dir, &source)
}

/// Ensure the template cache exists. Called lazily when listing templates.
fn ensure_template_cache(template_url: &str) {
    let Some(cache_dir) = template_cache_dir() else {
        return;
    };
    if cache_dir.is_dir() {
        return;
    }
    let url = resolve_template_url(template_url);
    if let Ok(source) = parse_template_source_url(&url) {
        let _ = fetch_templates(&cache_dir, &source);
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod template_url_tests {
    use super::*;

    #[test]
    fn parse_tree_url_with_subpath() {
        let result = parse_template_source_url("https://github.com/agentseek-ai/agentseek-templates/tree/main/templates");
        assert_eq!(result.unwrap(), TemplateSource::Tree {
            repo_url: "https://github.com/agentseek-ai/agentseek-templates".to_string(),
            branch: "main".to_string(),
            sub_path: "templates".to_string(),
        });
    }

    #[test]
    fn parse_tree_url_without_subpath() {
        let result = parse_template_source_url("https://github.com/org/repo/tree/develop");
        assert_eq!(result.unwrap(), TemplateSource::Tree {
            repo_url: "https://github.com/org/repo".to_string(),
            branch: "develop".to_string(),
            sub_path: String::new(),
        });
    }

    #[test]
    fn parse_tree_url_with_nested_subpath() {
        let result = parse_template_source_url("https://github.com/org/repo/tree/main/path/to/templates");
        assert_eq!(result.unwrap(), TemplateSource::Tree {
            repo_url: "https://github.com/org/repo".to_string(),
            branch: "main".to_string(),
            sub_path: "path/to/templates".to_string(),
        });
    }

    #[test]
    fn parse_plain_repo_url() {
        let result = parse_template_source_url("https://github.com/agentseek-ai/agentseek-templates");
        assert_eq!(result.unwrap(), TemplateSource::Repo {
            repo_url: "https://github.com/agentseek-ai/agentseek-templates".to_string(),
        });
    }

    #[test]
    fn parse_git_ssh_url() {
        let result = parse_template_source_url("git@github.com:org/repo.git");
        assert_eq!(result.unwrap(), TemplateSource::Repo {
            repo_url: "git@github.com:org/repo.git".to_string(),
        });
    }

    #[test]
    fn parse_empty_url_fails() {
        assert!(parse_template_source_url("").is_err());
        assert!(parse_template_source_url("   ").is_err());
    }

    #[test]
    fn parse_invalid_url_fails() {
        assert!(parse_template_source_url("ftp://example.com/repo").is_err());
        assert!(parse_template_source_url("not-a-url").is_err());
    }

    #[test]
    fn parse_url_with_whitespace_is_trimmed() {
        let result = parse_template_source_url("  https://github.com/org/repo  ");
        assert_eq!(result.unwrap(), TemplateSource::Repo {
            repo_url: "https://github.com/org/repo".to_string(),
        });
    }

    #[test]
    fn github_releases_api_from_https() {
        let api = github_releases_api("https://github.com/org/repo");
        assert_eq!(api.unwrap(), "https://api.github.com/repos/org/repo/releases/latest");
    }

    #[test]
    fn github_releases_api_from_https_with_git_suffix() {
        let api = github_releases_api("https://github.com/org/repo.git");
        assert_eq!(api.unwrap(), "https://api.github.com/repos/org/repo/releases/latest");
    }

    #[test]
    fn github_releases_api_from_ssh() {
        let api = github_releases_api("git@github.com:org/repo.git");
        assert_eq!(api.unwrap(), "https://api.github.com/repos/org/repo/releases/latest");
    }

    #[test]
    fn github_releases_api_from_non_github() {
        assert!(github_releases_api("https://gitlab.com/org/repo").is_none());
    }

    #[test]
    fn template_source_releases_info() {
        let tree = TemplateSource::Tree {
            repo_url: "https://github.com/org/repo".to_string(),
            branch: "main".to_string(),
            sub_path: "templates".to_string(),
        };
        let (api_url, repo_url) = tree.releases_info().unwrap();
        assert_eq!(api_url, "https://api.github.com/repos/org/repo/releases/latest");
        assert_eq!(repo_url, "https://github.com/org/repo");
        
        let releases = TemplateSource::Releases {
            repo_url: "https://github.com/other/repo".to_string(),
        };
        let (api_url, repo_url) = releases.releases_info().unwrap();
        assert_eq!(api_url, "https://api.github.com/repos/other/repo/releases/latest");
        assert_eq!(repo_url, "https://github.com/other/repo");
    }

    #[test]
    fn resolve_template_url_prefers_user_override() {
        assert_eq!(resolve_template_url("https://custom.example.com/templates"), "https://custom.example.com/templates");
    }

    #[test]
    fn resolve_template_url_trims_whitespace() {
        assert_eq!(resolve_template_url("  https://custom.example.com/templates  "), "https://custom.example.com/templates");
    }
}
