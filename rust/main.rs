use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use skyrim_mod_sync::{
    config::{self, Settings},
    deploy::{self, Game},
    files,
    host::{Binding, Host, Source},
    launch,
    net::Net,
    pack::Pack,
    peer, steam,
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
    /// Machine-readable output: one JSON result on stdout, progress on stderr; never prompts.
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
    /// Save a Nexus API key (optional: Premium makes Nexus mods download from Nexus, not the host).
    NexusLogin {
        #[arg(long)]
        stdin: bool,
    },
    /// Fetch the host's pack into the store (the code is only needed the first time).
    Sync {
        code: Option<String>,
        /// Reach the host at this address directly (local tests).
        #[arg(long)]
        peer_address: Option<SocketAddr>,
    },
    /// Put the pack into the game folder (the host: its own published pack).
    Load,
    /// Take the pack out of the game folder, restoring everything that was there.
    Unload,
    /// What is synced and loaded.
    Status,
    /// Sync, load and start Skyrim through Steam (the host: its own pack, nothing to sync).
    Play {
        code: Option<String>,
        #[arg(long)]
        peer_address: Option<SocketAddr>,
    },
    /// Delete store files the current pack no longer uses.
    Clean,
    /// Publishing your MO2 profile.
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
    /// Every mod, where friends get it, and what you would send.
    Status,
    /// Where a mod's files come from: `nexus` (MO2's record), `host`, `url URL`, `github OWNER/REPO [ASSET]`.
    Source {
        r#mod: String,
        kind: String,
        value: Option<String>,
        asset: Option<String>,
    },
    /// Publish the current profile; prints the code for friends (it stays the same).
    Publish,
    /// Send your files to friends until stopped (Ctrl-C).
    Serve {
        /// Listen on this address without relays (local tests).
        #[arg(long)]
        bind: Option<SocketAddr>,
    },
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
            "Store folder for synced mods (same drive as Skyrim: hardlinks, no copies)?",
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
    Ok(json!({"state": "ready", "settings": s}))
}

/// The pack a command works on: the code's, else this machine's published one (host), else the synced one.
enum Which {
    Code(String),
    Own,
    Synced(String),
}
fn which(code: Option<String>, st: &Store) -> Result<Which> {
    if let Some(c) = code {
        return Ok(Which::Code(c));
    }
    if Host::open()?.published()?.is_some() {
        return Ok(Which::Own);
    }
    match st.pack()? {
        Some(p) => Ok(Which::Synced(p.host)),
        None => Err(Error::new(
            "code_required",
            "Give the host's code: `sms play <code>`.",
        )),
    }
}

/// Syncs a friend's pack; returns it.
fn do_sync(code: &str, direct: Option<SocketAddr>, g: &Game, st: &Store) -> Result<(Pack, Value)> {
    clean_game(g)?;
    let pack = peer::fetch_pack(code, direct)?;
    let result = sync::sync(&pack, g, st, &net()?, direct)?;
    Ok((pack, result))
}

/// Loads the synced pack, or the host's own from its own files.
fn do_load(own: bool, g: &Game, st: &Store) -> Result<Value> {
    clean_game(g)?;
    let mut cache = st.hashes();
    if own {
        let published = Host::open()?
            .published()?
            .ok_or_else(|| Error::new("not_published", "Publish first."))?;
        let plan = deploy::plan(&published.pack, g, &mut cache, |e| {
            published.local.get(&e.hash).cloned()
        })?;
        return deploy::load(g, &plan, &published.pack.name);
    }
    let pack = st
        .pack()?
        .ok_or_else(|| Error::new("not_synced", "Nothing synced yet: `sms sync <code>`."))?;
    let plan = deploy::plan(&pack, g, &mut cache, |e| {
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
            out["synced"] = json!({"pack": p.name, "files": p.files.len(), "plugins": p.plugins.len(), "code": p.host});
            if g.state() == deploy::State::Clean {
                let plan = deploy::plan(&p, &g, &mut st.hashes(), |e| {
                    st.has(&e.hash, e.size).then(|| st.object(&e.hash))
                })?;
                out["missing"] = json!(plan.missing.len());
            }
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
        } => setup(game, store, local, documents, interactive),
        Command::NexusLogin { stdin } => {
            let key = config::read_key(stdin, interactive)?;
            let (name, premium) = Net::new(Some(key.clone()))?.nexus_user()?;
            config::save_key(&key)?;
            Ok(
                json!({"state": "saved", "account": name, "premium": premium,
                      "note": if premium { "Nexus mods download from Nexus." } else { "Not Premium: Nexus mods come from the host." }}),
            )
        }
        Command::Sync { code, peer_address } => {
            let s = Settings::load()?;
            let (g, st) = (game(&s)?, store(&s)?);
            match which(code, &st)? {
                Which::Code(c) | Which::Synced(c) => Ok(do_sync(&c, peer_address, &g, &st)?.1),
                Which::Own => {
                    Ok(json!({"state": "synced", "note": "The host plays from its own files."}))
                }
            }
        }
        Command::Load => {
            let s = Settings::load()?;
            let (g, st) = (game(&s)?, store(&s)?);
            let own = matches!(which(None, &st)?, Which::Own);
            do_load(own, &g, &st)
        }
        Command::Unload => {
            let g = game(&Settings::load()?)?;
            launch::refuse_if_running()?;
            deploy::unload(&g)
        }
        Command::Status => status(),
        Command::Play { code, peer_address } => {
            let s = Settings::load()?;
            let (g, st) = (game(&s)?, store(&s)?);
            launch::refuse_if_running()?;
            let (own, synced) = match which(code, &st)? {
                Which::Own => (true, Value::Null),
                Which::Code(c) | Which::Synced(c) => (false, do_sync(&c, peer_address, &g, &st)?.1),
            };
            let loaded = do_load(own, &g, &st)?;
            launch::start()?;
            Ok(
                json!({"state": "started", "synced": synced, "loaded": loaded, "note": "Run `sms unload` after playing."}),
            )
        }
        Command::Clean => {
            let freed = store(&Settings::load()?)?.clean()?;
            Ok(json!({"state": "cleaned", "freed_bytes": freed}))
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
                    sources: Default::default(),
                }),
                HostCommand::Status => h.status(&net()?),
                HostCommand::Source {
                    r#mod,
                    kind,
                    value,
                    asset,
                } => {
                    let source = match (kind.as_str(), value) {
                        ("nexus", None) => None,
                        ("host", None) => Some(Source::Host),
                        ("url", Some(url)) if url.starts_with("https://") => {
                            Some(Source::Url { url })
                        }
                        ("github", Some(repo)) => Some(Source::Github { repo, asset }),
                        _ => return Err(Error::new(
                            "invalid_source",
                            "Use `nexus`, `host`, `url https://…` or `github OWNER/REPO [ASSET]`.",
                        )),
                    };
                    h.set_source(&r#mod, source)
                }
                HostCommand::Publish => h.publish(&net()?),
                HostCommand::Serve { bind } => {
                    let published = h.published()?.ok_or_else(|| {
                        Error::new("not_published", "Publish first: `sms host publish`.")
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
                                println!("Serving. Friends run: sms play {code}\n(Ctrl-C to stop)");
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
