use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use skyrim_mod_sync::{
    config::{self, Settings},
    deploy::{self, Game},
    host::{Binding, Host, Override},
    launch, nexus, peer, steam,
    store::Store,
    sync, Error, Result,
};
use std::{net::SocketAddr, path::PathBuf, process::ExitCode};

#[derive(Parser)]
#[command(
    version,
    about = "Share a Skyrim mod setup with friends: sync it, load it into the game, play, unload"
)]
struct Cli {
    /// Machine-readable output: one JSON result on stdout, progress lines on stderr; never prompts.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Find Skyrim and choose the store folder (where synced files live).
    Setup {
        #[arg(long)]
        game: Option<PathBuf>,
        #[arg(long)]
        store: Option<PathBuf>,
        /// The game's AppData/Local/Skyrim Special Edition (found automatically).
        #[arg(long)]
        local: Option<PathBuf>,
        /// The game's Documents/My Games/Skyrim Special Edition (found automatically).
        #[arg(long)]
        documents: Option<PathBuf>,
    },
    /// Save your Nexus API key (downloads need a Premium account's key).
    NexusLogin {
        /// Read the key from one line of standard input.
        #[arg(long)]
        stdin: bool,
    },
    /// Fetch a pack (a host's code or a pack file) into the store; on the host, without one, its own pack.
    Sync {
        pack: Option<String>,
        /// Reach the host at this address directly (local tests).
        #[arg(long)]
        peer_address: Option<SocketAddr>,
    },
    /// Put the synced pack into the game folder.
    Load,
    /// Take the pack out of the game folder, restoring everything that was there.
    Unload,
    /// What is synced and loaded.
    Status,
    /// Sync (a code or pack file; on the host, its own pack), load, and start Skyrim through Steam.
    Play {
        pack: Option<String>,
        #[arg(long)]
        peer_address: Option<SocketAddr>,
    },
    /// Publishing your MO2 profile as a pack.
    Host {
        #[command(subcommand)]
        command: HostCommand,
    },
}

