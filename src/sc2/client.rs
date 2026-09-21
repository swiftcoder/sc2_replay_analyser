//! Minimal SC2 client helpers for replay analysis
//!
//! Covers: websocket connection, local client launch, replay start/observe/step
//! helpers, replay metadata parsing (see [`super::replay`]), and dockerised
//! client launch (see [`super::docker`]).

use std::{
    net::{TcpListener, TcpStream, ToSocketAddrs},
    path::Path,
    process::{Child, Command},
    thread,
    time::{Duration, Instant},
};

use sc2_proto::sc2api::{self, Request};
use tracing_subscriber::prelude::*;
use tungstenite::{client, handshake::HandshakeError};

use crate::sc2::api::Api;
use crate::sc2::error::Sc2Error;

const LOCALHOST: &str = "127.0.0.1";
#[cfg(target_os = "macos")]
const SC2_BINARY: &str = "SC2.app/Contents/MacOS/SC2";
#[cfg(any(target_os = "linux", target_os = "windows"))]
const SC2_BINARY: &str = "SC2_x64.exe";

pub fn init_tracing() -> Result<(), Sc2Error> {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    let logfmt_layer = tracing_logfmt::builder()
        .layer()
        .with_writer(std::io::stdout);
    let subscriber = tracing_subscriber::registry()
        .with(env_filter)
        .with(logfmt_layer);
    let _ = tracing::subscriber::set_global_default(subscriber);
    Ok(())
}

pub fn get_unused_port() -> i32 {
    (5025..65535)
        .find(|port| TcpListener::bind((LOCALHOST, *port)).is_ok())
        .unwrap() as i32
}

pub struct KillOnExit {
    child: Child,
}

impl KillOnExit {
    pub fn new(child: Child) -> Self {
        Self { child }
    }
}

impl Drop for KillOnExit {
    fn drop(&mut self) {
        self.child.kill().expect("failed to kill child process")
    }
}

/// Convenience wrapper without data-version override. Kept as public API.
#[allow(dead_code)]
pub fn launch_sc2(
    sc2_path: &str,
    sc2_version: Option<u32>,
    port: i32,
) -> Result<KillOnExit, Sc2Error> {
    launch_sc2_with_data_version_and_display_mode(sc2_path, sc2_version, None, 0, port)
}

pub fn launch_sc2_with_data_version(
    sc2_path: &str,
    sc2_version: Option<u32>,
    data_version: Option<&str>,
    port: i32,
) -> Result<KillOnExit, Sc2Error> {
    launch_sc2_with_data_version_and_display_mode(sc2_path, sc2_version, data_version, 0, port)
}

pub fn launch_sc2_with_data_version_and_display_mode(
    sc2_path: &str,
    sc2_version: Option<u32>,
    data_version: Option<&str>,
    display_mode: i32,
    port: i32,
) -> Result<KillOnExit, Sc2Error> {
    let sc2_version = sc2_version.unwrap_or_else(|| get_latest_base_version(sc2_path));
    let sc2_full_path = format!("{}/Versions/Base{}/{}", sc2_path, sc2_version, SC2_BINARY);

    #[cfg(target_os = "linux")]
    {
        return launch_sc2_linux(sc2_path, &sc2_full_path, data_version, display_mode, port);
    }

    let mut process = Command::new(sc2_full_path);
    process.current_dir(sc2_path);
    add_sc2_launch_args(&mut process, port, data_version, display_mode);

    let child = process.spawn()?;

    Ok(KillOnExit::new(child))
}

fn add_sc2_launch_args(
    process: &mut Command,
    port: i32,
    data_version: Option<&str>,
    display_mode: i32,
) {
    process
        .arg("-listen")
        .arg(LOCALHOST)
        .arg("-port")
        .arg(port.to_string())
        .arg("-displayMode")
        .arg(display_mode.to_string()); // 0=windowed, 1=fullscreen, 2=windowed-fullscreen

    if let Some(data_version) = data_version
        && !data_version.is_empty()
    {
        process.arg("-dataVersion").arg(data_version);
    }
}

