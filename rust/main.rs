use clap::{Args, Parser, Subcommand};
use serde_json::{json, Value};
use skyrim_mod_sync::{
    acquisition, authoring, config, files, package, peer, play,
    recipe::Recipe,
    revision, snapshot,
    target::{self, Storage},
    Error, Result,
};
use std::{
    collections::BTreeMap,
    fs,
    io::IsTerminal,
    path::{Path, PathBuf},
    process::ExitCode,
};
#[derive(Parser)]
#[command(
    version,
    about = "Capture, compare and reproduce deployed Skyrim instances"
)]
struct Cli {
    #[arg(
        long,
        global = true,
        help = "Print machine-readable JSON results; never prompt"
    )]
    json: bool,
    #[command(subcommand)]
    command: Commands,
}
#[derive(Args, Default)]
struct Paths {
    #[arg(long)]
    base: Option<PathBuf>,
    #[arg(long)]
    coop: Option<PathBuf>,
    #[arg(long)]
    downloads: Option<PathBuf>,
    #[arg(long)]
    steam: Option<PathBuf>,
    #[arg(long)]
    proton: Option<PathBuf>,
}
#[derive(Subcommand)]
enum Commands {
    /// Import an editable mod-manager profile or an ordinary deployed setup.
    Import {
        #[arg(long)]
        mo2: Option<PathBuf>,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        game: Option<PathBuf>,
        #[arg(long)]
        documents: Option<PathBuf>,
        #[arg(long)]
        local: Option<PathBuf>,
        #[arg(long)]
        recipe: Option<PathBuf>,
        #[command(flatten)]
        paths: Paths,
    },
    /// Serve immutable published revisions and print a reusable SMS3 code.
    Serve {
        #[arg(long)]
        coop: Option<PathBuf>,
        #[arg(long)]
        no_relay: bool,
        #[arg(long)]
        bind: Option<std::net::SocketAddr>,
        #[arg(long)]
        relay: Option<String>,
    },
    /// Launch the installed revision with isolated runtime state and a memory cap.
    Play {
        #[arg(long)]
        coop: Option<PathBuf>,
        #[arg(long)]
        steam: Option<PathBuf>,
        #[arg(long)]
        proton: Option<PathBuf>,
        #[arg(long)]
        runtime: Option<PathBuf>,
        #[arg(long, default_value_t = 4096)]
        memory_limit_mib: u64,
        #[arg(long)]
        timeout_seconds: Option<u64>,
    },
    /// Inspect the current revision and preserved runtime changes.
    Status {
        #[arg(long)]
        coop: Option<PathBuf>,
    },
    #[command(name = "__launch-helper", hide = true)]
    LaunchHelper {
        #[arg(long)]
        coop: PathBuf,
        #[arg(long)]
        steam: Option<PathBuf>,
        #[arg(long)]
        proton: Option<PathBuf>,
        #[arg(long)]
        runtime: Option<PathBuf>,
        #[arg(long)]
        probe: bool,
    },
    #[command(name = "__wine-session", hide = true)]
    WineSession {
        #[arg(long)]
        proton: PathBuf,
        #[arg(long)]
        runtime: PathBuf,
        #[arg(long)]
        program: String,
        #[arg(long, allow_hyphen_values = true)]
        argument: Vec<String>,
    },
    /// Capture files, binary versions, shared configuration and declared plugin activation.
    Capture {
        #[arg(long)]
        game: Option<PathBuf>,
        #[arg(long)]
        documents: Option<PathBuf>,
        #[arg(long)]
        local: Option<PathBuf>,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Compare two captures; exit 1 when files/configuration differ.
    Compare { expected: PathBuf, actual: PathBuf },
    /// Validate a recipe-v1, sync-v2 or revision-v3 package without installing.
    Inspect { package: PathBuf },
    /// Reconstruct an exact recipe profile in isolated managed storage.
    Reconstruct {
        #[arg(long)]
        recipe: PathBuf,
        #[command(flatten)]
        paths: Paths,
    },
    /// Deploy a deterministic recipe candidate for host preparation or comparison.
    Prepare {
        #[arg(long)]
        recipe: PathBuf,
        #[arg(long)]
        profile: String,
        #[arg(long)]
        reconstructed: Option<PathBuf>,
        #[arg(short, long)]
        output: PathBuf,
        #[command(flatten)]
        paths: Paths,
    },
    /// Freeze and sign an imported setup, or publish a legacy recipe-backed snapshot.
    Publish {
        #[arg(long)]
        snapshot: Option<PathBuf>,
        #[arg(long)]
        recipe: Option<PathBuf>,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        include_change: Vec<String>,
        #[arg(long)]
        use_source: Vec<String>,
        #[arg(long)]
        keep_runtime: bool,
        #[arg(long)]
        reconstructed: Option<PathBuf>,
        #[arg(short, long)]
        output: PathBuf,
        #[command(flatten)]
        paths: Paths,
    },
    /// Produce an SMS2 code pinning an uploaded package's HTTPS URL and SHA-256.
    Share {
        package: PathBuf,
        #[arg(long)]
        url: String,
    },
    /// List verified cache reuse, required downloads, differences and blockers.
    Plan {
        input: String,
        #[arg(long)]
        reconstructed: Option<PathBuf>,
        #[command(flatten)]
        paths: Paths,
    },
    /// Repair or install a target; leave the previous instance in backups.
    Sync {
        input: String,
        #[arg(long)]
        revision: Option<String>,
        #[arg(long)]
        peer_address: Option<std::net::SocketAddr>,
        #[arg(
            long,
            help = "Use cached reconstruction; repairs verify only requested sources, initial installs verify all directives"
        )]
        reconstructed: Option<PathBuf>,
        #[command(flatten)]
        paths: Paths,
    },
    /// Freshly hash a deployed instance against a shared package.
    Verify {
        package: PathBuf,
        #[arg(long)]
        instance: PathBuf,
    },
    /// Restore the latest retained backup.
    Rollback {
        #[arg(long)]
        coop: Option<PathBuf>,
    },
    /// Save and validate a Nexus key privately; never pass keys in arguments.
    NexusLogin {
        #[arg(long)]
        stdin: bool,
    },
    /// Save explicit local folder choices, or ask for the three acquisition paths.
    Configure {
        #[command(flatten)]
        paths: Paths,
        #[arg(long)]
        game: Option<PathBuf>,
        #[arg(long)]
        documents: Option<PathBuf>,
        #[arg(long)]
        local: Option<PathBuf>,
    },
    /// Report Steam discovery and required tools without starting anything.
    Doctor {
        #[arg(long)]
        steam: Option<PathBuf>,
    },
}
fn managed(paths: &Paths, interactive: bool) -> Result<(Storage, PathBuf, PathBuf)> {
    let base = config::resolve("base", paths.base.as_deref(), interactive)?;
    let downloads = config::resolve("downloads", paths.downloads.as_deref(), interactive)?;
    fs::create_dir_all(&downloads)?;
    let base = files::root(&base)?;
    let downloads = files::root(&downloads)?;
    let coop = config::resolve("coop", paths.coop.as_deref(), interactive)?;
    // Validate separation before any ownership or reconstruction writes.
    let coop = config::absolute(&coop)?;
    files::separate(&base, &coop)?;
    files::separate(&base, &downloads)?;
    files::separate(&coop, &downloads)?;
    Ok((Storage::open(&coop)?, base, downloads))
}
fn select_package(input: &str, storage: &Storage) -> Result<package::Package> {
    let path = if input.starts_with("SMS2.") {
        acquisition::import_code(input, &storage.root)?
    } else {
        PathBuf::from(input)
    };
    package::load(&path)
}
fn sources(paths: &Paths, base: &Path) -> Result<(Option<PathBuf>, Option<PathBuf>)> {
    let saved = config::load()?;
    let steam = paths.steam.clone().or_else(|| saved.get("steam").cloned());
    let proton = paths
        .proton
        .clone()
        .or_else(|| saved.get("proton").cloned());
    if steam.is_some() && proton.is_some() {
        return Ok((steam, proton));
    }
    let discovery = config::discover(&config::home()?, steam.as_deref())?;
    let candidates = discovery["candidates"].as_array().unwrap();
    if let Some(c) = candidates
        .iter()
        .find(|c| c["base"].as_str().is_some_and(|p| Path::new(p) == base))
    {
        if c["flatpak"] == true {
            return Err(Error::new("flatpak_unvalidated","Flatpak Steam was detected, but installer integration is unvalidated. Use a native Steam setup for the supported reconstruction path."));
        }
        return Ok((
            steam.or_else(|| c["steam"].as_str().map(PathBuf::from)),
            proton.or_else(|| c["proton"].as_str().map(PathBuf::from)),
        ));
    }
    Ok((steam, proton))
}
fn execute(cli: &Cli) -> Result<Value> {
    let interactive = !cli.json && std::io::stdin().is_terminal();
    match &cli.command {
        Commands::Import {
            mo2,
            profile,
            game,
            documents,
            local,
            recipe,
            paths,
        } => {
            let (storage, base, downloads) = managed(paths, interactive)?;
            let normalize = |p: &Option<PathBuf>| p.as_ref().map(|p| files::root(p)).transpose();
            let binding = authoring::Binding {
                schema: 1,
                mo2: normalize(mo2)?,
                profile: profile.clone(),
                game: normalize(game)?,
                documents: normalize(documents)?,
                local: normalize(local)?,
                recipe: normalize(recipe)?,
                base,
                downloads,
            };
            authoring::import(&storage, &binding)
        }
        Commands::Serve {
            coop,
            no_relay,
            bind,
            relay,
        } => {
            let storage = Storage::open(&config::resolve("coop", coop.as_deref(), interactive)?)?;
            peer::serve(&storage, *no_relay, *bind, relay.as_deref())
        }
        Commands::Play {
            coop,
            steam,
            proton,
            runtime,
            memory_limit_mib,
            timeout_seconds,
        } => {
            let storage = Storage::open(&config::resolve("coop", coop.as_deref(), interactive)?)?;
            let local_config = if storage.root.join("machine-paths.json").is_file() {
                Some(files::read_json::<Value>(
                    &storage.root.join("machine-paths.json"),
                )?)
            } else if storage.root.join("authoring-source.json").is_file() {
                Some(files::read_json::<Value>(
                    &storage.root.join("authoring-source.json"),
                )?)
            } else {
                None
            };
            let base = if let Some(p) = local_config.as_ref().and_then(|v| v["base"].as_str()) {
                PathBuf::from(p)
            } else {
                config::resolve("base", None, interactive)?
            };
            let paths = Paths {
                steam: steam.clone(),
                proton: proton.clone(),
                ..Paths::default()
            };
            let (steam, proton) = sources(&paths, &base)?;
            let steam = steam.ok_or_else(|| Error::new("steam_missing", "Supply --steam."))?;
            let proton = proton.ok_or_else(|| Error::new("proton_missing", "Supply --proton."))?;
            let runtime = runtime
                .clone()
                .or_else(|| proton.parent().map(|p| p.join("SteamLinuxRuntime_sniper")))
                .filter(|p| p.join("run").is_file())
                .ok_or_else(|| {
                    Error::new(
                        "runtime_missing",
                        "Supply --runtime pointing to SteamLinuxRuntime_sniper.",
                    )
                })?;
            play::play(
                &storage,
                &steam,
                &proton,
                &runtime,
                *memory_limit_mib,
                *timeout_seconds,
            )
        }
        Commands::Status { coop } => {
            let storage = Storage::open(&config::resolve("coop", coop.as_deref(), interactive)?)?;
            play::status(&storage)
        }
        Commands::LaunchHelper { .. } | Commands::WineSession { .. } => Err(Error::new(
            "internal_error",
            "Launch helper must execute before background threads start.",
        )),

        Commands::Capture {
            game,
            documents,
            local,
            output,
        } => {
            let game = config::resolve("game", game.as_deref(), interactive)?;
            let documents = config::resolve("documents", documents.as_deref(), interactive)?;
            let local = config::resolve("local", local.as_deref(), interactive)?;
            let data = snapshot::capture(&game, Some(&documents), Some(&local))?;
            if output.exists() {
                return Err(Error::new(
                    "destination_exists",
                    "Choose a new snapshot filename.",
                ));
            }
            files::atomic_json(output, &data)?;
            Ok(
                json!({"state":"captured","snapshot":output,"target_id":data.id()?,"files":data.files.len(),"versions":data.versions,"plugins":data.plugins.len(),"warnings":data.warnings,"runtime_verified":false}),
            )
        }
        Commands::Compare { expected, actual } => Ok(serde_json::to_value(snapshot::compare(
            &snapshot::load(expected)?,
            &snapshot::load(actual)?,
        )?)?),
        Commands::Inspect { package: p } => {
            if revision::is_package(p) {
                let temp = tempfile::tempdir()?;
                let (r, h) = revision::read_package(p, temp.path())?;
                return Ok(
                    json!({"state":"inspected","schema":3,"revision":r.id()?,"sequence":h.sequence,"files":r.files.len(),"peer_files":r.objects().len(),"runtime_verified":false}),
                );
            }
            let p = package::load(p)?;
            Ok(
                json!({"state":"inspected","schema":p.manifest.schema,"name":p.manifest.name,"target_id":p.manifest.id()?,"files":p.manifest.files.len(),"sources":p.manifest.sources,"runtime_verified":false}),
            )
        }
        Commands::Reconstruct { recipe, paths } => {
            let (storage, base, downloads) = managed(paths, interactive)?;
            let recipe = Recipe::load(&files::root(recipe)?)?;
            let (steam, proton) = sources(paths, &base)?;
            let result = skyrim_mod_sync::engine::reconstruct(
                &recipe,
                &storage.root,
                &base,
                &downloads,
                steam.as_deref(),
                proton.as_deref(),
            )?;
            let problems = recipe.verify(&result)?;
            if !problems.is_empty() {
                return Err(Error::new(
                    "reconstruction_mismatch",
                    format!(
                        "{} reconstructed files differ from the recipe.",
                        problems.len()
                    ),
                ));
            }
            Ok(json!({"state":"reconstructed","path":result}))
        }
        Commands::Prepare {
            recipe,
            profile,
            reconstructed,
            output,
            paths,
        } => {
            let (storage, base, downloads) = managed(paths, interactive)?;
            let recipe = Recipe::load(&files::root(recipe)?)?;
            let reconstructed = if let Some(p) = reconstructed {
                files::root(p)?
            } else {
                let (steam, proton) = sources(paths, &base)?;
                skyrim_mod_sync::engine::reconstruct(
                    &recipe,
                    &storage.root,
                    &base,
                    &downloads,
                    steam.as_deref(),
                    proton.as_deref(),
                )?
            };
            let problems = recipe.verify(&reconstructed)?;
            if !problems.is_empty() {
                return Err(Error::new(
                    "reconstruction_mismatch",
                    format!("{} files differ from recipe.", problems.len()),
                ));
            }
            fs::create_dir_all(storage.root.join("stages"))?;
            let stage = tempfile::tempdir_in(storage.root.join("stages"))?;
            let manifest = skyrim_mod_sync::recipe::materialize(
                &recipe,
                &reconstructed,
                profile,
                stage.path(),
            )?;
            package::write(output, &manifest, &recipe.artifact)?;
            Ok(
                json!({"state":"prepared","package":output,"target_id":manifest.id()?,"files":manifest.files.len(),"runtime_verified":false}),
            )
        }
        Commands::Publish {
            snapshot: snap,
            recipe,
            profile,
            reconstructed,
            output,
            paths,
            include_change,
            use_source,
            keep_runtime,
        } => {
            if snap.is_none() && recipe.is_none() && profile.is_none() {
                let storage = Storage::open(&config::resolve(
                    "coop",
                    paths.coop.as_deref(),
                    interactive,
                )?)?;
                return authoring::publish_with_choices(
                    &storage,
                    output,
                    include_change,
                    use_source,
                    *keep_runtime,
                    interactive,
                );
            }
            if !include_change.is_empty() {
                return Err(Error::new(
                    "invalid_arguments",
                    "--include-change belongs to instance publishing.",
                ));
            }
            let snap = snap.as_deref().ok_or_else(|| {
                Error::new(
                    "invalid_arguments",
                    "Legacy publish needs --snapshot, --recipe and --profile.",
                )
            })?;
            let recipe = recipe
                .as_deref()
                .ok_or_else(|| Error::new("invalid_arguments", "Legacy publish needs --recipe."))?;
            let profile = profile.as_deref().ok_or_else(|| {
                Error::new("invalid_arguments", "Legacy publish needs --profile.")
            })?;
            let host = snapshot::load(snap)?;
            let (storage, base, downloads) = managed(paths, interactive)?;
            let recipe = Recipe::load(&files::root(recipe)?)?;
            let reconstruction = if let Some(p) = reconstructed {
                files::root(p)?
            } else {
                let (steam, proton) = sources(paths, &base)?;
                skyrim_mod_sync::engine::reconstruct(
                    &recipe,
                    &storage.root,
                    &base,
                    &downloads,
                    steam.as_deref(),
                    proton.as_deref(),
                )?
            };
            target::publish(&host, &recipe, &reconstruction, profile, output, &storage)
        }
        Commands::NexusLogin { stdin } => {
            config::nexus_login(*stdin, !cli.json)?;
            Ok(json!({"state":"nexus_connected"}))
        }
        Commands::Share { package: p, url } => {
            package::load(p)?;
            let code = package::Code {
                schema: 2,
                url: url.clone(),
                sha256: files::sha(p)?,
                size: fs::metadata(p)?.len(),
            };
            let encoded = code.encode()?;
            // Verify the uploaded object before issuing a code; typo/hosting redirects
            // must fail here, rather than sending every friend to a broken download.
            let tmp = tempfile::tempdir()?;
            acquisition::download(url, &tmp.path().join("package"), code.size, &code.sha256)?;
            Ok(json!({"state":"shared","code":encoded}))
        }
        Commands::Plan {
            input,
            paths,
            reconstructed,
        } => {
            let (storage, base, downloads) = managed(paths, interactive)?;
            if input.starts_with("SMS3.") || revision::is_package(Path::new(input)) {
                let r = if input.starts_with("SMS3.") {
                    peer::fetch(&revision::Channel::parse(input)?, &storage, None, None)?.revision
                } else {
                    revision::read_package(Path::new(input), &storage.root)?.0
                };
                let changed = if storage.instance().exists() {
                    crate_diff(&r, &storage)?
                } else {
                    json!({"files_replaced":r.files.len(),"staged_bytes":r.files.iter().map(|e|e.size).sum::<u64>()})
                };
                return Ok(
                    json!({"state":"planned","revision":r.id()?,"changes":changed,"sources":r.files,"runtime_verified":false}),
                );
            }
            let p = select_package(input, &storage)?;
            target::plan_with_cache(
                &p,
                &base,
                &downloads,
                &storage.root,
                reconstructed.as_deref(),
            )
        }
        Commands::Sync {
            input,
            reconstructed,
            paths,
            revision: pin,
            peer_address,
        } => {
            let (storage, base, downloads) = managed(paths, interactive)?;
            let (steam, proton) = sources(paths, &base)?;
            if input.starts_with("SMS3.") || revision::is_package(Path::new(input)) {
                let (r, h) = if input.starts_with("SMS3.") {
                    let channel = revision::Channel::parse(input)?;
                    let reply = peer::fetch(&channel, &storage, pin.as_deref(), *peer_address)?;
                    (reply.revision, reply.head)
                } else {
                    if pin.is_some() {
                        return Err(Error::new(
                            "invalid_arguments",
                            "A revision file already pins its revision.",
                        ));
                    }
                    revision::read_package(Path::new(input), &storage.root)?
                };
                return peer::sync(
                    &r,
                    &h,
                    &storage,
                    &base,
                    &downloads,
                    steam.as_deref(),
                    proton.as_deref(),
                    *peer_address,
                );
            }
            if pin.is_some() || peer_address.is_some() {
                return Err(Error::new(
                    "invalid_arguments",
                    "Peer/revision selection requires SMS3.",
                ));
            }
            let p = select_package(input, &storage)?;
            target::sync(
                &p,
                &storage,
                &base,
                &downloads,
                steam.as_deref(),
                proton.as_deref(),
                reconstructed.as_deref(),
            )
        }
        Commands::Verify {
            package: p,
            instance,
        } => {
            if revision::is_package(p) {
                let temp = tempfile::tempdir()?;
                let (r, _) = revision::read_package(p, temp.path())?;
                target::verify(&r.legacy(), instance)
            } else {
                target::verify(&package::load(p)?.manifest, instance)
            }
        }
        Commands::Rollback { coop } => {
            Storage::open(&config::resolve("coop", coop.as_deref(), interactive)?)?.rollback()
        }
        Commands::Configure {
            paths,
            game,
            documents,
            local,
        } => {
            let mut values = BTreeMap::new();
            for (n, p) in [
                ("base", &paths.base),
                ("coop", &paths.coop),
                ("downloads", &paths.downloads),
                ("steam", &paths.steam),
                ("proton", &paths.proton),
                ("game", game),
                ("documents", documents),
                ("local", local),
            ] {
                if let Some(p) = p {
                    values.insert(n.to_owned(), p.clone());
                }
            }
            if values.is_empty() {
                for n in ["base", "coop", "downloads"] {
                    values.insert(n.into(), config::resolve(n, None, interactive)?);
                }
            }
            config::save(values)?;
            Ok(json!({"state":"configured","paths":config::load()?}))
        }
        Commands::Doctor { steam } => Ok(
            json!({"state":"diagnostics","platform":std::env::consts::OS,"architecture":std::env::consts::ARCH,"settings":config::load()?,"config_directory":config::directory()?,"default_storage":config::default_storage()?,"steam":config::discover(&config::home()?,steam.as_deref())?,"bubblewrap":config::which("bwrap"),"isolated_play":play::diagnostics(),"nexus_key_present":config::directory()?.join("nexus-api-key").is_file(),"gameplay_validated":false}),
        ),
    }
}
fn crate_diff(r: &revision::Revision, storage: &Storage) -> Result<Value> {
    let manifest = r.legacy();
    let diff = skyrim_mod_sync::repair::diff(&manifest, &storage.instance())?;
    Ok(
        json!({"files_replaced":diff.changed.len(),"files_removed":diff.removed.len(),"files_unchanged":diff.unchanged,"staged_bytes":diff.bytes,"changed_files":diff.changed.iter().map(|f|&f.path).collect::<Vec<_>>() }),
    )
}
fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Commands::WineSession {
        proton,
        runtime,
        program,
        argument,
    } = &cli.command
    {
        match play::wine_session(proton, runtime, program, argument) {
            Ok(_) => return ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(2);
            }
        }
    }
    if let Commands::LaunchHelper {
        coop,
        steam,
        proton,
        runtime,
        probe,
    } = &cli.command
    {
        let result = play::helper(
            coop,
            steam.as_deref(),
            proton.as_deref(),
            runtime.as_deref(),
            *probe,
        );
        match result {
            Ok(v) => {
                println!("{v}");
                return ExitCode::SUCCESS;
            }
            Err(e) => {
                println!(
                    "{}",
                    json!({"state":"failed","reason":e.code,"message":e.message})
                );
                return ExitCode::from(2);
            }
        }
    }
    let _ = ctrlc::set_handler(|| {
        skyrim_mod_sync::CANCELLED.store(true, std::sync::atomic::Ordering::Relaxed)
    });
    match execute(&cli) {
        Ok(v) => {
            println!(
                "{}",
                if cli.json {
                    serde_json::to_string(&v).unwrap()
                } else {
                    serde_json::to_string_pretty(&v).unwrap()
                }
            );
            if v["files_match"] == false && v.get("differences").is_some() {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            if cli.json {
                println!(
                    "{}",
                    json!({"state":"failed","reason":e.code,"message":e.message})
                );
            } else {
                eprintln!("{e}");
            }
            ExitCode::from(if e.code == "cancelled" { 130 } else { 2 })
        }
    }
}