#[derive(Subcommand)]
enum HostCommand {
    /// Choose the MO2 instance and profile to publish.
    Init {
        #[arg(long)]
        mo2: PathBuf,
        #[arg(long)]
        profile: String,
        /// The pack's name friends see (default: the profile's).
        #[arg(long)]
        name: Option<String>,
        /// The game folder, when MO2's own setting doesn't point at it.
        #[arg(long)]
        game: Option<PathBuf>,
    },
    /// Every mod, where friends will get its files, and how much the host sends.
    Status,
    /// Where a mod's files come from: `nexus` (its MO2 record), `host`, `url URL`, or
    /// `github OWNER/REPO [ASSET]` (the newest release's archive, picked at each publish).
    Source {
        r#mod: String,
        kind: String,
        url: Option<String>,
        /// For `github`: a part of the asset's name, when the release has several archives.
        asset: Option<String>,
    },
    /// Leave a mod out of the pack.
    Exclude { r#mod: String },
    /// Put an excluded mod back.
    Include { r#mod: String },
    /// Sign the current profile as the next pack; prints the code for friends.
    Publish {
        /// Also write a pack file (for packs friends can sync without you online).
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Send your files to friends until stopped (Ctrl-C).
    Serve {
        #[arg(long)]
        no_relay: bool,
        #[arg(long)]
        bind: Option<SocketAddr>,
    },
}

fn game(settings: &Settings) -> Result<Game> {
    Ok(Game {
        root: Settings::require(&settings.game, "game folder")?,
        local: Settings::require(&settings.local, "settings folder")?,
        documents: Settings::require(&settings.documents, "documents folder")?,
    })
}

fn store(settings: &Settings) -> Result<Store> {
    Store::open(&Settings::require(&settings.store, "store folder")?)
}

fn setup(
    game: Option<PathBuf>,
    store: Option<PathBuf>,
    local: Option<PathBuf>,
    documents: Option<PathBuf>,
    interactive: bool,
) -> Result<Value> {
    let mut s = Settings::load()?;
    let game = match game.or(s.game.clone()) {
        Some(g) => g,
        None => {
            let found = steam::installs(&steam::steam_roots());
            match found.len() {
                1 => found[0].game.clone(),
                0 => config::ask_folder(
                    "Skyrim Special Edition not found in Steam. Its folder (with SkyrimSE.exe)?",
                    interactive,
                )?,
                _ => {
                    return Err(Error::new(
                        "game_ambiguous",
                        format!(
                            "Several Skyrim installs: {}. Pick one with --game.",
                            steam::describe(&found)
                        ),
                    ))
                }
            }
        }
    };
    let game = skyrim_mod_sync::files::root(&game)?;
    skyrim_mod_sync::files::require(&game, "SkyrimSE.exe")?;
    let store = match store.or(s.store.clone()) {
        Some(p) => p,
        None => config::ask_folder(
            "Store folder for synced mods (on the same drive as Skyrim, files are hardlinked instead of copied)?",
            interactive,
        )?,
    };
    std::fs::create_dir_all(&store)?;
    let store = skyrim_mod_sync::files::root(&store)?;
    config::separate(&game, &store)?;
    let (found_local, found_docs) = match (&local, &documents) {
        (Some(l), Some(d)) => (l.clone(), d.clone()),
        _ => steam::user_folders(&game)?,
    };
    s.game = Some(game.clone());
    s.store = Some(store.clone());
    s.local = Some(local.unwrap_or(found_local));
    s.documents = Some(documents.unwrap_or(found_docs));
    s.save()?;
    let same_drive = same_device(&game, &store);
    Ok(
        json!({"state": "ready", "game": game, "store": store, "local": s.local, "documents": s.documents, "hardlinks": same_drive}),
    )
}

fn same_device(a: &std::path::Path, b: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(x), Ok(y)) = (std::fs::metadata(a), std::fs::metadata(b)) {
            return x.dev() == y.dev();
        }
        false
    }
    #[cfg(not(unix))]
    {
        a.components().next() == b.components().next()
    }
}

fn status() -> Result<Value> {
    let s = Settings::load()?;
    let mut out =
        json!({"game": s.game, "store": s.store, "nexus_key": config::nexus_key()?.is_some()});
    if let (Ok(g), Ok(st)) = (game(&s), store(&s)) {
        out["loaded"] = match g.state()? {
            deploy::State::Clean => json!(false),
            deploy::State::Interrupted => json!("interrupted (the next command undoes it)"),
            deploy::State::Loaded { name, sequence, .. } => json!(format!("{name} #{sequence}")),
        };
        if let Some(c) = st.current()? {
            out["synced"] = json!(format!("{} #{}", c.pack.name, c.head.sequence));
            out["files"] = json!(c.pack.files.len());
            out["plugins"] = json!(c.pack.plugins.len());
        }
    }
    Ok(out)
}

/// Syncs `code`, or with none the host's own latest pack from its own files (None if this machine
/// publishes nothing).
fn do_sync(code: Option<&str>, peer_address: Option<SocketAddr>) -> Result<Option<Value>> {
    let s = Settings::load()?;
    let (g, st) = (game(&s)?, store(&s)?);
    if g.state()? == deploy::State::Interrupted {
        launch::refuse_if_running()?;
        deploy::recover(&g)?;
    }
    let (pack, head, local) = match code {
        Some(code) => {
            let (pack, head) = sync::resolve(code, &st, peer_address)?;
            (pack, head, None)
        }
        None => match Host::open()?.own_pack()? {
            Some((pack, head, local)) => {
                st.accept(&head)?;
                (pack, head, Some(local))
            }
            None => return Ok(None),
        },
    };
    launch::check_version(&g.root, &pack.runtime)?;
    sync::sync(&pack, &head, &st, &g, peer_address, local.as_ref()).map(Some)
}

fn load() -> Result<Value> {
    let s = Settings::load()?;
    let (g, st) = (game(&s)?, store(&s)?);
    launch::refuse_if_running()?;
    let c = st
        .current()?
        .ok_or_else(|| Error::new("not_synced", "Nothing synced yet: `sms sync <code>`."))?;
    launch::check_version(&g.root, &c.pack.runtime)?;
    deploy::load(&g, &st, &c.pack, c.head.sequence)
}

fn run(cli: Cli) -> Result<Value> {
    let interactive = !cli.json;
    match cli.command {
        Command::Setup {
            game,
            store,
            local,
            documents,
        } => setup(game, store, local, documents, interactive),
        Command::NexusLogin { stdin } => {
            let key = config::read_key(stdin, interactive)?;
            let user = nexus::validate(&key)?;
            config::save_key(&key)?;
            Ok(
                json!({"state": "saved", "account": user.name, "premium": user.is_premium,
                      "note": if user.is_premium { "Nexus downloads are automatic." } else { "Not Premium: Nexus files must come from the host instead." }}),
            )
        }
        Command::Sync { pack, peer_address } => {
            do_sync(pack.as_deref(), peer_address)?.ok_or_else(|| {
                Error::new(
                    "pack_required",
                    "Give the host's code (or a pack file); as the host, publish first.",
                )
            })
        }
        Command::Load => load(),
        Command::Unload => {
            let g = game(&Settings::load()?)?;
            launch::refuse_if_running()?;
            deploy::unload(&g)
        }
        Command::Status => status(),
        Command::Play { pack, peer_address } => {
            // Without a code: the host's own pack, else the pack synced last.
            let synced = do_sync(pack.as_deref(), peer_address)?;
            let loaded = load()?;
            launch::start()?;
            Ok(
                json!({"state": "started", "synced": synced, "loaded": loaded,
                      "note": "Run `sms unload` after playing to restore your game."}),
            )
        }
        Command::Host { command } => {
            let h = Host::open()?;
            match command {
                HostCommand::Init {
                    mo2,
                    profile,
                    name,
                    game,
                } => h.init(Binding {
                    name: name.unwrap_or_else(|| profile.clone()),
                    mo2,
                    profile,
                    game,
                    overrides: Default::default(),
                }),
                HostCommand::Status => h.status(),
                HostCommand::Source {
                    r#mod,
                    kind,
                    url,
                    asset,
                } => match (kind.as_str(), url) {
                    ("nexus", None) => h.set_override(&r#mod, None),
                    ("host", None) => h.set_override(&r#mod, Some(Override::Host)),
                    ("url", Some(url)) => h.set_override(&r#mod, Some(Override::Url { url })),
                    ("github", Some(repo)) => {
                        h.set_override(&r#mod, Some(Override::Github { repo, asset }))
                    }
                    _ => Err(Error::new(
                        "invalid_source",
                        "Use `nexus`, `host`, `url URL` or `github OWNER/REPO [ASSET]`.",
                    )),
                },
                HostCommand::Exclude { r#mod } => h.set_override(&r#mod, Some(Override::Exclude)),
                HostCommand::Include { r#mod } => h.set_override(&r#mod, None),
                HostCommand::Publish { output } => h.publish(output.as_deref()),
                HostCommand::Serve { no_relay, bind } => peer::serve(&h.dir, no_relay, bind),
            }
        }
    }
}

/// Plain-text output: one `key: value` line per field, lists indented.
fn human(v: &Value, indent: usize, out: &mut String) {
    let pad = " ".repeat(indent);
    match v {
        Value::Object(m) => {
            for (k, v) in m {
                match v {
                    Value::Object(_) | Value::Array(_) => {
                        out.push_str(&format!("{pad}{k}:\n"));
                        human(v, indent + 2, out);
                    }
                    Value::Null => {}
                    Value::String(s) => out.push_str(&format!("{pad}{k}: {s}\n")),
                    _ => out.push_str(&format!("{pad}{k}: {v}\n")),
                }
            }
        }
        Value::Array(a) => {
            for item in a {
                match item {
                    Value::Object(m) => {
                        let line = m
                            .iter()
                            .filter(|(_, v)| !v.is_null())
                            .map(|(k, v)| match v {
                                Value::String(s) => format!("{k}={s}"),
                                _ => format!("{k}={v}"),
                            })
                            .collect::<Vec<_>>()
                            .join("  ");
                        out.push_str(&format!("{pad}- {line}\n"));
                    }
                    Value::String(s) => out.push_str(&format!("{pad}- {s}\n")),
                    _ => out.push_str(&format!("{pad}- {item}\n")),
                }
            }
        }
        Value::String(s) => out.push_str(&format!("{pad}{s}\n")),
        _ => out.push_str(&format!("{pad}{v}\n")),
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let json_mode = cli.json;
    let _ = ctrlc::set_handler(|| {
        skyrim_mod_sync::CANCELLED.store(true, std::sync::atomic::Ordering::Relaxed)
    });
    match run(cli) {
        Ok(v) => {
            if json_mode {
                println!("{v}");
            } else {
                let mut out = String::new();
                human(&v, 0, &mut out);
                print!("{out}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            if json_mode {
                println!(
                    "{}",
                    json!({"state": "error", "code": e.code, "message": e.message})
                );
            } else {
                eprintln!("error: {}", e.message);
            }
            ExitCode::from(if e.code == "cancelled" { 130 } else { 2 })
        }
    }
}
