mod categories;
mod clusters;
mod game_data;
mod metrics;
mod model;
mod output;
mod sc2;

use std::collections::BTreeMap;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use clap::Parser;
use sc2_proto::common::Race;
use sc2_proto::sc2api::Status;

use crate::{
    clusters::ClusterConfig,
    metrics::Analyzer,
    model::{
        EventRecord, MetricsFooter, MetricsHeader, MetricsRecord, PlayerHeader, PlayerStats,
        Summary, EVENTS_SCHEMA_VERSION, METRICS_SCHEMA_VERSION, SUMMARY_SCHEMA_VERSION,
    },
    output::{EventsWriter, MetricsWriter},
};

const INITIAL_WS_TIMEOUT_SECS: u64 = 30;

#[derive(Parser, Debug)]
#[command(author, version, about = "Play an SC2 replay via dockerised client and emit macro/army/engagement diagnostics")]
struct Args {
    /// Path to a .SC2Replay file.
    #[arg(long)]
    replay: PathBuf,

    /// Docker image containing the Linux SC2 client.
    #[arg(long, default_value = "aiarena/arenaclient-sc2:v0.8.0")]
    docker_image: String,

    /// Local directory of ladder maps, mounted into the container's Maps folder.
    /// Defaults to `/Applications/StarCraft II/Maps` on macOS, `maps` elsewhere.
    #[arg(long)]
    maps_dir: Option<PathBuf>,

    /// Observed player id (1 or 2). Auto-detected via --bot-name when possible.
    #[arg(long)]
    observed_player_id: Option<u32>,

    /// Bot name to match against replay player names for the observed player.
    #[arg(long)]
    bot_name: Option<String>,

    /// Number of game loops to step between observations.
    #[arg(long, default_value_t = 22)]
    sample_loops: u32,

    /// Output directory (default: output/analysis/<replay stem>).
    #[arg(long)]
    output_dir: Option<PathBuf>,

    /// Host port to map to the container's SC2 port.
    #[arg(long)]
    host_port: Option<u16>,

    /// SC2 websocket port inside the container.
    #[arg(long, default_value_t = 8637)]
    sc2_port: u16,

    /// Path to the SC2 installation inside the container.
    #[arg(long, default_value = "/root/StarCraftII")]
    container_sc2_path: String,

    /// Directory inside the container where the replay file is mounted.
    #[arg(long, default_value = "/replays")]
    container_replay_dir: String,

    /// Override the shell command that launches SC2 inside the container.
    #[arg(long)]
    container_launch: Option<String>,

    /// Host path to alignmutex.so (Apple Silicon Rosetta fix).
    /// Mounted read-only into the SC2 container and set as LD_PRELOAD so
    /// SC2's misaligned mutexes don't crash at map load under Rosetta.
    /// When omitted and the Rosetta workarounds are on (the default on
    /// macOS/ARM), `apple-silicon/alignmutex.so` is used automatically when
    /// that file exists (build it with ./apple-silicon/build.sh).
    #[arg(long)]
    alignmutex_so: Option<PathBuf>,

    /// Force-enable the Rosetta workarounds for the dockerised SC2 client
    /// (argv padding + automatic alignmutex.so). They default on for
    /// macOS/ARM, so this is only needed to opt in elsewhere.
    #[arg(long, conflicts_with = "no_rosetta")]
    rosetta: bool,

    /// Force-disable the Rosetta workarounds (argv padding and the automatic
    /// alignmutex.so). Pass this when running macOS/ARM containers under
    /// QEMU instead of Rosetta (Rosetta disabled in the VM), where the stock
    /// command line is correct and the padding would break parsing instead.
    /// An explicit --alignmutex-so is still honoured.
    #[arg(long, conflicts_with = "rosetta")]
    no_rosetta: bool,

    /// Connect to an already-running SC2 client instead of launching docker.
    #[arg(long)]
    no_docker: bool,

    /// Launch the SC2 client installed on this machine instead of dockerising one.
    /// Useful when the replay is newer than the SC2 version in the docker image.
    #[arg(long)]
    local_sc2: bool,

    /// Host to connect to when using --no-docker.
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Clustering: cell radius for grouping combat units.
    #[arg(long, default_value_t = 12.0)]
    cluster_radius: f32,

