use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use clap::Parser;

use pprr::app::{App, AppState, install_panic_hook, restore_terminal, run, setup_terminal};
use pprr::config;
use pprr::data::gh::{GhCli, GhClient};
use pprr::data::git::{GitCli, GitClient};

#[derive(Debug, Parser)]
#[command(name = "pprr", version, about = "TUI PR review")]
struct Cli {
    /// Override window_size from the config file.
    #[arg(long)]
    window_size: Option<usize>,
    /// Hide the commit strip on launch.
    #[arg(long)]
    no_commit_strip: bool,
}

fn main() {
    if let Err(e) = real_main() {
        let _ = restore_terminal();
        eprintln!("pprr: {e:?}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let cli = Cli::parse();
    let mut cfg = config::load()?;
    if let Some(n) = cli.window_size {
        cfg.window_size = n;
    }
    if cli.no_commit_strip {
        cfg.show_commit_strip = false;
    }

    if !is_tty() {
        return Err(anyhow!("pprr requires a TTY"));
    }
    if std::env::var("COLORTERM")
        .map(|v| !(v == "truecolor" || v == "24bit"))
        .unwrap_or(true)
    {
        eprintln!("pprr: COLORTERM is not 'truecolor' — colors may render incorrectly");
    }

    let gh: Arc<dyn GhClient> = Arc::new(GhCli);
    let git: Arc<dyn GitClient> = Arc::new(GitCli);

    gh.auth_status()
        .context("gh auth status failed (run `gh auth login`)")?;

    let cwd = std::env::current_dir()?;
    let repo_root = git.repo_root(&cwd).context("not inside a git repo")?;
    if !git.has_github_remote(&repo_root)? {
        return Err(anyhow!("no github.com remote in {}", repo_root.display()));
    }

    let repo_name = repo_root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let branch = current_branch(&repo_root).unwrap_or_else(|| "?".into());

    let mut app = App::new(repo_root, gh, git, cfg);
    let mut st = AppState::new(repo_name, branch);

    install_panic_hook();
    let mut term = setup_terminal()?;
    let result = run(&mut term, &mut app, &mut st);
    restore_terminal()?;
    result
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