#[cfg(target_os = "linux")]
fn launch_sc2_linux(
    sc2_path: &str,
    sc2_full_path: &str,
    data_version: Option<&str>,
    display_mode: i32,
    port: i32,
) -> Result<KillOnExit, Sc2Error> {
    let proton_bin = resolve_proton_bin(sc2_path);
    let mut process = Command::new(&proton_bin);
    let workdir = Path::new(sc2_full_path)
        .parent()
        .unwrap_or_else(|| Path::new(sc2_path));
    process.current_dir(workdir).arg("run").arg(sc2_full_path);
    add_sc2_launch_args(&mut process, port, data_version, display_mode);

    if std::env::var_os("STEAM_COMPAT_DATA_PATH").is_none()
        || std::env::var_os("STEAM_COMPAT_CLIENT_INSTALL_PATH").is_none()
    {
        if let Some((compat_data_path, client_install_path)) = infer_steam_compat_paths(sc2_path) {
            if std::env::var_os("STEAM_COMPAT_DATA_PATH").is_none() {
                process.env("STEAM_COMPAT_DATA_PATH", &compat_data_path);
            }
            if std::env::var_os("STEAM_COMPAT_CLIENT_INSTALL_PATH").is_none() {
                process.env("STEAM_COMPAT_CLIENT_INSTALL_PATH", &client_install_path);
            }
            if std::env::var_os("WINEPREFIX").is_none() {
                process.env("WINEPREFIX", format!("{compat_data_path}/pfx"));
            }
            if std::env::var_os("STEAM_COMPAT_INSTALL_PATH").is_none() {
                process.env("STEAM_COMPAT_INSTALL_PATH", sc2_path);
            }
            if let Some(app_id) = infer_steam_app_id(sc2_path) {
                if std::env::var_os("STEAM_COMPAT_APP_ID").is_none() {
                    process.env("STEAM_COMPAT_APP_ID", &app_id);
                }
                if std::env::var_os("SteamAppId").is_none() {
                    process.env("SteamAppId", &app_id);
                }
                if std::env::var_os("SteamGameId").is_none() {
                    process.env("SteamGameId", app_id);
                }
            }
            if std::env::var_os("WINEPATH").is_none()
                && let Some(winepath) = infer_sc2_winepath(sc2_path, sc2_full_path)
            {
                process.env("WINEPATH", winepath);
            }
            tracing::info!(
                proton_bin,
                compat_data_path,
                client_install_path,
                "launching SC2 via Proton with inferred Steam compatibility paths"
            );
        } else {
            tracing::info!(proton_bin, "launching SC2 via Proton");
        }
    } else {
        tracing::info!(
            proton_bin,
            "launching SC2 via Proton with existing Steam compatibility env"
        );
    }

    let child = process.spawn()?;
    Ok(KillOnExit::new(child))
}

#[cfg(target_os = "linux")]
fn infer_steam_compat_paths(sc2_path: &str) -> Option<(String, String)> {
    let marker = "/steamapps/compatdata/";
    let prefix_idx = sc2_path.find(marker)?;
    let steam_client_install = sc2_path[..prefix_idx].to_string();

    let after_marker = &sc2_path[prefix_idx + marker.len()..];
    let app_id_end = after_marker.find('/')?;
    let app_id = &after_marker[..app_id_end];
    let compat_data_path = format!("{steam_client_install}{marker}{app_id}");

    Some((compat_data_path, steam_client_install))
}

#[cfg(target_os = "linux")]
fn infer_steam_app_id(sc2_path: &str) -> Option<String> {
    let marker = "/steamapps/compatdata/";
    let after = &sc2_path[sc2_path.find(marker)? + marker.len()..];
    Some(after[..after.find('/')?].to_string())
}

#[cfg(target_os = "linux")]
fn infer_sc2_winepath(sc2_path: &str, sc2_full_path: &str) -> Option<String> {
    let exe_dir = Path::new(sc2_full_path).parent()?;
    let support64 = Path::new(sc2_path).join("Support64");
    let support = Path::new(sc2_path).join("Support");

    let mut paths = vec![exe_dir.to_string_lossy().to_string(), sc2_path.to_string()];
    if support64.is_dir() {
        paths.push(support64.to_string_lossy().to_string());
    }
    if support.is_dir() {
        paths.push(support.to_string_lossy().to_string());
    }
    Some(paths.join(";"))
}

#[cfg(target_os = "linux")]
fn resolve_proton_bin(sc2_path: &str) -> String {
    if let Ok(bin) = std::env::var("SC2_PROTON_BIN") {
        return bin;
    }

    if let Some((_, steam_client_install)) = infer_steam_compat_paths(sc2_path) {
        let proton_10 = format!("{steam_client_install}/steamapps/common/Proton 10.0/proton");
        if Path::new(&proton_10).is_file() {
            return proton_10;
        }

        let proton_experimental =
            format!("{steam_client_install}/steamapps/common/Proton - Experimental/proton");
        if Path::new(&proton_experimental).is_file() {
            return proton_experimental;
        }
    }

    "proton".to_string()
}

