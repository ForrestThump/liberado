//! Thin external-process adapters for Hermes and Deep Agents.

use super::*;

pub(super) struct HermesAdapter<'a> {
    pub(super) manifest: &'a CompareManifest,
    pub(super) args: &'a RunArgs,
    pub(super) session_id: String,
    pub(super) credential: &'a ResolvedCredential,
}

impl HarnessAdapter for HermesAdapter<'_> {
    fn id(&self) -> &'static str {
        "hermes"
    }

    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn preflight(&self) -> Result<AdapterPreflight, Box<dyn Error>> {
        preflight_external_bin(
            self.id(),
            "Hermes",
            self.args.hermes_bin.as_deref(),
            crate::adapter::default_path_program(self.id()),
        )
    }

    fn launch(&self) -> Result<HarnessExecution, Box<dyn Error>> {
        let prompt = self.manifest.run_root.join("task.txt");
        let exit_code = run_hermes(
            self.manifest,
            self.args,
            &prompt,
            &self.session_id,
            self.credential,
            "session",
        )?;
        Ok(HarnessExecution {
            harness: self.id().to_string(),
            session_id: self.session_id.clone(),
            exit_code,
        })
    }

    fn run(&self, prompt: &str, stem: &str) -> Result<i32, Box<dyn Error>> {
        let path = self
            .manifest
            .run_root
            .join("artifacts")
            .join("hermes")
            .join(format!("{stem}.prompt.txt"));
        fs::write(&path, prompt)?;
        run_hermes(
            self.manifest,
            self.args,
            &path,
            &self.session_id,
            self.credential,
            stem,
        )
    }
}

pub(super) struct DeepAgentsAdapter<'a> {
    pub(super) manifest: &'a CompareManifest,
    pub(super) args: &'a RunArgs,
    pub(super) session_id: String,
    pub(super) credential: &'a ResolvedCredential,
}

impl HarnessAdapter for DeepAgentsAdapter<'_> {
    fn id(&self) -> &'static str {
        "deepagents"
    }

    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn preflight(&self) -> Result<AdapterPreflight, Box<dyn Error>> {
        preflight_external_bin(
            self.id(),
            "Deep Agents",
            self.args.deep_agents_bin.as_deref(),
            crate::adapter::default_path_program(self.id()),
        )
    }

    fn launch(&self) -> Result<HarnessExecution, Box<dyn Error>> {
        let prompt = self.manifest.run_root.join("task.txt");
        let exit_code = run_deepagents(
            self.manifest,
            self.args,
            &prompt,
            &self.session_id,
            self.credential,
            "session",
        )?;
        Ok(HarnessExecution {
            harness: self.id().to_string(),
            session_id: self.session_id.clone(),
            exit_code,
        })
    }

    fn run(&self, prompt: &str, stem: &str) -> Result<i32, Box<dyn Error>> {
        let path = self
            .manifest
            .run_root
            .join("artifacts")
            .join("deepagents")
            .join(format!("{stem}.prompt.txt"));
        fs::write(&path, prompt)?;
        run_deepagents(
            self.manifest,
            self.args,
            &path,
            &self.session_id,
            self.credential,
            stem,
        )
    }
}

fn preflight_external_bin(
    harness: &str,
    label: &str,
    explicit: Option<&Path>,
    path_default: Option<&str>,
) -> Result<AdapterPreflight, Box<dyn Error>> {
    let executable = explicit
        .map(PathBuf::from)
        .or_else(|| path_default.map(PathBuf::from))
        .ok_or_else(|| format!("no executable configured for harness '{harness}'"))?;
    if explicit.is_some() && !executable.is_file() {
        return Err(format!("{label} binary does not exist: {}", executable.display()).into());
    }
    Ok(AdapterPreflight {
        harness: harness.to_string(),
        executable: path_text(&executable),
    })
}