    /// Clustering: radius for pairing opposing clusters as confrontations.
    #[arg(long, default_value_t = 20.0)]
    confront_radius: f32,

    /// Engagements: radius for opposing clusters to count as an engagement.
    #[arg(long, default_value_t = 14.0)]
    engagement_radius: f32,

    /// Engagements: minimum combat supply on each side to start an engagement.
    #[arg(long, default_value_t = 6.0)]
    engagement_min_supply: f32,
}

/// Rosetta workarounds default on for macOS/ARM: Docker runs x86 containers
/// under emulation there, and Rosetta is the fast path worth defaulting to.
/// (QEMU users must pass --no-rosetta: the argv padding breaks parsing when
/// /proc/self/cmdline is intact.)
fn default_rosetta() -> bool {
    cfg!(target_os = "macos") && cfg!(target_arch = "aarch64")
}

/// Default host path for the alignmutex preload library, used when the user
/// didn't pass --alignmutex-so explicitly.
fn default_alignmutex_so() -> PathBuf {
    PathBuf::from("apple-silicon/alignmutex.so")
}

/// OS-specific default for `--maps-dir`.
#[cfg(target_os = "macos")]
fn default_maps_dir() -> PathBuf {
    PathBuf::from("/Applications/StarCraft II/Maps")
}

#[cfg(not(target_os = "macos"))]
fn default_maps_dir() -> PathBuf {
    PathBuf::from("maps")
}

