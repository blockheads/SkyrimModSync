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
use std::{
    io::{self, IsTerminal},
    net::SocketAddr,
    path::{Path, PathBuf},
    process::ExitCode,
};

#[derive(Parser)]
#[command(
    version,
    about = "Play Skyrim with your friends' exact mod setup. Run without a command: set up, paste a pack code, play."
)]
struct Cli {
    /// Machine-readable output: one JSON result on stdout, progress on stderr; never prompts.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

const PACK: &str =
    "A pack code (gist id or link), github:OWNER/REPO, a pack URL or file, or a host's code";

#[derive(Subcommand)]
enum Command {
    /// Choose the game, the folder for mods and your Nexus API key (the first run asks by itself).
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
    /// Sync a pack and put it in the game; then start Skyrim from Steam. Without a code: the last one.
    Enable {
        #[arg(help = PACK)]
        code: Option<String>,
        /// Reach the host at this address directly (local tests).
        #[arg(long, hide = true)]
        peer_address: Option<SocketAddr>,
    },
    /// Take the pack out of the game: it is back as it was.
    Disable,
    /// What is set up, synced and enabled.
    Status,
    /// Delete synced files the current pack no longer uses.
    Clean,
    /// Share a mod list: publish it, upload the pack to your secret gist, enable it here, and send files
    /// to friends until Ctrl-C.
    #[command(args_conflicts_with_subcommands = true)]
    Host {
        /// The mod list (default: modlist.json in this folder).
        list: Option<PathBuf>,
        /// Also write the pack to a file.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Don't upload the pack to a gist.
        #[arg(long)]
        no_upload: bool,
        /// Listen on this address without relays (local tests).
        #[arg(long, hide = true)]
        bind: Option<SocketAddr>,
        #[command(subcommand)]
        command: Option<HostCommand>,
    },
}

#[derive(Subcommand)]
enum HostCommand {
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

/// An enabled or half-enabled pack comes out first (the game must not be running).
fn clean_game(g: &Game) -> Result<()> {
    if g.state() != deploy::State::Clean {
        launch::refuse_if_running()?;
        deploy::unload(g)?;
    }
    Ok(())
}

const NO_KEY: &str = "Without a Premium Nexus key every mod comes from the host, who must have `sms host` running while you sync.";

/// The folder this program sits in, when it is a Skyrim folder.
fn beside_game() -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    dir.join("SkyrimSE.exe").is_file().then_some(dir)
}

fn setup(
    game: Option<PathBuf>,
    store: Option<PathBuf>,
    local: Option<PathBuf>,
    documents: Option<PathBuf>,
    key_stdin: bool,
    interactive: bool,
) -> Result<Value> {
    let mut s = Settings::load()?;
    let game = match game.or_else(beside_game).or(s.game.clone()) {
        Some(g) => g,
        None => {
            let found = steam::installs(&steam::steam_roots());
            match found.len() {
                1 => found[0].game.clone(),
                0 => config::ask(
                    "Skyrim not found in Steam. Its folder (with SkyrimSE.exe)?",
                    None,
                    interactive,
                )?
                .into(),
                _ => {
                    return Err(Error::new(
                        "game_ambiguous",
                        format!(
                        "Several Skyrim installs: {}. Put sms in the one to use, or pass --game.",
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
        None => {
            let near = steam::library_of(&game)
                .or_else(|| game.parent().map(Path::to_path_buf))
                .unwrap_or_else(|| game.clone())
                .join("SkyrimModSync");
            config::ask(
                "Folder for mods (on the same drive as Skyrim)?",
                Some(&near.to_string_lossy()),
                interactive,
            )?
            .into()
        }
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
    let mut note = if nexus {
        "Nexus mods download from Nexus.".to_owned()
    } else {
        NO_KEY.to_owned()
    };
    let (g, st) = (
        s.game.clone().unwrap_or_default(),
        s.store.clone().unwrap_or_default(),
    );
    if !files::same_drive(&g, &st) {
        let warning = format!(
            "The mods folder {} is on another drive than Skyrim: mods will be copied (twice the space, slower). Pick one on Skyrim's drive with `sms setup --store <folder>`.",
            st.display()
        );
        eprintln!("warning: {warning}");
        note = format!("{note}\n{warning}");
    }
    Ok(json!({"state": "ready", "settings": s, "nexus_premium": nexus, "note": note}))
}

/// The settings, asking for what is missing first (the first run).
fn ready(interactive: bool) -> Result<Settings> {
    let s = Settings::load()?;
    let moved = beside_game()
        .and_then(|b| files::root(&b).ok())
        .is_some_and(|b| Some(b) != s.game);
    if s.game.is_none() || s.store.is_none() || moved {
        let r = setup(None, None, None, None, false, interactive)?;
        if r["nexus_premium"] != true {
            eprintln!("{NO_KEY}");
        }
        return Settings::load();
    }
    Ok(s)
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
    let source = source.filter(|s| !s.trim().is_empty());
    let remembered = st.root.join("source.txt");
    let source = match source.or_else(|| std::fs::read_to_string(&remembered).ok()) {
        Some(s) => sync::source(&s),
        None => {
            if let Some(own) = Host::open()?.published()? {
                return sync::sync(&own.pack, g, st, &net, None, Some(&own));
            }
            return Err(Error::new(
                "pack_required",
                "Which pack? `sms enable <code>` with the code your host gave you.",
            ));
        }
    };
    let pack = sync::fetch_pack(&source, &net, direct)?;
    let result = sync::sync(&pack, g, st, &net, direct, None)?;
    files::write_atomic(&remembered, source.as_bytes())?;
    Ok(result)
}

fn do_load(g: &Game, st: &Store) -> Result<Value> {
    clean_game(g)?;
    let pack = st
        .pack()?
        .ok_or_else(|| Error::new("not_synced", "Nothing synced yet: `sms enable <code>`."))?;
    let plan = deploy::plan(&pack, g, &mut st.hashes(), |e| {
        st.has(&e.hash, e.size).then(|| st.object(&e.hash))
    })?;
    deploy::load(g, &plan, &pack.name)
}

const PLAY: &str = "Start Skyrim from Steam. `sms disable` puts your game back as it was.";

fn enable(code: Option<String>, direct: Option<SocketAddr>, s: &Settings) -> Result<Value> {
    let (g, st) = (game(s)?, store(s)?);
    launch::refuse_if_running()?;
    let synced = do_sync(code, direct, &g, &st)?;
    let loaded = do_load(&g, &st)?;
    Ok(json!({
        "state": "enabled",
        "pack": loaded["pack"],
        "downloaded_archives": synced["archives"],
        "files_from_host": synced["host_files"],
        "note": PLAY
    }))
}

/// No command: set up if needed, ask for the pack code, enable it.
fn guided(interactive: bool) -> Result<Value> {
    if !interactive {
        return Err(Error::new(
            "command_required",
            "Name a command (see --help).",
        ));
    }
    let s = ready(interactive)?;
    let last = std::fs::read_to_string(store(&s)?.root.join("source.txt")).ok();
    let code = config::ask("Pack code (paste it here)?", last.as_deref(), interactive)?;
    enable(Some(code), None, &s)
}

/// The mod list `sms host` reads by default.
fn default_list() -> Result<PathBuf> {
    let here = PathBuf::from("modlist.json");
    let beside = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("modlist.json")));
    std::iter::once(here)
        .chain(beside)
        .find(|p| p.is_file())
        .ok_or_else(|| {
            Error::new(
                "list_required",
                "No modlist.json here: name your mod list, or start one with `sms host add modlist.json <nexus file link>`.",
            )
        })
}

/// Saves the pack to this host's gist (a new one the first time); returns the gist id.
fn upload(h: &Host, net: &Net, interactive: bool) -> Result<String> {
    let pack = h
        .published()?
        .ok_or_else(|| Error::new("not_published", "Nothing published."))?
        .pack;
    let files = [("pack.json", serde_json::to_string_pretty(&pack)?)];
    let description = format!("{} (SkyrimModSync pack)", pack.name);
    let token = config::github_token(interactive)?;
    let saved = match net.gist_save(&token, h.gist().as_deref(), &description, &files) {
        Err(e) if e.code == "gist_missing" => net.gist_save(&token, None, &description, &files),
        Err(e) if e.code == "github_token" => {
            config::forget_github_token()?;
            Err(e)
        }
        other => other,
    }?;
    h.set_gist(&saved)?;
    Ok(saved)
}

fn host(
    list: Option<PathBuf>,
    output: Option<PathBuf>,
    no_upload: bool,
    bind: Option<SocketAddr>,
    json_mode: bool,
) -> Result<Value> {
    let interactive = !json_mode;
    let list = match list {
        Some(l) => l,
        None => default_list()?,
    };
    let s = ready(interactive)?;
    let (g, st) = (game(&s)?, store(&s)?);
    launch::refuse_if_running()?;
    clean_game(&g)?;
    let h = Host::open()?;
    let net = net()?;
    let published = h.publish(&list, &g.root, &net, output.as_deref())?;
    let code = if no_upload {
        None
    } else {
        Some(upload(&h, &net, interactive)?)
    };
    let own = h
        .published()?
        .ok_or_else(|| Error::new("not_published", "Nothing published."))?;
    sync::sync(&own.pack, &g, &st, &net, None, Some(&own))?;
    do_load(&g, &st)?;
    let _ = std::fs::remove_file(st.root.join("source.txt"));
    peer::serve(
        h.key()?,
        own,
        bind,
        |peer_code| {
            if json_mode {
                println!(
                    "{}",
                    json!({"state": "serving", "code": code, "host_code": peer_code, "published": published})
                );
                return;
            }
            match &code {
                Some(c) => println!("\nPack code: {c}\nFriends: put sms in their Skyrim folder, run it and paste the code."),
                None => println!("\nPack code (no gist): {peer_code}"),
            }
            println!("Your game has the pack too: start Skyrim from Steam.");
            println!("Leave this open while friends sync: it sends them files Nexus can't (Ctrl-C to stop).");
        },
        || skyrim_mod_sync::CANCELLED.load(std::sync::atomic::Ordering::Relaxed),
    )?;
    Ok(
        json!({"state": "stopped", "note": "The pack stays enabled here; `sms disable` takes it out."}),
    )
}

fn status() -> Result<Value> {
    let s = Settings::load()?;
    let mut out = json!({"settings": s, "nexus_key": config::nexus_key()?.is_some()});
    if let (Ok(g), Ok(st)) = (game(&s), store(&s)) {
        out["enabled"] = match g.state() {
            deploy::State::Clean => json!(false),
            deploy::State::Interrupted => json!("interrupted (the next command undoes it)"),
            deploy::State::Loaded(name) => json!(name),
        };
        if let Some(p) = st.pack()? {
            out["synced"] =
                json!({"pack": p.name, "files": p.files.len(), "plugins": p.plugins.len()});
        }
    }
    if let Some(g) = Host::open()?.gist() {
        out["your_pack_code"] = json!(g);
    }
    Ok(out)
}

fn run(command: Option<Command>, json_mode: bool) -> Result<Value> {
    let interactive = !json_mode;
    let Some(command) = command else {
        return guided(interactive);
    };
    match command {
        Command::Setup {
            game,
            store,
            local,
            documents,
            key_stdin,
        } => setup(game, store, local, documents, key_stdin, interactive),
        Command::Enable { code, peer_address } => enable(code, peer_address, &ready(interactive)?),
        Command::Disable => {
            let g = game(&Settings::load()?)?;
            launch::refuse_if_running()?;
            let r = deploy::unload(&g)?;
            Ok(
                json!({"state": "disabled", "restored": r["restored"], "note": "Your game is back as it was. `sms enable` turns the pack on again."}),
            )
        }
        Command::Status => status(),
        Command::Clean => {
            let freed = store(&Settings::load()?)?.clean()?;
            Ok(json!({"state": "cleaned", "freed_bytes": freed}))
        }
        Command::Host {
            command: Some(HostCommand::Add { list, link }),
            ..
        } => Host::open()?.add(&list, &link, &net()?),
        Command::Host {
            list,
            output,
            no_upload,
            bind,
            command: None,
        } => host(list, output, no_upload, bind, json_mode),
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

/// Started by a double-click on Windows: keep the window open to read the result.
fn pause() {
    if io::stdin().is_terminal() {
        eprintln!("\nPress Enter to close.");
        let _ = io::stdin().read_line(&mut String::new());
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let json_mode = cli.json;
    let hold = cfg!(windows) && cli.command.is_none() && !json_mode;
    skyrim_mod_sync::JSON.store(json_mode, std::sync::atomic::Ordering::Relaxed);
    let _ = ctrlc::set_handler(|| {
        skyrim_mod_sync::CANCELLED.store(true, std::sync::atomic::Ordering::Relaxed)
    });
    let code = match run(cli.command, json_mode) {
        Ok(v) => {
            if json_mode {
                println!("{v}");
            } else {
                let mut v = v;
                let note = v.as_object_mut().and_then(|m| m.remove("note"));
                let mut out = String::new();
                human(&v, 0, &mut out);
                if let Some(Value::String(n)) = note {
                    out.push_str(&format!("\n{n}\n"));
                }
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
    };
    if hold {
        pause();
    }
    code
}