fn run_hermes(
    manifest: &CompareManifest,
    args: &RunArgs,
    prompt_file: &Path,
    _session_id: &str,
    credential: &ResolvedCredential,
    stem: &str,
) -> Result<i32, Box<dyn Error>> {
    let layout = harness(manifest, "hermes")?;
    let binary = args.hermes_bin.clone().unwrap_or_else(|| {
        PathBuf::from(crate::adapter::default_path_program("hermes").unwrap_or("hermes"))
    });
    let home = layout.artifacts.join("home");
    fs::create_dir_all(&home)?;
    let mut cmd = std_command(&binary);
    cmd.args(["chat", "--oneshot", "--query-file"])
        .arg(prompt_file)
        .args(["--provider", &args.provider, "--model", &args.model])
        .args(["--reasoning", &args.thinking, "--yolo", "--source", "tool"])
        .current_dir(&layout.worktree)
        .env("CARGO_TARGET_DIR", &layout.target_dir)
        .env("HERMES_HOME", &home)
        .env(&args.api_key_env, credential.expose());
    execute_logged(
        &mut cmd,
        layout,
        stem,
        args.run_timeout_secs,
        args.cancel_file.as_deref(),
    )
}

fn run_deepagents(
    manifest: &CompareManifest,
    args: &RunArgs,
    prompt_file: &Path,
    _session_id: &str,
    credential: &ResolvedCredential,
    stem: &str,
) -> Result<i32, Box<dyn Error>> {
    let layout = harness(manifest, "deepagents")?;
    let binary = args.deep_agents_bin.clone().unwrap_or_else(|| {
        PathBuf::from(crate::adapter::default_path_program("deepagents").unwrap_or("dcode"))
    });
    let home = layout.artifacts.join("home");
    fs::create_dir_all(&home)?;
    let model = deepagents_model(&args.provider, &args.model);
    let mut cmd = std_command(&binary);
    cmd.args(["--stdin", "--model", &model, "--shell-allow-list", "all"])
        .current_dir(&layout.worktree)
        .stdin(Stdio::from(File::open(prompt_file)?))
        .env("CARGO_TARGET_DIR", &layout.target_dir)
        .env("DEEPAGENTS_HOME", &home)
        .env(&args.api_key_env, credential.expose());
    execute_logged(
        &mut cmd,
        layout,
        stem,
        args.run_timeout_secs,
        args.cancel_file.as_deref(),
    )
}

pub(super) fn deepagents_model(provider: &str, model: &str) -> String {
    if model.contains(':') {
        model.to_string()
    } else {
        format!("{provider}:{model}")
    }
}

pub(super) fn append_external_harness_pins(pins: &mut String, args: &RunArgs) {
    append_one_external_pin(
        pins,
        args,
        "hermes",
        args.hermes_bin.as_deref(),
        "hermes",
        args.hermes_git_sha.as_deref(),
        "unset (hermes native default; CLI --max-turns default 500)",
    );
    append_one_external_pin(
        pins,
        args,
        "deepagents",
        args.deep_agents_bin.as_deref(),
        "dcode",
        args.deep_agents_git_sha.as_deref(),
        "unset (deepagents native default; headless HITL safety cap 50)",
    );
}

fn append_one_external_pin(
    pins: &mut String,
    args: &RunArgs,
    id: &str,
    binary: Option<&Path>,
    path_default: &str,
    git_sha: Option<&str>,
    turn_cap: &str,
) {
    if !args.run_order.iter().any(|have| have == id) {
        return;
    }
    pins.push_str(&format!(
        "{id}_bin={}\n{id}_git_sha={}\n{id}_turn_cap={turn_cap}\n",
        pin_bin(binary, path_default),
        git_sha.unwrap_or("unset"),
    ));
}

fn pin_bin(explicit: Option<&Path>, path_default: &str) -> String {
    explicit
        .map(path_text)
        .unwrap_or_else(|| format!("PATH:{path_default}"))
}