fn main() -> Result<()> {
    unsafe { std::env::set_var("RUST_BACKTRACE", "1") };
    crate::sc2::init_tracing()?;

    let args = Args::parse();
    let replay_path = resolve_replay_path(&args.replay)?;
    let replay_path = std::fs::canonicalize(replay_path)
        .with_context(|| format!("resolving replay path {}", replay_path.display()))?;
    // The maps dir is only mounted into a dockerised SC2; local analysis uses the
    // maps already installed next to the local client, so don't require it there.
    let maps_dir_raw = args.maps_dir.clone().unwrap_or_else(default_maps_dir);
    let maps_dir = if args.local_sc2 {
        maps_dir_raw
    } else {
        std::fs::canonicalize(&maps_dir_raw)
            .with_context(|| format!("resolving maps dir {}", maps_dir_raw.display()))?
    };

    let replay_metadata = crate::sc2::replay::replay_metadata_from_mpq(
        replay_path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("replay path not utf-8"))?,
    )
    .ok();
    tracing::info!(
        base_build = ?replay_metadata.as_ref().and_then(|m| m.base_build),
        data_version = ?replay_metadata.as_ref().and_then(|m| m.data_version.as_deref()),
        "parsed replay metadata"
    );

    let host_port = args.host_port.unwrap_or_else(|| crate::sc2::get_unused_port() as u16);

    // Rosetta workarounds: default on for macOS/ARM, force-off with
    // --no-rosetta (QEMU), force-on elsewhere with --rosetta.
    let rosetta = if args.no_rosetta {
        false
    } else {
        args.rosetta || default_rosetta()
    };
    // Preload library: explicit path wins; otherwise auto-use the default
    // build output when the workarounds are on and it exists.
    let alignmutex_so = args.alignmutex_so.clone().or_else(|| {
        if rosetta && default_alignmutex_so().is_file() {
            Some(default_alignmutex_so())
        } else {
            None
        }
    });
    if rosetta {
        tracing::info!("Rosetta workarounds enabled");
    }

    let docker_config = crate::sc2::docker::DockerConfig {
        image: args.docker_image.clone(),
        maps_dir: maps_dir.clone(),
        replay_host_path: Some(replay_path.clone()),
        host_port,
        sc2_port: args.sc2_port,
        container_sc2_path: args.container_sc2_path.clone(),
        container_maps_dir: crate::sc2::docker::DEFAULT_CONTAINER_MAPS_DIR.to_string(),
        container_replay_dir: Some(args.container_replay_dir.clone()),
        container_launch: args.container_launch.clone(),
        base_build: replay_metadata.as_ref().and_then(|m| m.base_build),
        data_version: replay_metadata
            .as_ref()
            .and_then(|m| m.data_version.clone()),
        host_network: false,
        extra_ports: vec![],
        alignmutex_host_path: alignmutex_so.clone(),
        rosetta_argv_workaround: rosetta,
    };

    let _local_sc2 = if args.local_sc2 {
        let sc2_path = crate::sc2::get_path_to_sc2();
        let data_version = replay_metadata
            .as_ref()
            .and_then(|m| m.data_version.clone());
        tracing::info!(sc2_path, host_port, "launching local SC2 for replay analysis");
        let launch = crate::sc2::launch_sc2_with_data_version(
            &sc2_path,
            None,
            data_version.as_deref(),
            host_port as i32,
        )
        .with_context(|| format!("launching local SC2 at {sc2_path}"))?;
        wait_for_local_sc2_port(host_port)?;
        tracing::info!("local SC2 ready");
        Some(launch)
    } else {
        None
    };

    let _docker = if !args.local_sc2 && !args.no_docker {
        if !maps_dir.is_dir() {
            tracing::warn!(
                maps_dir = %maps_dir.display(),
                "maps directory does not exist; SC2 may fail to load the replay's map"
            );
        }
        Some(crate::sc2::docker::DockerSc2::launch(&docker_config)?)
    } else if args.no_docker && !args.local_sc2 {
        tracing::warn!(
            host = %args.host,
            port = host_port,
            "connecting to existing SC2 client (no docker)"
        );
        None
    } else {
        None
    };

    if (alignmutex_so.is_some() || rosetta || args.no_rosetta)
        && (args.local_sc2 || args.no_docker)
    {
        tracing::warn!(
            "--alignmutex-so/--rosetta/--no-rosetta only apply to the dockerised SC2 client; ignoring them for local/existing client"
        );
    }
    if rosetta && alignmutex_so.is_none() && !args.local_sc2 && !args.no_docker {
        tracing::warn!(
            "Rosetta workarounds on but no alignmutex.so found (build it with ./apple-silicon/build.sh or pass --alignmutex-so); SC2 will likely crash at map load"
        );
    }

    let replay_arg = if args.local_sc2 || args.no_docker {
        replay_path.display().to_string()
    } else {
        docker_config.container_replay_path()
    };

    let mut api = crate::sc2::connect_websocket_with_timeout_secs(
        &args.host,
        host_port as i32,
        INITIAL_WS_TIMEOUT_SECS,
    )?;
    wait_for_client_ready(&mut api)?;

    let replay_info = fetch_replay_info(&mut api, &replay_arg)?;

    let players = replay_players(&replay_info);
    let observed_player_id = resolve_observed_player(&args, &players);
    tracing::info!(observed_player_id, "observed player");

    crate::sc2::start_replay_for_analysis(&mut api, &replay_arg, observed_player_id as i32)?;
    wait_for_in_replay_status(&mut api)?;
    tracing::info!("replay started");

    let table_res = crate::sc2::request_game_data(&mut api)?;
    let table = game_data::fetch_unit_table(&table_res);
    let upgrades = game_data::fetch_upgrade_table(&table_res);
    tracing::info!(upgrades = upgrades.by_ability.len(), "loaded upgrade table");

    let output_dir = args
        .output_dir
        .clone()
        .unwrap_or_else(|| default_output_dir(&replay_path));
    std::fs::create_dir_all(&output_dir)?;

    let map_name = replay_info.get_map_name().to_string();
    let metrics_header = MetricsRecord::Header(MetricsHeader {
        schema_version: METRICS_SCHEMA_VERSION,
        map_name: map_name.clone(),
        game_version: (!replay_info.get_game_version().is_empty())
            .then(|| replay_info.get_game_version().to_string()),
        data_build: replay_info.data_build,
        base_build: replay_info.base_build,
        observed_player_id,
        sample_loops: args.sample_loops,
        players: players.clone(),
    });
    let mut metrics_writer = MetricsWriter::create(&output_dir, metrics_header)?;
    let mut events_writer = EventsWriter::create(
        &output_dir,
        EventRecord::Header(model::EventHeader {
            schema_version: EVENTS_SCHEMA_VERSION,
        }),
    )?;

    let cluster_config = ClusterConfig {
        cluster_radius: args.cluster_radius,
        confront_radius: args.confront_radius,
        engagement_radius: args.engagement_radius,
        engagement_min_supply: args.engagement_min_supply,
        ..Default::default()
    };

    let mut analyzer = Analyzer::new(observed_player_id);

    let wall_start = Instant::now();
    let mut terminal_polls_without_result = 0usize;

    let final_tick = loop {
        let response = crate::sc2::observe(&mut api, true)?;
        let status = response.get_status();
        let response_observation = response.get_observation();
        let replay_results = response_observation.get_player_result();
        let observation = response_observation.get_observation();
        let game_loop = observation.get_game_loop();

        if !replay_results.is_empty() {
            tracing::info!(game_loop, "replay reached terminal player_result");
            break game_loop;
        }

        if matches!(status, Status::ended | Status::quit) {
            terminal_polls_without_result += 1;
            if terminal_polls_without_result >= 20 {
                break game_loop;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
            continue;
        }
        terminal_polls_without_result = 0;

        let frame = metrics::decode_observation(observation);
        let (tick_metrics, events) =
            analyzer.process_frame(&frame, &table, &upgrades, &cluster_config);

        metrics_writer.push(MetricsRecord::Tick(tick_metrics))?;
        for event in events {
            events_writer.push(event)?;
        }

        if analyzer.frame_count().is_multiple_of(60) {
            tracing::info!(
                game_loop,
                time_seconds = frame.tick as f64 / 22.4,
                elapsed = ?wall_start.elapsed(),
                "analyzing"
            );
        }

        crate::sc2::step_replay(&mut api, args.sample_loops)?;
    };

    let final_time = final_tick as f64 / 22.4;

    let mut events = Vec::new();
    analyzer.finish_engagements(final_tick, &mut events);
    for event in events {
        events_writer.push(event)?;
    }

    let result = match observed_player_id {
        1 => replay_result_of(&replay_info, 1),
        _ => replay_result_of(&replay_info, 2),
    };

    let engagement_summaries = analyzer.take_engagement_summaries();
    let player_stats: BTreeMap<u32, PlayerStats> = analyzer
        .stats
        .iter()
        .map(|(&player, s)| {
            (
                player,
                PlayerStats {
                    final_expansions: analyzer.final_expansions(player),
                    peak_expansions: s.peak_expansions,
                    peak_workers: s.peak_workers,
                    peak_army_supply: s.peak_army_supply,
                    peak_army_value: s.peak_army_value,
                    total_killed_value: s.killed.units_value
                        + s.killed.workers_value
                        + s.killed.support_value
                        + s.killed.structures_value,
                    total_lost_value: s.lost.units_value
                        + s.lost.workers_value
                        + s.lost.support_value
                        + s.lost.structures_value,
                    worker_kills_value: s.killed.workers_value,
                    worker_losses_value: s.lost.workers_value,
                },
            )
        })
        .collect();

    let transitions = analyzer.take_transitions();
    let upgrades = analyzer.take_upgrade_summaries();
    let summary = Summary {
        schema_version: SUMMARY_SCHEMA_VERSION,
        replay_path: replay_path.display().to_string(),
        map_name,
        game_version: (!replay_info.get_game_version().is_empty())
            .then(|| replay_info.get_game_version().to_string()),
        base_build: replay_info.base_build,
        data_version: replay_metadata
            .as_ref()
            .and_then(|m| m.data_version.clone()),
        duration_loops: replay_info.game_duration_loops.unwrap_or(0),
        observed_player_id,
        players: players.clone(),
        result,
        engagements: engagement_summaries,
        transitions,
        upgrades,
        player_stats,
    };

    let metrics_path = metrics_writer.finish(MetricsRecord::Footer(MetricsFooter {
        tick: final_tick,
        time_seconds: final_time,
        result: summary.result.clone(),
        frames_analyzed: analyzer.frame_count(),
    }))?;
    events_writer.finish(EventRecord::Footer(model::EventFooter {
        tick: final_tick,
        time_seconds: final_time,
    }))?;
    let summary_path = output::write_summary(&output_dir, &summary)?;

    tracing::info!(
        elapsed = ?wall_start.elapsed(),
        metrics = %metrics_path.display(),
        summary = %summary_path.display(),
        "analysis complete"
    );

    crate::sc2::leave_current_game(&mut api)?;
    crate::sc2::quit_client(&mut api)?;

    Ok(())
}

