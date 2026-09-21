//! SC2 client plumbing for replay analysis: websocket API, client launch,
//! dockerised client launch, replay metadata parsing.

pub mod api;
pub mod client;
pub mod docker;
pub mod error;
pub mod point;
pub mod replay;

pub use client::{
    connect_websocket_with_timeout_secs, get_path_to_sc2,
    get_unused_port, init_tracing, launch_sc2_with_data_version, leave_current_game, observe, ping, quit_client, replay_info, request_game_data,
    start_replay_for_analysis, step_replay,
};
pub use point::Point2;
