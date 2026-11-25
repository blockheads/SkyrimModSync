use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use skyrim_mod_sync::{
    config::{self, Settings},
    deploy::{self, Game},
    files,
    host::Host,
    launch,
    net::Net,
    peer, steam,
    store::Store,
    sync, Error, Result,
};
use std::{net::SocketAddr, path::PathBuf, process::ExitCode};

#[derive(Parser)]
#[command(version, about = "Play Skyrim with your friends' exact mod setup")]
struct Cli {
    /// Machine-readable output: one JSON result on stdout, progress on stderr; never prompts.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

/// A pack: `github:OWNER/REPO` (its newest release's pack.json), a pack URL or file, or a host's code.
const PACK: &str = "github:OWNER/REPO, a pack URL or file, or a host's code";

#[derive(Subcommand)]
enum Command {
    /// Find Skyrim, choose where synced mods live, and give your Nexus API key.
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
        /// Read the Nexus API key from one line of standard input.
        #[arg(long)]
        key_stdin: bool,
    },
    /// Sync, load and start Skyrim through Steam. Without a pack: your last one (the host: its own).
    Play {
        #[arg(help = PACK)]
        pack: Option<String>,
        /// Reach the host at this address directly (local tests).
        #[arg(long)]
        peer_address: Option<SocketAddr>,
    },
    /// Fetch a pack's files into the store without loading.
    Sync {
        #[arg(help = PACK)]
        pack: Option<String>,
        #[arg(long)]
        peer_address: Option<SocketAddr>,
    },
    /// Put the synced pack into the game folder.
    Load,
    /// Take the pack out of the game folder, restoring everything that was there.
    Unload,
    /// What is set up, synced and loaded.
    Status,
    /// Delete store files the current pack no longer uses.
    Clean,
    /// Make a pack from a mod list and share it.
    Host {
        #[command(subcommand)]
        command: HostCommand,
    },
}

