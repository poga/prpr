use std::sync::Arc;
use std::thread;

use anyhow::{Context, Result, anyhow};
use clap::Parser;

use prpr::app::{App, AppState, install_panic_hook, restore_terminal, run, setup_terminal};
use prpr::config;
use prpr::data::gh::{GhCli, GhClient};
use prpr::data::git::{GitCli, GitClient};
use prpr::data::projects::{self, Repo};

#[derive(Debug, Parser)]
#[command(name = "prpr", version, about = "TUI PR review")]
struct Cli {
    /// Override window_size from the config file.
    #[arg(long)]
    window_size: Option<usize>,
    /// Review every GitHub clone that is a direct child of the current folder.
    #[arg(long)]
    projects: bool,
}

fn main() {
    if let Err(e) = real_main() {
        let _ = restore_terminal();
        eprintln!("prpr: {e:?}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let cli = Cli::parse();
    let mut cfg = config::load()?;
    if let Some(n) = cli.window_size {
        cfg.window_size = n;
    }

    if !is_tty() {
        return Err(anyhow!("prpr requires a TTY"));
    }
    if std::env::var("COLORTERM")
        .map(|v| !(v == "truecolor" || v == "24bit"))
        .unwrap_or(true)
    {
        eprintln!("prpr: COLORTERM is not 'truecolor' — colors may render incorrectly");
    }

    let gh: Arc<dyn GhClient> = Arc::new(GhCli);
    let git: Arc<dyn GitClient> = Arc::new(GitCli);

    let cwd = std::env::current_dir()?;
    let (repos, st) = if cli.projects {
        projects_startup(&cwd, git.as_ref())?
    } else {
        single_startup(&cwd, git.as_ref())?
    };
    let mut st = st;
    let mut app = App::new(repos, gh, git, cfg);

    // Syntax definitions take ~150ms to load; warm them while the list loads.
    thread::spawn(prpr::render::syntax::warm);

    install_panic_hook();
    let mut term = setup_terminal()?;
    let result = run(&mut term, &mut app, &mut st);
    restore_terminal()?;
    result
}

/// Single-repo mode: the clone containing cwd.
fn single_startup(cwd: &std::path::Path, git: &dyn GitClient) -> Result<(Vec<Repo>, AppState)> {
    let repo_root = git.repo_root(cwd).context(
        "not inside a git repo (run `prpr --projects` to review every clone under this folder)",
    )?;

    // Run the two remaining git probes in parallel — they're independent and
    // each spawns a subprocess. `current_branch` is cosmetic (title bar), so
    // `has_github_remote` is the only one that gates startup.
    let (has_remote, branch) = thread::scope(|s| {
        let root = repo_root.as_path();
        let remote_h = s.spawn(move || git.has_github_remote(root));
        let branch_h = s.spawn(move || current_branch(root));
        (remote_h.join().unwrap(), branch_h.join().unwrap())
    });
    if !has_remote? {
        return Err(anyhow!("no github.com remote in {}", repo_root.display()));
    }
    let branch = branch.unwrap_or_else(|| "?".into());

    let repo = Repo::from_root(repo_root);
    let st = AppState::new(repo.name.clone(), branch);
    Ok((vec![repo], st))
}

/// Projects mode: every GitHub clone directly under cwd.
fn projects_startup(cwd: &std::path::Path, git: &dyn GitClient) -> Result<(Vec<Repo>, AppState)> {
    if let Ok(root) = git.repo_root(cwd) {
        return Err(anyhow!(
            "--projects must run from a folder that contains clones, not inside one ({})",
            root.display()
        ));
    }
    let repos = projects::discover(cwd, git)?;
    if repos.is_empty() {
        return Err(anyhow!("no GitHub clones found under {}", cwd.display()));
    }
    let folder = cwd
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| cwd.display().to_string());
    let st = AppState::new_projects(folder, repos.len());
    Ok((repos, st))
}

fn is_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
}

fn current_branch(repo_root: &std::path::Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .current_dir(repo_root)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