fn resolve_replay_path(replay: &Path) -> Result<&Path> {
    if !replay.is_file() {
        bail!("replay file does not exist: {}", replay.display());
    }
    if !replay
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("SC2Replay"))
    {
        bail!("path is not a .SC2Replay: {}", replay.display());
    }
    Ok(replay)
}

fn default_output_dir(replay: &Path) -> PathBuf {
    let stem = replay
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("replay");
    PathBuf::from("output/analysis").join(stem)
}

fn resolve_observed_player(args: &Args, players: &[PlayerHeader]) -> u32 {
    if let Some(id) = args.observed_player_id {
        return id;
    }
    if let Some(bot_name) = args.bot_name.as_deref() {
        let needle = bot_name.to_ascii_lowercase();
        if let Some(player) = players.iter().find(|p| p.name.to_ascii_lowercase() == needle) {
            return player.player_id;
        }
        tracing::warn!(bot_name, "bot name not found in replay players; using player 1");
    }
    1
}

fn replay_players(replay_info: &sc2_proto::sc2api::ResponseReplayInfo) -> Vec<PlayerHeader> {
    replay_info
        .get_player_info()
        .iter()
        .map(|player| {
            let player_info = player.get_player_info();
            PlayerHeader {
                player_id: player_info.get_player_id(),
                name: player_info.get_player_name().to_string(),
                race: race_name(player_info.get_race_actual()).to_string(),
                result: player
                    .has_player_result()
                    .then(|| format!("{:?}", player.get_player_result().get_result())),
            }
        })
        .collect()
}

