//! The `nash` command line interface.
//!
//! A thin, scriptable front end over the Nash engine: inspect providers, list
//! models, run connection tests, chat (streaming or not), drive the coding
//! agent, and manage configuration, secrets and sessions.

mod commands;
mod paths;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Nash AI Engine — universal provider-agnostic AI engine and coding agent.
#[derive(Parser)]
#[command(name = "nash", version, about, long_about = None)]
struct Cli {
    /// Path to the configuration file.
    #[arg(long, global = true, env = "NASH_CONFIG")]
    config: Option<PathBuf>,

    /// Path to the SQLite database.
    #[arg(long, global = true, env = "NASH_DB")]
    db: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List configured providers.
    Providers,

    /// List the models a provider advertises.
    Models {
        /// Provider id.
        provider: String,
    },

    /// Run connection and generation diagnostics.
    Test {
        /// Provider id (omit to test all).
        provider: Option<String>,
        /// Test every configured provider.
        #[arg(long)]
        all: bool,
    },

    /// Send a single prompt to a model.
    Chat {
        /// The prompt text.
        prompt: String,
        /// Provider id to source the default model from.
        #[arg(long)]
        provider: Option<String>,
        /// Model id (overrides the provider default).
        #[arg(long)]
        model: Option<String>,
        /// System prompt prepended to the conversation.
        #[arg(long)]
        system: Option<String>,
        /// Disable streaming; wait for the full response.
        #[arg(long)]
        no_stream: bool,
        /// Continue an existing session by id.
        #[arg(long)]
        session: Option<String>,
        /// Persist this exchange as a new session.
        #[arg(long)]
        save: bool,
    },

    /// Run the coding agent on a task.
    Agent {
        /// The task to perform.
        task: String,
        /// Working directory the tools operate in.
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// Model id.
        #[arg(long)]
        model: Option<String>,
        /// System prompt for the agent.
        #[arg(long)]
        system: Option<String>,
        /// Approval mode: auto, ask, or deny.
        #[arg(long, default_value = "ask")]
        approve: String,
        /// Maximum number of model turns.
        #[arg(long, default_value_t = 8)]
        max_steps: usize,
    },

    /// Inspect and edit configuration.
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },

    /// Manage credentials in the OS keyring.
    Secret {
        #[command(subcommand)]
        cmd: SecretCmd,
    },

    /// List and inspect saved sessions.
    Sessions {
        #[command(subcommand)]
        cmd: SessionsCmd,
    },
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Print the effective configuration as JSON.
    Show,
    /// Print the resolved config and database paths.
    Paths,
    /// Write a default configuration file.
    Init {
        /// Overwrite an existing file.
        #[arg(long)]
        force: bool,
    },
    /// Set the default provider.
    SetDefault {
        /// Provider id.
        provider: String,
    },
}

#[derive(Subcommand)]
enum SecretCmd {
    /// Store a secret (prompts if no value is given).
    Set {
        /// Secret name (referenced by a provider's `secret_ref`).
        name: String,
        /// The secret value (avoid on shared shells; prompted when omitted).
        value: Option<String>,
    },
    /// List stored secret names.
    List,
    /// Remove a secret.
    Remove {
        /// Secret name.
        name: String,
    },
}

#[derive(Subcommand)]
enum SessionsCmd {
    /// List saved sessions.
    List {
        /// Maximum number to show.
        #[arg(long, default_value_t = 20)]
        limit: i64,
    },
    /// Show a session and its messages.
    Show {
        /// Session id.
        id: String,
    },
    /// Delete a session.
    Delete {
        /// Session id.
        id: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();
    let ctx = commands::Ctx::new(cli.config, cli.db);

    match cli.command {
        Command::Providers => commands::providers(&ctx)?,
        Command::Models { provider } => commands::models(&ctx, &provider).await?,
        Command::Test { provider, all } => commands::test(&ctx, provider.as_deref(), all).await?,
        Command::Chat {
            prompt,
            provider,
            model,
            system,
            no_stream,
            session,
            save,
        } => {
            commands::chat(
                &ctx,
                &prompt,
                provider.as_deref(),
                model.as_deref(),
                system.as_deref(),
                !no_stream,
                session.as_deref(),
                save,
            )
            .await?
        }
        Command::Agent {
            task,
            dir,
            model,
            system,
            approve,
            max_steps,
        } => {
            commands::agent(
                &ctx,
                &task,
                &dir,
                model.as_deref(),
                system.as_deref(),
                &approve,
                max_steps,
            )
            .await?
        }
        Command::Config { cmd } => match cmd {
            ConfigCmd::Show => commands::config_show(&ctx)?,
            ConfigCmd::Paths => commands::config_paths(&ctx)?,
            ConfigCmd::Init { force } => commands::config_init(&ctx, force)?,
            ConfigCmd::SetDefault { provider } => commands::config_set_default(&ctx, &provider)?,
        },
        Command::Secret { cmd } => match cmd {
            SecretCmd::Set { name, value } => {
                commands::secret_set(&ctx, &name, value.as_deref())?
            }
            SecretCmd::List => commands::secret_list(&ctx)?,
            SecretCmd::Remove { name } => commands::secret_remove(&ctx, &name)?,
        },
        Command::Sessions { cmd } => match cmd {
            SessionsCmd::List { limit } => commands::sessions_list(&ctx, limit)?,
            SessionsCmd::Show { id } => commands::sessions_show(&ctx, &id)?,
            SessionsCmd::Delete { id } => commands::sessions_delete(&ctx, &id)?,
        },
    }
    Ok(())
}
