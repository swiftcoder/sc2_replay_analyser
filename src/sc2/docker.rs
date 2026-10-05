//! Launch the dockerised Linux SC2 client (the same image the ladder runs) so
//! replays can be played back headless for analysis, instead of relying on the
//! local Mac/PC client.

use std::net::TcpStream;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

/// Default docker image with a Linux SC2 client. On Apple Silicon it runs under
/// emulation and is slow, but works.
/// (Mirrored as clap `default_value` in main.rs; kept here as the canonical constant.)
#[allow(dead_code)]
pub const DEFAULT_DOCKER_IMAGE: &str = "aiarena/arenaclient-sc2:v0.8.0";

/// Default container path to the SC2 install; the maps dir is mounted here.
/// (Mirrored as clap `default_value` in main.rs; kept here as the canonical constant.)
#[allow(dead_code)]
pub const DEFAULT_CONTAINER_SC2_PATH: &str = "/root/StarCraftII";

/// Default container path where the host maps dir is mounted.
pub const DEFAULT_CONTAINER_MAPS_DIR: &str = "/root/StarCraftII/maps";

/// Container path the alignmutex LD_PRELOAD library is mounted at when the
/// Apple Silicon Rosetta fix is enabled (see apple-silicon/alignmutex.c).
pub const CONTAINER_ALIGNMUTEX_PATH: &str = "/opt/alignmutex.so";

#[derive(Clone)]
pub struct DockerConfig {
    /// Docker image containing the Linux SC2 client.
    pub image: String,
    /// Host directory of ladder maps, mounted into the container's maps folder.
    pub maps_dir: PathBuf,
    /// Host port the container's SC2 websocket is published on.
    pub host_port: u16,
    /// SC2 websocket port inside the container.
    pub sc2_port: u16,
    /// Path to the SC2 installation inside the container.
    pub container_sc2_path: String,
    /// Container path where `maps_dir` is mounted.
    pub container_maps_dir: String,
    /// Override the shell command that launches SC2 inside the container.
    pub container_launch: Option<String>,
    pub base_build: Option<u32>,
    pub data_version: Option<String>,
    /// Replay file to also mount into the container (replay analysis only).
    pub replay_host_path: Option<PathBuf>,
    /// Container dir where the replay file is mounted (replay analysis only).
    pub container_replay_dir: Option<String>,
    /// Use host networking instead of -p port mapping. Required for BotVsBot
    /// host_game where SC2 peer ports (5002-5005) must be reachable between
    /// the two containers and the ladder bots on 127.0.0.1.
    /// On macOS host networking is not supported, so we fall back to explicit
    /// -p mappings for the peer ports.
    pub host_network: bool,
    /// Extra host->container port mappings for SC2 peer ports (5002-5005).
    /// Only used when host_network is false.
    pub extra_ports: Vec<u16>,
    /// Host path to a prebuilt alignmutex.so (Apple Silicon Rosetta fix).
    /// When set, the file is mounted read-only at CONTAINER_ALIGNMUTEX_PATH
    /// and LD_PRELOAD is pointed at it so SC2's misaligned pthread mutexes
    /// are routed to aligned shadows. None = stock behaviour (QEMU-safe).
    pub alignmutex_host_path: Option<PathBuf>,
    /// Duplicate the SC2 exe path as a sacrificial first argument.
    /// OrbStack's Rosetta drops argv[0] from /proc/self/cmdline, which SC2
    /// parses as its command line (treating the first entry as the program
    /// name). Without this SC2 skips "-listen" and dies at startup with
    /// "Missing -listen command line" + signal 11. Only needed under
    /// Rosetta setups with this cmdline bug (observed on OrbStack; the
    /// colima setup in aiarena/local-play-bootstrap#22 reaches map load
    /// without it). Must be false under QEMU, where cmdline is intact and
    /// the extra arg would break parsing instead.
    pub rosetta_argv_workaround: bool,
}

impl DockerConfig {
    /// Path to the replay as SC2 inside the container will see it.
    pub fn container_replay_path(&self) -> String {
        let file_name = self
            .replay_host_path
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("replay.SC2Replay");
        let dir = self
            .container_replay_dir
            .as_deref()
            .unwrap_or("/replays")
            .trim_end_matches('/');
        format!("{dir}/{file_name}")
    }

    /// Path to a ladder map as SC2 inside the container will see it.
    #[allow(dead_code)]
    pub fn container_map_path(&self, map_name: &str) -> String {
        let dir = self.container_maps_dir.trim_end_matches('/');
        format!("{dir}/{map_name}.SC2Map")
    }

    /// Clone this config with a different host port, for spinning up extra
    /// container clients (e.g. the second instance in a bot-vs-bot host game).
    #[allow(dead_code)]
    pub fn with_host_port(&self, host_port: u16) -> Self {
        let mut config = self.clone();
        config.host_port = host_port;
        config
    }
}