#[derive(Subcommand)]
enum HostCommand {
    /// Resolve a mod list into a pack; prints your code (it never changes).
    Publish {
        list: PathBuf,
        /// Also write the pack to a file (e.g. to attach to a release).
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Send files to friends who have no Nexus key, or whose downloads fail (Ctrl-C to stop).
    Serve {
        /// Listen on this address without relays (local tests).
        #[arg(long)]
        bind: Option<SocketAddr>,
    },
    /// Add a Nexus mod to a list from the link of one of its files.
    Add { list: PathBuf, link: String },
}

fn game(s: &Settings) -> Result<Game> {
    Ok(Game {
        root: config::require(&s.game, "game folder")?,
        local: config::require(&s.local, "settings folder")?,
        documents: config::require(&s.documents, "documents folder")?,
    })
}
fn store(s: &Settings) -> Result<Store> {
    Store::open(&config::require(&s.store, "store folder")?)
}
fn net() -> Result<Net> {
    Net::new(config::nexus_key()?)
}

/// A loaded or half-loaded pack comes out first (the game must not be running).
fn clean_game(g: &Game) -> Result<()> {
    if g.state() != deploy::State::Clean {
        launch::refuse_if_running()?;
        deploy::unload(g)?;
    }
    Ok(())
}

const NO_KEY: &str = "Without a Premium Nexus key every mod comes from the host, who must be running `sms host serve` while you sync.";

fn setup(
    game: Option<PathBuf>,
    store: Option<PathBuf>,
    local: Option<PathBuf>,
    documents: Option<PathBuf>,
    key_stdin: bool,
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
                    "Skyrim not found in Steam. Its folder (with SkyrimSE.exe)?",
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
    let game = files::root(&game)?;
    files::require(&game, "SkyrimSE.exe")?;
    let store = match store.or(s.store.clone()) {
        Some(p) => p,
        None => config::ask_folder(
            "Folder for synced mods (same drive as Skyrim: hardlinks, no copies)?",
            interactive,
        )?,
    };
    std::fs::create_dir_all(&store)?;
    let store = files::root(&store)?;
    files::separate(&game, &store)?;
    let (found_local, found_docs) = match (&local, &documents) {
        (Some(l), Some(d)) => (l.clone(), d.clone()),
        _ => steam::user_folders(&game)?,
    };
    s.game = Some(game);
    s.store = Some(store);
    s.local = Some(local.unwrap_or(found_local));
    s.documents = Some(documents.unwrap_or(found_docs));
    s.save()?;

    let nexus = match config::nexus_key()? {
        Some(key) if !key_stdin => Net::new(Some(key))?
            .nexus_user()
            .map(|u| u.1)
            .unwrap_or(false),
        _ => match config::read_key(key_stdin, interactive)? {
            Some(key) => {
                let (_, premium) = Net::new(Some(key.clone()))?.nexus_user()?;
                config::save_key(&key)?;
                premium
            }
            None => false,
        },
    };
    let note = if nexus {
        "Nexus mods download from Nexus."
    } else {
        NO_KEY
    };
    Ok(json!({"state": "ready", "settings": s, "nexus_premium": nexus, "note": note}))
}

/// Syncs the given pack, or without one this machine's published pack (host) or the last one.
fn do_sync(
    source: Option<String>,
    direct: Option<SocketAddr>,
    g: &Game,
    st: &Store,
) -> Result<Value> {
    clean_game(g)?;
    let net = net()?;
    if source.is_none() {
        if let Some(own) = Host::open()?.published()? {
            return sync::sync(&own.pack, g, st, &net, None, Some(&own));
        }
    }
    let remembered = st.root.join("source.txt");
    let source = match source {
        Some(s) => s,
        None => std::fs::read_to_string(&remembered).map_err(|_| {
            Error::new(
                "pack_required",
                "Which pack? `sms play github:OWNER/REPO` (or a link, file or code).",
            )
        })?,
    };
    let pack = sync::fetch_pack(source.trim(), &net, direct)?;
    let result = sync::sync(&pack, g, st, &net, direct, None)?;
    files::write_atomic(&remembered, source.trim().as_bytes())?;
    if net.key.is_none() {
        eprintln!("{NO_KEY}");
    }
    Ok(result)
}

fn do_load(g: &Game, st: &Store) -> Result<Value> {
    clean_game(g)?;
    let pack = st
        .pack()?
        .ok_or_else(|| Error::new("not_synced", "Nothing synced yet: `sms play <pack>`."))?;
    let plan = deploy::plan(&pack, g, &mut st.hashes(), |e| {
        st.has(&e.hash, e.size).then(|| st.object(&e.hash))
    })?;
    deploy::load(g, &plan, &pack.name)
}

fn status() -> Result<Value> {
    let s = Settings::load()?;
    let mut out = json!({"settings": s, "nexus_key": config::nexus_key()?.is_some()});
    if let (Ok(g), Ok(st)) = (game(&s), store(&s)) {
        out["loaded"] = match g.state() {
            deploy::State::Clean => json!(false),
            deploy::State::Interrupted => json!("interrupted (the next command undoes it)"),
            deploy::State::Loaded(name) => json!(name),
        };
        if let Some(p) = st.pack()? {
            out["synced"] =
                json!({"pack": p.name, "files": p.files.len(), "plugins": p.plugins.len()});
        }
    }
    Ok(out)
}

fn run(cli: Cli) -> Result<Value> {
    let interactive = !cli.json;
    match cli.command {
        Command::Setup {
            game,
            store,
            local,
            documents,
            key_stdin,
        } => setup(game, store, local, documents, key_stdin, interactive),
        Command::Play { pack, peer_address } => {
            let s = Settings::load()?;
            let (g, st) = (game(&s)?, store(&s)?);
            launch::refuse_if_running()?;
            let synced = do_sync(pack, peer_address, &g, &st)?;
            let loaded = do_load(&g, &st)?;
            launch::start()?;
            Ok(
                json!({"state": "started", "synced": synced, "loaded": loaded, "note": "Run `sms unload` after playing."}),
            )
        }
        Command::Sync { pack, peer_address } => {
            let s = Settings::load()?;
            do_sync(pack, peer_address, &game(&s)?, &store(&s)?)
        }
        Command::Load => {
            let s = Settings::load()?;
            do_load(&game(&s)?, &store(&s)?)
        }
        Command::Unload => {
            let g = game(&Settings::load()?)?;
            launch::refuse_if_running()?;
            deploy::unload(&g)
        }
        Command::Status => status(),
        Command::Clean => {
            let freed = store(&Settings::load()?)?.clean()?;
            Ok(json!({"state": "cleaned", "freed_bytes": freed}))
        }
        Command::Host { command } => {
            let h = Host::open()?;
            match command {
                HostCommand::Publish { list, output } => {
                    let game = config::require(&Settings::load()?.game, "game folder")?;
                    h.publish(&list, &game, &net()?, output.as_deref())
                }
                HostCommand::Add { list, link } => h.add(&list, &link, &net()?),
                HostCommand::Serve { bind } => {
                    let published = h.published()?.ok_or_else(|| {
                        Error::new("not_published", "Publish first: `sms host publish <list>`.")
                    })?;
                    let json_mode = cli.json;
                    peer::serve(
                        h.key()?,
                        published,
                        bind,
                        |code| {
                            if json_mode {
                                println!("{}", json!({"state": "serving", "code": code}));
                            } else {
                                println!("Serving. Friends without a Nexus key run: sms play {code}\n(Ctrl-C to stop)");
                            }
                        },
                        || skyrim_mod_sync::CANCELLED.load(std::sync::atomic::Ordering::Relaxed),
                    )?;
                    Ok(json!({"state": "stopped"}))
                }
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