pub fn get_path_to_sc2() -> String {
    match std::env::var_os("SC2PATH") {
        Some(path) => path
            .to_str()
            .unwrap()
            .replace('~', dirs::home_dir().unwrap().to_str().unwrap()),
        None => {
            #[cfg(windows)]
            {
                let file = std::fs::read_to_string(format!(
                    "{}/Documents/StarCraft II/ExecuteInfo.txt",
                    dirs::home_dir().unwrap().to_str().unwrap(),
                ))
                .expect("Can't read ExecuteInfo.txt");

                use regex::Regex;

                let re = Regex::new(r"= (.*)\\Versions")
                    .unwrap()
                    .captures(&file)
                    .unwrap();

                let path = Path::new(&re[1]);
                if path.exists() {
                    return path.to_str().unwrap().replace('\\', "/");
                }

                "C:/Program Files (x86)/StarCraft II".to_string()
            }
            #[cfg(target_os = "linux")]
            {
                let home = dirs::home_dir().unwrap();
                if let Some(path) = find_steam_proton_sc2_path(&home) {
                    path
                } else {
                    format!("{}/StarCraftII", home.to_str().unwrap())
                }
            }
            #[cfg(target_os = "macos")]
            {
                "/Applications/StarCraft II".to_string()
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn find_steam_proton_sc2_path(home: &Path) -> Option<String> {
    let compat_root = home.join(".steam/steam/steamapps/compatdata");
    let entries = compat_root.read_dir().ok()?;
    for entry in entries.flatten() {
        let candidate: PathBuf = entry
            .path()
            .join("pfx/drive_c/Program Files (x86)/StarCraft II");
        if candidate.is_dir() {
            return Some(candidate.to_string_lossy().to_string());
        }
    }
    None
}

fn get_latest_base_version(sc2_path: &str) -> u32 {
    Path::new(&format!("{}/Versions", sc2_path))
        .read_dir()
        .expect("Can't read `Versions` folder")
        .filter_map(|dir| {
            let dir = dir.unwrap();
            dir.file_type().ok().filter(|ftype| ftype.is_dir()).and(
                dir.file_name()
                    .to_str()
                    .filter(|name| name.starts_with("Base"))
                    .map(|name| name[4..].parse::<u32>().unwrap()),
            )
        })
        .max()
        .unwrap()
}

/// Convenience wrapper without timeout. Kept as public API.
#[allow(dead_code)]
pub fn connect_websocket(host: &str, port: i32) -> Result<Api, Sc2Error> {
    connect_websocket_with_optional_timeout(host, port, None)
}

pub fn connect_websocket_with_timeout_secs(
    host: &str,
    port: i32,
    timeout_secs: u64,
) -> Result<Api, Sc2Error> {
    connect_websocket_with_optional_timeout(host, port, Some(Duration::from_secs(timeout_secs)))
}

fn connect_websocket_with_optional_timeout(
    host: &str,
    port: i32,
    timeout: Option<Duration>,
) -> Result<Api, Sc2Error> {
    let url = format!("ws://{}:{}/sc2api", host, port);
    let addrs = (host, port as u16)
        .to_socket_addrs()?
        .collect::<Vec<std::net::SocketAddr>>();

    if addrs.is_empty() {
        return Err(Sc2Error::Protocol(format!(
            "unable to resolve websocket address for {}:{}",
            host, port
        )));
    }

    let deadline = timeout.map(|t| Instant::now() + t);
    let mut last_error = String::new();

    loop {
        if let Some(deadline) = deadline
            && Instant::now() >= deadline
        {
            return Err(Sc2Error::Protocol(match timeout {
                Some(timeout) => {
                    format!(
                        "timed out connecting websocket to {} after {:?}: {}",
                        url, timeout, last_error
                    )
                }
                None => format!("failed connecting websocket to {}: {}", url, last_error),
            }));
        }

        for addr in &addrs {
            let connect_timeout = match deadline {
                Some(deadline) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        break;
                    }
                    remaining.min(Duration::from_secs(2))
                }
                None => Duration::from_secs(2),
            };

            match TcpStream::connect_timeout(addr, connect_timeout) {
                Ok(stream) => {
                    let _ = stream.set_nodelay(true);
                    match client(&url, tungstenite::stream::MaybeTlsStream::Plain(stream)) {
                        Ok((ws, _)) => return Ok(Api { ws }),
                        Err(HandshakeError::Failure(err)) => {
                            last_error = format!("websocket handshake failed: {err}");
                        }
                        Err(HandshakeError::Interrupted(_)) => {
                            last_error = "websocket handshake interrupted".to_string();
                        }
                    }
                }
                Err(err) => {
                    last_error = format!("tcp connect to {addr} failed: {err}");
                }
            }
        }

        thread::sleep(Duration::from_millis(200));
    }
}

/// Generic replay starter. `start_replay_for_analysis` is the variant used
/// by the analyzer binary; this one is kept as public API.
#[allow(dead_code)]
pub fn start_replay(
    api: &mut Api,
    replay_path: &str,
    observed_player_id: i32,
    realtime: bool,
) -> Result<(), Sc2Error> {
    let mut req = Request::new();
    let req_start_replay = req.mut_start_replay();

    req_start_replay.set_replay_path(normalize_replay_path_for_sc2(replay_path));
    req_start_replay.set_observed_player_id(observed_player_id);
    req_start_replay.set_realtime(realtime);

    let options = req_start_replay.mut_options();
    options.set_raw(true);
    options.set_score(true);
    options.set_show_cloaked(true);
    options.set_show_burrowed_shadows(true);
    options.set_show_placeholders(false);
    options.set_raw_affects_selection(false);
    options.set_raw_crop_to_playable_area(true);

    let res = api.send(req)?;
    let res_start_replay = res.get_start_replay();
    if res_start_replay.has_error() {
        return Err(Sc2Error::Protocol(format!(
            "failed to start replay {:?}: {}",
            res_start_replay.get_error(),
            res_start_replay.get_error_details()
        )));
    }

    Ok(())
}

#[cfg(target_os = "linux")]
fn normalize_replay_path_for_sc2(replay_path: &str) -> String {
    let path = Path::new(replay_path);
    if !path.is_absolute() {
        return replay_path.to_string();
    }

    if let Ok(compat_data_path) = std::env::var("STEAM_COMPAT_DATA_PATH") {
        let drive_c_prefix = format!("{compat_data_path}/pfx/drive_c/");
        if replay_path.starts_with(&drive_c_prefix) {
            let rest = replay_path[drive_c_prefix.len()..].replace('/', "\\");
            return format!("C:\\{rest}");
        }
    }

    format!("Z:{}", replay_path.replace('/', "\\"))
}

#[cfg(not(target_os = "linux"))]
fn normalize_replay_path_for_sc2(replay_path: &str) -> String {
    replay_path.to_string()
}

/// Start a replay with raw + score observation enabled and the fog removed, so
/// every unit (including the opponent's) is visible for post-hoc analysis.
pub fn start_replay_for_analysis(
    api: &mut Api,
    replay_path: &str,
    observed_player_id: i32,
) -> Result<(), Sc2Error> {
    let mut req = Request::new();
    let req_start_replay = req.mut_start_replay();

    req_start_replay.set_replay_path(normalize_replay_path_for_sc2(replay_path));
    req_start_replay.set_observed_player_id(observed_player_id);
    req_start_replay.set_realtime(false);
    req_start_replay.set_disable_fog(true);

    let options = req_start_replay.mut_options();
    options.set_raw(true);
    options.set_score(true);
    options.set_show_cloaked(true);
    options.set_show_burrowed_shadows(true);
    options.set_show_placeholders(false);
    options.set_raw_affects_selection(false);
    options.set_raw_crop_to_playable_area(true);

    let res = api.send(req)?;
    let res_start_replay = res.get_start_replay();
    if res_start_replay.has_error() {
        return Err(Sc2Error::Protocol(format!(
            "failed to start replay {:?}: {}",
            res_start_replay.get_error(),
            res_start_replay.get_error_details()
        )));
    }

    Ok(())
}

pub fn observe(api: &mut Api, disable_fog: bool) -> Result<sc2api::Response, Sc2Error> {
    let mut req = Request::new();
    req.mut_observation().set_disable_fog(disable_fog);
    api.send(req)
}

pub fn step_replay(api: &mut Api, count: u32) -> Result<(), Sc2Error> {
    let mut req = Request::new();
    req.mut_step().set_count(count);
    api.send(req)?;
    Ok(())
}

/// Request the game data (unit types, abilities, upgrades) as a raw response.
pub fn request_game_data(api: &mut Api) -> Result<sc2api::Response, Sc2Error> {
    let mut req = Request::new();
    req.mut_data().set_unit_type_id(true);
    req.mut_data().set_ability_id(true);
    req.mut_data().set_upgrade_id(true);
    api.send(req)
}

pub fn ping(api: &mut Api) -> Result<sc2api::Response, Sc2Error> {
    let mut req = Request::new();
    req.mut_ping();
    api.send(req)
}

pub fn replay_info(
    api: &mut Api,
    replay_path: &str,
) -> Result<sc2api::ResponseReplayInfo, Sc2Error> {
    let mut req = Request::new();
    let replay_info = req.mut_replay_info();
    replay_info.set_replay_path(normalize_replay_path_for_sc2(replay_path));
    replay_info.set_download_data(false);
    let mut res = api.send(req)?;
    Ok(res.take_replay_info())
}

pub fn leave_current_game(api: &mut Api) -> Result<(), Sc2Error> {
    let mut req = Request::new();
    req.mut_leave_game();
    api.send(req)?;
    Ok(())
}

pub fn quit_client(api: &mut Api) -> Result<(), Sc2Error> {
    let mut req = Request::new();
    req.mut_quit();
    api.send(req)?;
    Ok(())
}