fn default_launch(config: &DockerConfig) -> String {
    let sc2 = &config.container_sc2_path;
    let port = config.sc2_port;
    let data_version_arg = config
        .data_version
        .as_ref()
        .filter(|v| !v.is_empty())
        .map(|v| format!("-dataVersion {v}"))
        .unwrap_or_default();
    let base_dir = config.base_build.map(|b| format!("Base{b}")).unwrap_or_default();

    // Mirrors how the aiarena sc2_controller launches SC2: native Linux
    // binary, headless (no X server required), data dir pointed at the
    // StarCraft II install. The listen address embeds the port
    // (`0.0.0.0:<port>`) because this SC2 build reliably parses that form.
    // With rosetta_argv_workaround the exe path is repeated as a sacrificial
    // argv[1]: Rosetta drops argv[0] from /proc/self/cmdline (which SC2
    // parses, skipping entry 0 as the program name), so without padding SC2
    // never sees "-listen".
    let exe_arg = if config.rosetta_argv_workaround {
        "\"$D/SC2_x64\" "
    } else {
        ""
    };
    format!(
        "D=; if [ -n \"{base_dir}\" ] && [ -d {sc2}/Versions/{base_dir} ]; then D={sc2}/Versions/{base_dir}; fi; \
         [ -z \"$D\" ] && D=$(ls -d {sc2}/Versions/Base* 2>/dev/null | sort -V | tail -1); \
         mkdir -p /tmp/sc2-game; cd {sc2}; \
         exec \"$D/SC2_x64\" {exe_arg}-listen 0.0.0.0:{port} -dataDir {sc2} -displayMode 0 -tempDir /tmp/sc2-game {data_version_arg}",
    )
}

pub struct DockerSc2 {
    name: String,
    launched: bool,
}

impl DockerSc2 {
    pub fn launch(config: &DockerConfig) -> Result<Self> {
        let name = format!("sc2replay-{}-{}", std::process::id(), config.host_port);

        let check = Command::new("docker").arg("version").output();
        match check {
            Ok(out) if out.status.success() => {}
            Ok(out) => {
                bail!(
                    "docker is not usable (status {}): {}",
                    out.status,
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            Err(err) => bail!("failed to run docker: {err}"),
        }

        // Clean up any stale container from a previous run.
        let _ = Command::new("docker").args(["rm", "-f", &name]).output();

        let launch = config
            .container_launch
            .clone()
            .unwrap_or_else(|| default_launch(config));

        let mut cmd = Command::new("docker");
        cmd.args(["run", "-d", "--rm", "--name", &name]);
        // For BotVsBot host_game the two SC2 peers must reach each other on
        // 127.0.0.1:5002-5005. With bridge networking 127.0.0.1 inside one
        // container is not the host's 127.0.0.1, so P2P hangs (GenesisLotus
        // JoinGame timeout).  OrbStack on macOS does support --network host
        // (Docker Desktop does not) — try it first; the caller can still ask
        // for bridge by leaving host_network=false.
        let use_host_network = config.host_network;
        if use_host_network {
            cmd.args(["--network", "host"]);
        } else {
            cmd.args([
                "-p",
                &format!("127.0.0.1:{}:{}", config.host_port, config.sc2_port),
            ]);
            for p in &config.extra_ports {
                cmd.args([
                    "-p",
                    &format!("127.0.0.1:{}:{}", p, p),
                ]);
            }
        }
        cmd.args(["-v"])
            .arg(format!(
                "{}:{}:ro",
                config.maps_dir.display(),
                config.container_maps_dir
            ));

        if let (Some(replay), Some(dir)) =
            (&config.replay_host_path, &config.container_replay_dir)
        {
            let replay_dir = replay.parent().unwrap_or_else(|| std::path::Path::new("."));
            cmd.args(["-v"])
                .arg(format!(
                    "{}:{}/:ro",
                    replay_dir.display(),
                    dir.trim_end_matches('/')
                ));
        }

        // Apple Silicon Rosetta fix: preload alignmutex.so into the SC2
        // container so misaligned pthread mutexes don't SIGBUS under Rosetta.
        // Mirrors docker-compose-apple-silicon.yml from
        // aiarena/local-play-bootstrap#22 (LD_PRELOAD + read-only mount),
        // adapted to `docker run` since this tool doesn't use compose.
        if let Some(so_path) = &config.alignmutex_host_path {
            let abs = std::fs::canonicalize(so_path).with_context(|| {
                format!("resolving --alignmutex-so {}", so_path.display())
            })?;
            if !abs.is_file() {
                bail!("--alignmutex-so is not a file: {}", abs.display());
            }
            cmd.args(["-v"]).arg(format!(
                "{}:{}:ro",
                abs.display(),
                CONTAINER_ALIGNMUTEX_PATH
            ));
            cmd.args(["-e"]).arg(format!(
                "LD_PRELOAD={}",
                CONTAINER_ALIGNMUTEX_PATH
            ));
            tracing::info!(
                host_path = %abs.display(),
                container_path = CONTAINER_ALIGNMUTEX_PATH,
                "preloading alignmutex.so into SC2 container (Rosetta fix)"
            );
        }

        cmd.args(["--entrypoint", "/bin/bash"])
            .arg(&config.image)
            .arg("-c")
            .arg(&launch);

        tracing::info!(
            image = %config.image,
            host_port = config.host_port,
            sc2_port = config.sc2_port,
            "starting SC2 client container"
        );

        let output = cmd.output().context("failed to run `docker run`")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            bail!(
                "docker run failed (status {}):\nstdout: {}\nstderr: {}",
                output.status,
                stdout.trim(),
                stderr.trim()
            );
        }

        tracing::debug!(command = ?cmd, launch = %launch, "docker run command");

        let container_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
        tracing::info!(container_id, "SC2 client container started");

        wait_for_port(config.host_port, Duration::from_secs(180))
            .with_context(|| format!("SC2 in container never opened port {}", config.host_port))?;

        Ok(Self { name, launched: true })
    }
}

impl Drop for DockerSc2 {
    fn drop(&mut self) {
        if self.launched {
            let _ = Command::new("docker").args(["rm", "-f", &self.name]).output();
        }
    }
}

fn wait_for_port(port: u16, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for port {port}");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}
