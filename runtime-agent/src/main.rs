use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use noland_runtime::{artifact, engine, session::clean_command, store::Store, PROFILE};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    version,
    about = "Run trusted Linux apps in the cloud PC's provisioned runtime (not a security sandbox)"
)]
struct Cli {
    #[command(subcommand)]
    command: Action,
}

#[derive(Subcommand)]
enum Action {
    Gui,
    Status {
        #[arg(long)]
        json: bool,
    },
    Doctor {
        #[arg(long)]
        json: bool,
    },
    /// Update Ubuntu libraries in a new isolated image; never upgrade the host.
    Update {
        #[arg(long)]
        approve: bool,
    },
    InspectApp {
        #[arg(long)]
        app_id: String,
    },
    InstallPrerequisites {
        #[arg(long)]
        app_id: String,
        #[arg(long)]
        approve: bool,
        #[arg(long)]
        expected_plan: Option<String>,
    },
    Plan {
        #[arg(long, default_value = PROFILE)]
        profile: String,
    },
    /// Check the runtime provisioned on this cloud PC; no separate release/signing setup.
    Ensure {
        #[arg(long, default_value = PROFILE)]
        profile: String,
        #[arg(long)]
        approve: bool,
        // Accepted for compatibility with previous preview clients.
        #[arg(long)]
        expected_sha256: Option<String>,
        #[arg(long)]
        background: bool,
    },
    Cancel,
    Register {
        #[arg(long)]
        file: PathBuf,
        /// Grant the owner's executable bit, only after validating a selected ELF file.
        #[arg(long)]
        make_executable: bool,
    },
    Launch {
        #[arg(long)]
        app_id: String,
    },
    CreateShortcut {
        #[arg(long)]
        app_id: Option<String>,
    },
    Rollback {
        #[arg(long, default_value = PROFILE)]
        profile: String,
    },
    #[command(hide = true)]
    Supervise {
        #[arg(long)]
        app_id: String,
    },
}

fn print_json(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let store = Store::open()?;
    match cli.command {
        Action::Gui => gui(&store),
        Action::Status { .. } => print_json(&engine::status(&store)?),
        Action::Doctor { .. } => print_json(&engine::doctor(&store)?),
        Action::Update { approve } => {
            print_json(&noland_runtime::maintenance::update(&store, approve)?)
        }
        Action::InspectApp { app_id } => {
            print_json(&noland_runtime::maintenance::inspect(&store, &app_id)?)
        }
        Action::InstallPrerequisites {
            app_id,
            approve,
            expected_plan,
        } => print_json(&noland_runtime::maintenance::install_prerequisites(
            &store,
            &app_id,
            approve,
            expected_plan.as_deref(),
        )?),
        Action::Plan { profile } => {
            anyhow::ensure!(profile == PROFILE, "Unsupported runtime profile");
            print_json(&serde_json::json!({
                "source": "cloud-pc-provisioning", "requiredBytes": 0,
                "status": engine::status(&store)?,
                "message": "The runtime is downloaded during Provisioning. No separate signed release is needed."
            }))
        }
        Action::Ensure {
            profile, approve, ..
        } => {
            anyhow::ensure!(profile == PROFILE, "Unsupported runtime profile");
            anyhow::ensure!(approve, "Use --approve to check the provisioned runtime");
            let root = store
                .root()
                .context("Resume Provisioning in Noland Connect to install the cloud runtime")?;
            artifact::validate_root(&root)?;
            print_json(&engine::status(&store)?)
        }
        Action::Cancel => {
            std::fs::write(store.state.join("cancel"), b"cancel")?;
            print_json(&serde_json::json!({"cancelRequested": true}))
        }
        Action::Register {
            file,
            make_executable,
        } => print_json(&engine::register_selected(&store, &file, make_executable)?),
        Action::Launch { app_id } => print_json(&engine::launch(&store, &app_id)?),
        Action::CreateShortcut { app_id } => {
            print_json(&engine::create_shortcut(&store, app_id.as_deref())?)
        }
        Action::Rollback { profile } => {
            anyhow::ensure!(profile == PROFILE, "Unsupported runtime profile");
            artifact::rollback(&store)?;
            print_json(&engine::status(&store)?)
        }
        Action::Supervise { app_id } => engine::supervise(&store, &app_id),
    }
}

fn gui(store: &Store) -> Result<()> {
    // Keep this guard alive for the lifetime of the parent-side GTK window.
    let session = noland_runtime::session::Session::discover(store)?;
    session.refresh_authority()?;
    let status = clean_command("/usr/bin/python3")
        .env("HOME", std::env::var("HOME")?)
        .env("DISPLAY", noland_runtime::DISPLAY)
        .env("XAUTHORITY", &session.authority)
        .args(["-c", include_str!("gui.py")])
        .arg(std::env::current_exe()?)
        .status()
        .context(
            "The launcher needs python3-gi and gir1.2-gtk-3.0; resume Provisioning to repair it",
        )?;
    anyhow::ensure!(status.success(), "Runtime launcher exited unsuccessfully");
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{}", serde_json::json!({"error":format!("{error:#}")}));
        std::process::exit(1);
    }
}
