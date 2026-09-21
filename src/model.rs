use std::collections::BTreeMap;

use serde::Serialize;

pub const METRICS_SCHEMA_VERSION: u32 = 2;
pub const EVENTS_SCHEMA_VERSION: u32 = 3;
pub const SUMMARY_SCHEMA_VERSION: u32 = 3;

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl From<crate::sc2::Point2> for Point {
    fn from(p: crate::sc2::Point2) -> Self {
        Self { x: p.x, y: p.y }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PlayerHeader {
    pub player_id: u32,
    pub name: String,
    pub race: String,
    pub result: Option<String>,
}

// ---------------------------------------------------------------------------
// metrics.jsonl
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(tag = "record_type", rename_all = "snake_case")]
pub enum MetricsRecord {
    Header(MetricsHeader),
    Tick(TickMetrics),
    Footer(MetricsFooter),
}

#[derive(Serialize)]
pub struct MetricsHeader {
    pub schema_version: u32,
    pub map_name: String,
    pub game_version: Option<String>,
    pub data_build: Option<u32>,
    pub base_build: Option<u32>,
    pub observed_player_id: u32,
    pub sample_loops: u32,
    pub players: Vec<PlayerHeader>,
}

#[derive(Serialize)]
pub struct TickMetrics {
    pub tick: u32,
    pub time_seconds: f64,
    pub players: BTreeMap<u32, PlayerMetrics>,
    pub clusters: Vec<ClusterMetrics>,
    pub confrontations: Vec<ConfrontationMetrics>,
}

#[derive(Default, Serialize)]
pub struct PlayerMetrics {
    pub minerals: u32,
    pub vespene: u32,
    pub food_used: u32,
    pub food_cap: u32,
    pub food_army: u32,
    pub food_workers: u32,
    pub expansions: usize,
    pub workers: usize,
    pub income: IncomeMetrics,
    pub army: ArmyMetrics,
    pub production: ProductionMetrics,
    pub killed: DeathMetrics,
    pub lost: DeathMetrics,
    pub supply_by_type: Vec<SupplyByType>,
    pub buildings_by_type: Vec<BuildingByType>,
}

#[derive(Default, Serialize)]
pub struct IncomeMetrics {
    pub minerals_per_min: f32,
    pub vespene_per_min: f32,
    pub estimated: bool,
}

#[derive(Default, Serialize)]
pub struct ArmyMetrics {
    pub combat_supply: f32,
    pub support_supply: f32,
    pub combat_value: f32,
    pub support_value: f32,
    pub air_supply: f32,
    pub stealth_supply: f32,
    pub tier3_supply: f32,
}

#[derive(Default, Serialize)]
pub struct ProductionMetrics {
    pub structures: usize,
    pub producing: usize,
    pub utilisation: f32,
    pub tiers: BTreeMap<u8, TierProduction>,
    pub capacity_spend_minerals_per_min: f32,
    pub capacity_spend_vespene_per_min: f32,
    pub produced_minerals_per_min: f32,
    pub produced_vespene_per_min: f32,
}

#[derive(Default, Serialize)]
pub struct TierProduction {
    pub structures: usize,
    pub producing: usize,
    pub capacity_spend_minerals_per_min: f32,
    pub capacity_spend_vespene_per_min: f32,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct DeathMetrics {
    pub units_value: f32,
    pub units_count: usize,
    pub workers_value: f32,
    pub workers_count: usize,
    pub structures_value: f32,
    pub structures_count: usize,
    pub support_value: f32,
    pub support_count: usize,
}

#[derive(Serialize)]
pub struct SupplyByType {
    pub unit_type: String,
    pub count: usize,
    pub supply: f32,
    pub value: f32,
}

/// Per-building-type counts for one player at one tick. `count` is everything
/// present (including buildings still under construction); `completed` only
/// finished ones; `under_construction = count - completed`.
#[derive(Serialize)]
pub struct BuildingByType {
    pub unit_type: String,
    pub count: usize,
    pub completed: usize,
    pub under_construction: usize,
    pub value: f32,
}

#[derive(Serialize)]
pub struct ClusterMetrics {
    pub player: u32,
    pub centroid: Point,
    pub supply: f32,
    pub value: f32,
    pub hits: f32,
    pub dps: f32,
    pub count: usize,
    pub air_supply: f32,
    pub units: Vec<ClusterUnit>,
}

#[derive(Serialize)]
pub struct ClusterUnit {
    pub unit_type: String,
    pub count: usize,
    pub supply: f32,
}

#[derive(Serialize)]
pub struct ConfrontationMetrics {
    pub our_player: u32,
    pub enemy_player: u32,
    pub our_supply: f32,
    pub enemy_supply: f32,
    pub our_value: f32,
    pub enemy_value: f32,
    pub our_score: f32,
    pub enemy_score: f32,
    pub distance: f32,
}

#[derive(Serialize)]
pub struct MetricsFooter {
    pub tick: u32,
    pub time_seconds: f64,
    pub result: Option<String>,
    pub frames_analyzed: usize,
}

// ---------------------------------------------------------------------------
// events.jsonl
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(tag = "record_type", rename_all = "snake_case")]
pub enum EventRecord {
    Header(EventHeader),
    CompositionTransition(CompositionTransition),
    ProductionTransition(ProductionTransition),
    StructureTransition(StructureTransition),
    UpgradeStarted(UpgradeTransition),
    UpgradeResearched(UpgradeTransition),
    ExpansionGained(ExpansionEvent),
    ExpansionLost(ExpansionEvent),
    EngagementStart(EngagementStart),
    EngagementEnd(EngagementEnd),
    Footer(EventFooter),
}

#[derive(Serialize)]
pub struct EventHeader {
    pub schema_version: u32,
}

#[derive(Serialize)]
pub struct CompositionTransition {
    pub tick: u32,
    pub time_seconds: f64,
    pub player: u32,
    pub category: String,
    pub share_before: f32,
    pub share_after: f32,
}

#[derive(Serialize)]
pub struct ProductionTransition {
    pub tick: u32,
    pub time_seconds: f64,
    pub player: u32,
    pub structure: String,
    pub tier: u8,
    pub count: usize,
}

/// A non-production structure (forge, bay, tech building, cannon...) whose
/// *completed* count changed. `change` is "gained" or "lost" (destroyed /
/// cancelled); `completed` is the new total.
#[derive(Serialize)]
pub struct StructureTransition {
    pub tick: u32,
    pub time_seconds: f64,
    pub player: u32,
    pub structure: String,
    pub completed: usize,
    pub change: String,
}

/// Research on an upgrade, keyed by the upgrade's ability id (as carried by the
/// researching structure's order). `UpgradeStarted` fires when research begins,
/// `UpgradeResearched` when it completes.
#[derive(Serialize)]
pub struct UpgradeTransition {
    pub tick: u32,
    pub time_seconds: f64,
    pub player: u32,
    pub ability_id: u32,
    pub name: String,
}

#[derive(Serialize)]
pub struct ExpansionEvent {
    pub tick: u32,
    pub time_seconds: f64,
    pub player: u32,
    pub expansions: usize,
    pub location: Option<Point>,
}

#[derive(Serialize)]
pub struct EngagementStart {
    pub id: u64,
    pub tick: u32,
    pub time_seconds: f64,
    pub kind: String,
    pub location: Point,
    pub base: Option<String>,
    pub sides: Vec<EngagementSide>,
}

#[derive(Serialize)]
pub struct EngagementEnd {
    pub id: u64,
    pub tick: u32,
    pub time_seconds: f64,
    pub duration_seconds: f64,
    pub location: Point,
    pub peak_supply: Vec<EngagementPeak>,
    pub deaths: Vec<EngagementDeaths>,
    pub outcome: Option<String>,
}

#[derive(Serialize)]
pub struct EngagementSide {
    pub player: u32,
    pub supply: f32,
    pub value: f32,
    pub count: usize,
    pub units: Vec<ClusterUnit>,
}

#[derive(Serialize)]
pub struct EngagementPeak {
    pub player: u32,
    pub supply: f32,
    pub value: f32,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct EngagementDeaths {
    pub player: u32,
    pub killed_value: f32,
    pub lost_value: f32,
    pub lost_units: usize,
}

#[derive(Serialize)]
pub struct EventFooter {
    pub tick: u32,
    pub time_seconds: f64,
}

// ---------------------------------------------------------------------------
// summary.json
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct Summary {
    pub schema_version: u32,
    pub replay_path: String,
    pub map_name: String,
    pub game_version: Option<String>,
    pub base_build: Option<u32>,
    pub data_version: Option<String>,
    pub duration_loops: u32,
    pub observed_player_id: u32,
    pub players: Vec<PlayerHeader>,
    pub result: Option<String>,
    pub engagements: Vec<EngagementSummary>,
    pub transitions: Vec<TransitionSummary>,
    pub upgrades: Vec<UpgradeSummary>,
    pub player_stats: BTreeMap<u32, PlayerStats>,
}

#[derive(Serialize)]
pub struct EngagementSummary {
    pub id: u64,
    pub start_tick: u32,
    pub end_tick: u32,
    pub start_seconds: f64,
    pub end_seconds: f64,
    pub kind: String,
    pub location: Point,
    pub base: Option<String>,
    pub sides: Vec<EngagementSide>,
    pub deaths: Vec<EngagementDeaths>,
    pub outcome: Option<String>,
}

#[derive(Serialize)]
pub struct TransitionSummary {
    pub tick: u32,
    pub time_seconds: f64,
    pub player: u32,
    pub kind: String,
    pub detail: String,
}

/// A completed upgrade research, grouped by player in the summary.
#[derive(Serialize)]
pub struct UpgradeSummary {
    pub player: u32,
    pub name: String,
    pub tick: u32,
    pub time_seconds: f64,
}

#[derive(Serialize)]
pub struct PlayerStats {
    pub final_expansions: usize,
    pub peak_expansions: usize,
    pub peak_workers: usize,
    pub peak_army_supply: f32,
    pub peak_army_value: f32,
    pub total_killed_value: f32,
    pub total_lost_value: f32,
    pub worker_kills_value: f32,
    pub worker_losses_value: f32,
}

impl Default for PlayerStats {
    fn default() -> Self {
        Self {
            final_expansions: 0,
            peak_expansions: 0,
            peak_workers: 0,
            peak_army_supply: 0.0,
            peak_army_value: 0.0,
            total_killed_value: 0.0,
            total_lost_value: 0.0,
            worker_kills_value: 0.0,
            worker_losses_value: 0.0,
        }
    }
}