fn replay_result_of(replay_info: &sc2_proto::sc2api::ResponseReplayInfo, player_id: u32) -> Option<String> {
    replay_info
        .get_player_info()
        .iter()
        .find(|p| p.get_player_info().get_player_id() == player_id)
        .filter(|p| p.has_player_result())
        .map(|p| format!("{:?}", p.get_player_result().get_result()))
}

fn race_name(race: Race) -> &'static str {
    match race {
        Race::Terran => "terran",
        Race::Zerg => "zerg",
        Race::Protoss => "protoss",
        Race::Random => "random",
        Race::NoRace => "norace",
    }
}

/// Wait for a freshly launched local SC2 to open its websocket port, then for the
/// client to accept pings. Mirrors the docker path's port wait with a long timeout
/// to cover a cold client start (first launch can be slow).
fn wait_for_local_sc2_port(port: u16) -> Result<()> {
    const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for local SC2 to open port {port}");
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

fn wait_for_client_ready(api: &mut crate::sc2::api::Api) -> Result<()> {
    const MAX_READY_POLLS: usize = 30;
    const READY_POLL_DELAY: std::time::Duration = std::time::Duration::from_secs(1);

    for _ in 1..=MAX_READY_POLLS {
        match crate::sc2::ping(api) {
            Ok(res) => {
                if !matches!(res.get_status(), Status::unknown) {
                    return Ok(());
                }
            }
            Err(err) => return Err(err.into()),
        }
        std::thread::sleep(READY_POLL_DELAY);
    }
    bail!("timed out waiting for SC2 to become ready")
}

fn wait_for_in_replay_status(api: &mut crate::sc2::api::Api) -> Result<()> {
    const MAX_POLLS: usize = 120;
    const POLL_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

    for _ in 1..=MAX_POLLS {
        match crate::sc2::ping(api) {
            Ok(res) => {
                if res.get_status() == Status::in_replay {
                    return Ok(());
                }
            }
            Err(err) => return Err(err.into()),
        }
        std::thread::sleep(POLL_DELAY);
    }
    bail!("timed out waiting for SC2 to enter in_replay status")
}

/// SC2 can briefly report `InvalidReplayPath` while its data is still loading
/// after the websocket becomes ready (especially under emulation). Retry a few
/// times before giving up.
fn fetch_replay_info(
    api: &mut crate::sc2::api::Api,
    replay_path: &str,
) -> Result<sc2_proto::sc2api::ResponseReplayInfo> {
    const MAX_ATTEMPTS: usize = 10;
    const RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

    for attempt in 1..=MAX_ATTEMPTS {
        let info = crate::sc2::replay_info(api, replay_path)?;
        if !info.has_error() {
            return Ok(info);
        }
        tracing::warn!(
            attempt,
            error = ?info.get_error(),
            details = info.get_error_details(),
            "replay_info not ready yet; retrying"
        );
        std::thread::sleep(RETRY_DELAY);
    }
    bail!("replay_info kept failing after {MAX_ATTEMPTS} attempts")
}
