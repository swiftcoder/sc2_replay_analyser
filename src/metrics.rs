use std::collections::{BTreeMap, HashMap, HashSet};

use crate::sc2::Point2;
use sc2_proto::sc2api::Observation;

use crate::{
    categories::{UnitClass, UnitClassifier},
    clusters::{
        Cluster, ClusterConfig, EngagementTracker, TownhallRef, cluster_combat_units,
        confrontations,
    },
    game_data::{UnitTable, UpgradeTable},
    model::{
        ArmyMetrics, BuildingByType, ClusterMetrics, CompositionTransition, ConfrontationMetrics,
        DeathMetrics, EventRecord, ExpansionEvent, IncomeMetrics, PlayerMetrics, Point,
        ProductionMetrics, ProductionTransition, StructureTransition, SupplyByType, TickMetrics,
        TransitionSummary, UpgradeSummary, UpgradeTransition,
    },
};

pub const GAME_LOOPS_PER_SECOND: f64 = 22.4;

pub const COMPOSITION_AIR_THRESHOLD: f32 = 0.35;
pub const COMPOSITION_AIR_FLOOR: f32 = 0.25;
pub const STEALTH_SUPPLY_THRESHOLD: f32 = 4.0;
pub const TIER3_SUPPLY_THRESHOLD: f32 = 6.0;

const WORKER_BASE_RADIUS: f32 = 12.0;
const GAS_WORKER_RADIUS: f32 = 6.0;
const MINERAL_WORKER_RATE: f32 = 44.0;
const WORKER_SATURATION: usize = 16;
const GAS_WORKER_RATE: f32 = 42.0;
const GAS_PER_EXTRACTOR: usize = 3;

#[derive(Debug, Clone)]
pub struct FrameUnit {
    pub tag: u64,
    pub unit_type: u32,
    pub owner: u32,
    pub pos: Point2,
    pub is_active: bool,
    pub is_hallucination: bool,
    pub hits: f32,
    /// 0..1 construction progress; structures under construction are < 1.
    pub build_progress: f32,
    pub add_on_tag: Option<u64>,
    /// Outstanding orders as (ability_id, progress). Used to spot research.
    pub orders: Vec<(u32, f32)>,
    /// Kept from the SC2 observation for future metrics (e.g. DPS uptime).
    #[allow(dead_code)]
    pub weapon_cooldown: f32,
}

/// Raw persistent effects from the SC2 observation. Decoded for future
/// metrics; not yet consumed by the analyzer.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct EffectInfo {
    pub effect_id: u32,
    pub pos: Point2,
    pub radius: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct PlayerCommonSnapshot {
    pub minerals: u32,
    pub vespene: u32,
    pub food_used: u32,
    pub food_cap: u32,
    pub food_army: u32,
    pub food_workers: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct ScoreSnapshot {
    pub collection_rate_minerals: f32,
    pub collection_rate_vespene: f32,
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub tick: u32,
    pub units: Vec<FrameUnit>,
    pub dead: Vec<u64>,
    pub player_common: Option<PlayerCommonSnapshot>,
    pub score: Option<ScoreSnapshot>,
    #[allow(dead_code)]
    pub effects: Vec<EffectInfo>,
}

pub fn decode_observation(observation: &Observation) -> Frame {
    let tick = observation.get_game_loop();

    let common = if observation.has_player_common() {
        let pc = observation.get_player_common();
        Some(PlayerCommonSnapshot {
            minerals: pc.get_minerals(),
            vespene: pc.get_vespene(),
            food_used: pc.get_food_used(),
            food_cap: pc.get_food_cap(),
            food_army: pc.get_food_army(),
            food_workers: pc.get_food_workers(),
        })
    } else {
        None
    };

    let score = if observation.has_score() {
        let s = observation.get_score();
        let details = s.get_score_details();
        Some(ScoreSnapshot {
            collection_rate_minerals: details.get_collection_rate_minerals(),
            collection_rate_vespene: details.get_collection_rate_vespene(),
        })
    } else {
        None
    };

    let mut units = Vec::new();
    let mut dead = Vec::new();

    let raw = observation.get_raw_data();
    for unit in raw.get_units() {
        let owner = unit.get_owner();
        if owner != 1 && owner != 2 {
            continue;
        }
        let add_on_tag = {
            let tag = unit.get_add_on_tag();
            if tag > 0 {
                Some(tag)
            } else {
                None
            }
        };
        let mut orders = Vec::new();
        for order in unit.get_orders() {
            let ability_id = order.get_ability_id();
            if ability_id > 0 {
                orders.push((ability_id, order.get_progress()));
            }
        }
        units.push(FrameUnit {
            tag: unit.get_tag(),
            unit_type: unit.get_unit_type(),
            owner: owner as u32,
            pos: Point2::new(unit.get_pos().get_x(), unit.get_pos().get_y()),
            is_active: unit.get_is_active(),
            is_hallucination: unit.get_is_hallucination(),
            hits: unit.get_health() + unit.get_shield(),
            build_progress: unit.get_build_progress(),
            add_on_tag,
            orders,
            weapon_cooldown: unit.get_weapon_cooldown(),
        });
    }
    dead.extend(raw.get_event().get_dead_units().iter().copied());

    let mut effects = Vec::new();
    for eff in raw.get_effects() {
        let eid = eff.get_effect_id();
        let radius = eff.get_radius();
        for pos in eff.get_pos() {
            effects.push(EffectInfo {
                effect_id: eid,
                pos: Point2::new(pos.get_x(), pos.get_y()),
                radius,
            });
        }
    }

    Frame {
        tick,
        units,
        dead,
        player_common: common,
        score,
        effects,
    }
}

#[derive(Debug, Clone)]
struct DeathRecord {
    owner: u32,
    tag: u64,
    value: f32,
    is_worker: bool,
    is_structure: bool,
    is_support: bool,
}

impl DeathRecord {
    fn from_unit(owner: u32, tag: u64, unit_type: u32, table: &UnitTable) -> Self {
        let info = table.get(unit_type);
        let name = info.map(|i| i.name.as_str()).unwrap_or("");
        Self {
            owner,
            tag,
            value: info.map(|i| i.value()).unwrap_or(0.0),
            is_worker: UnitClassifier::is_worker(name),
            is_structure: UnitClassifier::is_structure_name(name),
            is_support: UnitClassifier::class(name) == UnitClass::Support,
        }
    }
}

impl DeathMetrics {
    fn record(&mut self, death: &DeathRecord) {
        if death.is_structure {
            self.structures_value += death.value;
            self.structures_count += 1;
        } else if death.is_worker {
            self.workers_value += death.value;
            self.workers_count += 1;
        } else if death.is_support {
            self.support_value += death.value;
            self.support_count += 1;
        } else {
            self.units_value += death.value;
            self.units_count += 1;
        }
    }
}

#[derive(Debug, Default)]
struct EconomyModel {
    bases: Vec<(Point2, usize)>,
    extractors: Vec<(Point2, usize)>,
}

impl EconomyModel {
    fn build(units: &[&FrameUnit], table: &UnitTable) -> Self {
        let mut townhalls: Vec<&FrameUnit> = Vec::new();
        let mut extractors: Vec<&FrameUnit> = Vec::new();
        let mut workers: Vec<&FrameUnit> = Vec::new();

        for unit in units {
            let name = table.name_of(unit.unit_type);
            if UnitClassifier::is_townhall(&name) {
                townhalls.push(unit);
            } else if UnitClassifier::structure_kind(&name) == crate::categories::StructureKind::Economy
            {
                extractors.push(unit);
            } else if UnitClassifier::is_worker(&name) {
                workers.push(unit);
            }
        }

        let mut bases = Vec::new();
        for th in &townhalls {
            let count = workers
                .iter()
                .filter(|w| w.pos.distance_squared(th.pos) <= WORKER_BASE_RADIUS * WORKER_BASE_RADIUS)
                .count();
            bases.push((th.pos, count));
        }

        let mut extractor_workers = Vec::new();
        for ex in &extractors {
            let count = workers
                .iter()
                .filter(|w| w.pos.distance_squared(ex.pos) <= GAS_WORKER_RADIUS * GAS_WORKER_RADIUS)
                .count();
            extractor_workers.push((ex.pos, count));
        }

        Self {
            bases,
            extractors: extractor_workers,
        }
    }

    fn minerals_per_min(&self) -> f32 {
        self.bases
            .iter()
            .map(|(_, workers)| {
                let saturated = (*workers).min(WORKER_SATURATION) as f32 * MINERAL_WORKER_RATE;
                let over = workers.saturating_sub(WORKER_SATURATION) as f32
                    * (MINERAL_WORKER_RATE / 2.0);
                saturated + over
            })
            .sum()
    }

    fn vespene_per_min(&self) -> f32 {
        self.extractors
            .iter()
            .map(|(_, workers)| (*workers).min(GAS_PER_EXTRACTOR) as f32 * GAS_WORKER_RATE)
            .sum()
    }
}

/// Max (minerals_per_second, vespene_per_second) a structure can spend building
/// its best unit, scaled to per minute.
fn production_spend_rate(structure: &str) -> (f32, f32) {

    let units: &[(&str, u32, u32, f32)] = match structure {
        "Barracks" => &[
            ("Marine", 50, 0, 25.0),
            ("Marauder", 100, 25, 30.0),
            ("Reaper", 50, 50, 45.0),
            ("Ghost", 150, 125, 40.0),
        ],
        "Factory" => &[
            ("Hellion", 100, 0, 30.0),
            ("WidowMine", 75, 25, 32.0),
            ("SiegeTank", 150, 125, 40.0),
            ("Thor", 300, 200, 60.0),
        ],
        "Starport" => &[
            ("Medivac", 100, 100, 36.0),
            ("Viking", 150, 75, 42.0),
            ("Liberator", 150, 150, 36.0),
            ("Banshee", 150, 100, 60.0),
            ("Raven", 100, 200, 60.0),
            ("Battlecruiser", 400, 300, 90.0),
        ],
        "Gateway" => &[
            ("Zealot", 100, 0, 27.0),
            ("Stalker", 125, 50, 37.0),
            ("Sentry", 50, 100, 37.0),
            ("Adept", 100, 25, 32.0),
            ("DarkTemplar", 125, 125, 38.0),
            ("HighTemplar", 50, 150, 38.0),
        ],
        "WarpGate" => &[
            ("Zealot", 100, 0, 20.0),
            ("Stalker", 125, 50, 20.0),
            ("Sentry", 50, 100, 20.0),
            ("Adept", 100, 25, 20.0),
            ("DarkTemplar", 125, 125, 20.0),
            ("HighTemplar", 50, 150, 20.0),
        ],
        "RoboticsFacility" => &[
            ("Observer", 25, 75, 21.0),
            ("WarpPrism", 100, 0, 36.0),
            ("Immortal", 275, 100, 55.0),
            ("Colossus", 300, 200, 75.0),
            ("Disruptor", 150, 150, 60.0),
        ],
        "Stargate" => &[
            ("Phoenix", 150, 100, 35.0),
            ("VoidRay", 150, 150, 45.0),
            ("Oracle", 150, 150, 45.0),
            ("Carrier", 350, 250, 120.0),
            ("Tempest", 300, 200, 80.0),
        ],
        "SpawningPool" => &[("Zergling", 50, 0, 17.0)],
        "RoachWarren" => &[("Roach", 75, 25, 27.0), ("Ravager", 100, 50, 45.0)],
        "BanelingNest" => &[("Baneling", 25, 25, 14.0)],
        "HydraliskDen" => &[("Hydralisk", 100, 50, 29.0)],
        "LurkerDenMP" => &[("Lurker", 50, 100, 43.0)],
        "InfestationPit" => &[("Infestor", 100, 150, 46.0)],
        "Spire" => &[("Mutalisk", 100, 100, 33.0), ("Corruptor", 150, 100, 38.0)],
        "GreaterSpire" => &[("BroodLord", 150, 150, 34.0)],
        "UltraliskCavern" => &[("Ultralisk", 300, 200, 55.0)],
        _ => return (0.0, 0.0),
    };

    let mut best_minerals = 0.0f32;
    let mut best_vespene = 0.0f32;
    for &(_, mineral, vespene, seconds) in units {
        if seconds > 0.0 {
            best_minerals = best_minerals.max(mineral as f32 / seconds);
            best_vespene = best_vespene.max(vespene as f32 / seconds);
        }
    }
    (best_minerals * 60.0, best_vespene * 60.0)
}

fn is_reactor_addon(name: &str) -> bool {
    matches!(
        name,
        "Reactor" | "BarracksReactor" | "FactoryReactor" | "StarportReactor"
    )
}

/// Progress at which an upgrade order that has vanished counts as researched
/// rather than cancelled. Research takes 25-135 game seconds and we sample at
/// 1 game second, so a genuine completion will always be seen well past this.
const UPGRADE_COMPLETED_PROGRESS: f32 = 0.5;

#[derive(Debug, Default)]
pub struct PlayerStatsState {
    pub killed: DeathMetrics,
    pub lost: DeathMetrics,
    pub peak_expansions: usize,
    pub peak_workers: usize,
    pub peak_army_supply: f32,
    pub peak_army_value: f32,
}

pub struct Analyzer {
    pub observed_player_id: u32,
    last_seen: HashMap<u64, (u32, u32, bool)>,
    calibration: Option<f32>,
    townhall_tags: HashMap<u32, HashMap<u64, Point2>>,
    prev_townhall_count: BTreeMap<u32, usize>,
    air_heavy: BTreeMap<u32, bool>,
    stealth_present: BTreeMap<u32, bool>,
    tier3_present: BTreeMap<u32, bool>,
    prev_structure_counts: BTreeMap<(u32, String), usize>,
    prev_completed_buildings: BTreeMap<(u32, String), usize>,
    prev_tick: Option<u32>,
    engagement: EngagementTracker,
    frames_analyzed: usize,
    pub stats: BTreeMap<u32, PlayerStatsState>,
    transitions: Vec<TransitionSummary>,
    upgrade_seen: BTreeMap<u32, HashSet<u32>>,
    upgrade_progress: BTreeMap<(u32, u32), f32>,
    upgrades: Vec<UpgradeSummary>,
}

impl Analyzer {
    pub fn new(observed_player_id: u32) -> Self {
        Self {
            observed_player_id,
            last_seen: HashMap::new(),
            calibration: None,
            townhall_tags: HashMap::new(),
            prev_townhall_count: BTreeMap::new(),
            air_heavy: BTreeMap::new(),
            stealth_present: BTreeMap::new(),
            tier3_present: BTreeMap::new(),
            prev_structure_counts: BTreeMap::new(),
            prev_completed_buildings: BTreeMap::new(),
            prev_tick: None,
            engagement: EngagementTracker::new(),
            frames_analyzed: 0,
            stats: BTreeMap::new(),
            transitions: Vec::new(),
            upgrade_seen: BTreeMap::new(),
            upgrade_progress: BTreeMap::new(),
            upgrades: Vec::new(),
        }
    }

    pub fn frame_count(&self) -> usize {
        self.frames_analyzed
    }

    pub fn final_expansions(&self, player: u32) -> usize {
        self.prev_townhall_count.get(&player).copied().unwrap_or(0)
    }

    pub fn take_engagement_summaries(&mut self) -> Vec<crate::model::EngagementSummary> {
        std::mem::take(&mut self.engagement.completed)
    }

    pub fn take_transitions(&mut self) -> Vec<TransitionSummary> {
        std::mem::take(&mut self.transitions)
    }

    pub fn take_upgrade_summaries(&mut self) -> Vec<UpgradeSummary> {
        self.upgrades.sort_by_key(|u| (u.player, u.tick));
        std::mem::take(&mut self.upgrades)
    }

    pub fn finish_engagements(&mut self, tick: u32, events: &mut Vec<EventRecord>) {
        self.engagement.finish(tick, events);
    }

    pub fn process_frame(
        &mut self,
        frame: &Frame,
        table: &UnitTable,
        upgrades: &UpgradeTable,
        config: &ClusterConfig,
    ) -> (TickMetrics, Vec<EventRecord>) {
        let mut events = Vec::new();
        let elapsed_seconds = match self.prev_tick {
            Some(prev) if frame.tick >= prev => (frame.tick - prev) as f64 / GAME_LOOPS_PER_SECOND,
            _ => 0.0,
        };
        self.prev_tick = Some(frame.tick);

        // -- deaths --
        let mut death_records: Vec<DeathRecord> = Vec::new();
        for tag in &frame.dead {
            if let Some((owner, unit_type, hallucination)) = self.last_seen.remove(tag)
                && !hallucination {
                    death_records.push(DeathRecord::from_unit(owner, *tag, unit_type, table));
                }
        }

        // -- produced units (non-structures appearing this tick) --
        let mut produced_value: BTreeMap<u32, f32> = BTreeMap::new();
        if self.frames_analyzed > 0 {
            for unit in &frame.units {
                if unit.is_hallucination {
                    continue;
                }
                let name = table.name_of(unit.unit_type);
                let class = UnitClassifier::class(&name);
                if class == UnitClass::Structure || class == UnitClass::Other {
                    continue;
                }
                if !self.last_seen.contains_key(&unit.tag)
                    && let Some(info) = table.get(unit.unit_type) {
                        *produced_value.entry(unit.owner).or_default() += info.value();
                    }
            }
        }

        // -- update last_seen for the current frame --
        //
        // We deliberately keep `last_seen` entries for units that are not in
        // the current frame. Workers inside gas buildings / transports vanish
        // from the raw unit list (no death event) and reappear later; the SC2
        // API's `dead_units` is the authoritative source for deaths, so we only
        // ever record deaths there.
        for unit in &frame.units {
            self.last_seen.insert(
                unit.tag,
                (unit.owner, unit.unit_type, unit.is_hallucination),
            );
        }

        // -- aggregate deaths per player --
        let mut tick_lost: BTreeMap<u32, DeathMetrics> = BTreeMap::new();
        let mut tick_killed: BTreeMap<u32, DeathMetrics> = BTreeMap::new();
        for death in &death_records {
            tick_lost.entry(death.owner).or_default().record(death);
            let other = if death.owner == 1 { 2 } else { 1 };
            tick_killed.entry(other).or_default().record(death);
        }

        // -- per-player units --
        let mut units_by_player: BTreeMap<u32, Vec<&FrameUnit>> = BTreeMap::new();
        for unit in &frame.units {
            units_by_player.entry(unit.owner).or_default().push(unit);
        }

        // -- engagement deaths (owner, tag, value) for tag-based attribution --
        let engagement_deaths: Vec<(u32, u64, f32)> = death_records
            .iter()
            .map(|d| (d.owner, d.tag, d.value))
            .collect();

        let mut player_metrics: BTreeMap<u32, PlayerMetrics> = BTreeMap::new();
        let mut clusters_all: Vec<Cluster> = Vec::new();
        let mut townhalls: Vec<TownhallRef> = Vec::new();
        

        let observed_player_id = self.observed_player_id;
        let is_observed = move |p: u32| p == observed_player_id;

        for player in [1u32, 2u32] {
            let units = units_by_player
                .get(&player).cloned()
                .unwrap_or_default();

            let (mut metrics, economy) = self.compute_player_metrics(
                player,
                &units,
                table,
                upgrades,
                frame,
                &tick_killed,
                &tick_lost,
                produced_value.get(&player).copied().unwrap_or(0.0),
                elapsed_seconds,
                &mut events,
            );
            let is_obs = is_observed(player);

            // income: observed player uses score; enemy uses calibrated model
            let model_min = economy.minerals_per_min();
            let model_gas = economy.vespene_per_min();
            if is_obs {
                let score_available = frame
                    .score
                    .is_some_and(|s| s.collection_rate_minerals > 0.0);
                if score_available {
                    let score = frame.score.unwrap();
                    metrics.income = IncomeMetrics {
                        minerals_per_min: score.collection_rate_minerals,
                        vespene_per_min: score.collection_rate_vespene,
                        estimated: false,
                    };
                    // calibrate the model against the real score
                    if model_min > 1.0 {
                        let ratio = score.collection_rate_minerals / model_min;
                        self.calibration = Some(ratio.clamp(0.3, 3.0));
                    }
                } else {
                    // SC2 did not populate the score; fall back to the model.
                    metrics.income = IncomeMetrics {
                        minerals_per_min: model_min,
                        vespene_per_min: model_gas,
                        estimated: true,
                    };
                }
            } else {
                let calibration = self.calibration.unwrap_or(1.0);
                metrics.income = IncomeMetrics {
                    minerals_per_min: model_min * calibration,
                    vespene_per_min: model_gas * calibration,
                    estimated: true,
                };
            }

            self.update_stats(player, &metrics);
            player_metrics.insert(player, metrics);

            for (pos, _) in &economy.bases {
                townhalls.push(TownhallRef {
                    player,
                    name: table.name_of(
                        units
                            .iter()
                            .find(|u| {
                                let name = table.name_of(u.unit_type);
                                UnitClassifier::is_townhall(&name)
                            })
                            .map(|u| u.unit_type)
                            .unwrap_or(0),
                    ),
                    pos: *pos,
                });
            }

            // -- clusters --
            let combat_units: Vec<&FrameUnit> = units
                .iter()
                .filter(|u| {
                    !u.is_hallucination
                        && UnitClassifier::class(&table.name_of(u.unit_type)) == UnitClass::Combat
                })
                .copied()
                .collect();
            clusters_all.extend(cluster_combat_units(
                &combat_units,
                table,
                config.cluster_radius,
            ));
        }

        // confrontations: our clusters vs enemy clusters
        let our_clusters: Vec<Cluster> = clusters_all
            .iter()
            .filter(|c| c.player == self.observed_player_id)
            .cloned()
            .collect();
        let enemy_clusters: Vec<Cluster> = clusters_all
            .iter()
            .filter(|c| c.player != self.observed_player_id)
            .cloned()
            .collect();
        let confrontation_metrics: Vec<ConfrontationMetrics> = confrontations(&our_clusters, &enemy_clusters, config.confront_radius);

        self.engagement.on_tick(
            frame.tick,
            &clusters_all,
            &townhalls,
            &engagement_deaths,
            config,
            &mut events,
        );

        let cluster_metrics: Vec<ClusterMetrics> = clusters_all
            .iter()
            .map(crate::clusters::cluster_to_metrics)
            .collect();

        self.frames_analyzed += 1;

        let tick_metrics = TickMetrics {
            tick: frame.tick,
            time_seconds: loops_to_seconds(frame.tick),
            players: player_metrics,
            clusters: cluster_metrics,
            confrontations: confrontation_metrics,
        };
        (tick_metrics, events)
    }

    #[allow(clippy::too_many_arguments)]
    fn compute_player_metrics(
        &mut self,
        player: u32,
        units: &[&FrameUnit],
        table: &UnitTable,
        upgrades: &UpgradeTable,
        frame: &Frame,
        tick_killed: &BTreeMap<u32, DeathMetrics>,
        tick_lost: &BTreeMap<u32, DeathMetrics>,
        produced_value: f32,
        elapsed_seconds: f64,
        events: &mut Vec<EventRecord>,
    ) -> (PlayerMetrics, EconomyModel) {
        let economy = EconomyModel::build(units, table);

        let mut workers = 0usize;
        let mut expansions = 0usize;
        let mut army = ArmyMetrics::default();
        let mut supply_by_type: BTreeMap<String, (usize, f32, f32)> = BTreeMap::new();
        // building name -> (total present, completed, value)
        let mut buildings_by_type: BTreeMap<String, (usize, usize, f32)> = BTreeMap::new();
        let mut production = ProductionMetrics::default();
        let mut food_used = 0.0f32;
        let mut food_army = 0.0f32;
        let mut food_workers = 0.0f32;
        let mut food_cap = 0.0f32;

        let mut addon_tags: HashMap<u64, String> = HashMap::new();
        for unit in units.iter() {
            let name = table.name_of(unit.unit_type);
            if is_reactor_addon(&name) {
                addon_tags.insert(unit.tag, name);
            }
        }

        let is_observed = player == self.observed_player_id;

        for unit in units {
            let info = match table.get(unit.unit_type) {
                Some(info) => info,
                None => continue,
            };
            if unit.is_hallucination {
                continue;
            }
            let name = info.name.as_str();
            let class = UnitClassifier::class(name);
            let flags = UnitClassifier::combat_flags(name);

            if class == UnitClass::Worker {
                workers += 1;
                food_workers += info.food_required;
                food_used += info.food_required;
                food_army += info.food_required;
            } else if class == UnitClass::Combat {
                army.combat_supply += info.food_required;
                army.combat_value += info.value();
                food_used += info.food_required;
                food_army += info.food_required;
                if flags.air {
                    army.air_supply += info.food_required;
                }
                if flags.stealth {
                    army.stealth_supply += info.food_required;
                }
                if flags.tier3 {
                    army.tier3_supply += info.food_required;
                }
            } else if class == UnitClass::Support {
                army.support_supply += info.food_required;
                army.support_value += info.value();
                food_used += info.food_required;
                food_army += info.food_required;
            } else if class == UnitClass::Structure {
                if UnitClassifier::is_townhall(name) {
                    expansions += 1;
                }
                let entry = buildings_by_type.entry(info.name.clone()).or_insert((0, 0, 0.0));
                entry.0 += 1;
                entry.2 += info.value();
                if unit.build_progress >= 1.0 {
                    entry.1 += 1;
                }
            }

            if UnitClassifier::is_supply_provider(name) {
                food_cap += info.food_provided;
            }

            // supply_by_type for non-structure units
            if class != UnitClass::Structure && class != UnitClass::Other {
                let entry = supply_by_type.entry(info.name.clone()).or_insert((0, 0.0, 0.0));
                entry.0 += 1;
                entry.1 += info.food_required;
                entry.2 += info.value();
            }

            // -- production --
            if let Some(prod) = UnitClassifier::production_info(name) {
                let multiplier = if let Some(addon) = unit.add_on_tag
                    && let Some(addon_name) = addon_tags.get(&addon)
                    && is_reactor_addon(addon_name)
                {
                    2
                } else {
                    1
                };
                let (min_per_min, ves_per_min) = production_spend_rate(name);
                let tier = prod.tier;

                let tier_entry = production.tiers.entry(tier).or_default();
                tier_entry.structures += 1;
                tier_entry.capacity_spend_minerals_per_min += min_per_min * multiplier as f32;
                tier_entry.capacity_spend_vespene_per_min += ves_per_min * multiplier as f32;
                if unit.is_active {
                    tier_entry.producing += 1;
                }

                production.structures += 1;
                production.capacity_spend_minerals_per_min += min_per_min * multiplier as f32;
                production.capacity_spend_vespene_per_min += ves_per_min * multiplier as f32;
                if unit.is_active {
                    production.producing += 1;
                }
            }
        }

        if production.structures > 0 {
            production.utilisation =
                production.producing as f32 / production.structures as f32;
        }
        if elapsed_seconds > 0.0 {
            production.produced_minerals_per_min =
                (produced_value as f64 / elapsed_seconds * 60.0) as f32;
        }

        // -- deaths for this player --
        let mut killed = tick_killed.get(&player).cloned().unwrap_or_default();
        let mut lost = tick_lost.get(&player).cloned().unwrap_or_default();
        {
            let stat = self.stats.entry(player).or_default();
            stat.killed.units_value += killed.units_value;
            stat.killed.workers_value += killed.workers_value;
            stat.killed.support_value += killed.support_value;
            stat.killed.structures_value += killed.structures_value;
            stat.killed.units_count += killed.units_count;
            stat.killed.workers_count += killed.workers_count;
            stat.killed.support_count += killed.support_count;
            stat.killed.structures_count += killed.structures_count;

            stat.lost.units_value += lost.units_value;
            stat.lost.workers_value += lost.workers_value;
            stat.lost.support_value += lost.support_value;
            stat.lost.structures_value += lost.structures_value;
            stat.lost.units_count += lost.units_count;
            stat.lost.workers_count += lost.workers_count;
            stat.lost.support_count += lost.support_count;
            stat.lost.structures_count += lost.structures_count;

            killed = stat.killed.clone();
            lost = stat.lost.clone();
        }

        // -- structure transitions (non-production buildings) --
        self.detect_structure_transitions(player, &buildings_by_type, frame.tick, events);

        // -- player_common bank/food for observed player --
        let mut metrics = PlayerMetrics {
            workers,
            expansions,
            army,
            production,
            killed,
            lost,
            supply_by_type: supply_by_type
                .into_iter()
                .map(|(unit_type, (count, supply, value))| SupplyByType {
                    unit_type,
                    count,
                    supply,
                    value,
                })
                .collect(),
            buildings_by_type: buildings_by_type
                .into_iter()
                .map(|(unit_type, (count, completed, value))| BuildingByType {
                    unit_type,
                    count,
                    completed,
                    under_construction: count - completed,
                    value,
                })
                .collect(),
            ..Default::default()
        };

        if is_observed
            && let Some(common) = &frame.player_common
        {
            metrics.minerals = common.minerals;
            metrics.vespene = common.vespene;
            metrics.food_used = common.food_used;
            metrics.food_cap = common.food_cap;
            metrics.food_army = common.food_army;
            metrics.food_workers = common.food_workers;
        } else {
            metrics.food_used = food_used.round() as u32;
            metrics.food_army = food_army.round() as u32;
            metrics.food_workers = food_workers.round() as u32;
            metrics.food_cap = food_cap.round() as u32;
        }

        // -- expansion events --
        self.detect_expansion_changes(
            player,
            units,
            table,
            frame.tick,
            &mut metrics,
            events,
        );

            // -- composition transitions --
            self.detect_composition_transitions(player, &metrics.army, frame.tick, events);

            // -- production transitions --
            self.detect_production_transitions(player, units, table, frame.tick, events);

            // -- upgrade research --
            self.detect_upgrade_transitions(player, units, upgrades, frame.tick, events);

        (metrics, economy)
    }

    fn detect_expansion_changes(
        &mut self,
        player: u32,
        units: &[&FrameUnit],
        table: &UnitTable,
        tick: u32,
        metrics: &mut PlayerMetrics,
        events: &mut Vec<EventRecord>,
    ) {
        let mut current: HashMap<u64, Point2> = HashMap::new();
        for unit in units {
            if unit.is_hallucination {
                continue;
            }
            let name = table.name_of(unit.unit_type);
            if UnitClassifier::is_townhall(&name) {
                current.insert(unit.tag, unit.pos);
            }
        }

        let prev_count = self.prev_townhall_count.get(&player).copied().unwrap_or(0);
        let now_count = current.len();

        if now_count > prev_count {
            let known = self.townhall_tags.entry(player).or_default();
            for (tag, pos) in &current {
                if !known.contains_key(tag) {
                    known.insert(*tag, *pos);
                    events.push(EventRecord::ExpansionGained(ExpansionEvent {
                        tick,
                        time_seconds: loops_to_seconds(tick),
                        player,
                        expansions: now_count,
                        location: Some(Point::from(*pos)),
                    }));
                }
            }
        } else if now_count < prev_count {
            let known = self.townhall_tags.entry(player).or_default();
            let mut lost_locations = Vec::new();
            for (tag, pos) in known.iter() {
                if !current.contains_key(tag) {
                    lost_locations.push(*pos);
                }
            }
            for pos in lost_locations {
                known.retain(|_, p| *p != pos);
                events.push(EventRecord::ExpansionLost(ExpansionEvent {
                    tick,
                    time_seconds: loops_to_seconds(tick),
                    player,
                    expansions: now_count,
                    location: Some(Point::from(pos)),
                }));
            }
        }

        self.prev_townhall_count.insert(player, now_count);
        metrics.expansions = now_count;
    }

    fn detect_composition_transitions(
        &mut self,
        player: u32,
        army: &ArmyMetrics,
        tick: u32,
        events: &mut Vec<EventRecord>,
    ) {
        let combat_supply = army.combat_supply;
        let air_share = army.air_supply / combat_supply.max(0.01);

        let prev_air = self.air_heavy.get(&player).copied().unwrap_or(false);
        let now_air = air_share >= COMPOSITION_AIR_THRESHOLD;
        if now_air && !prev_air {
            events.push(EventRecord::CompositionTransition(CompositionTransition {
                tick,
                time_seconds: loops_to_seconds(tick),
                player,
                category: "air".to_string(),
                share_before: 0.0,
                share_after: air_share,
            }));
            self.transitions.push(TransitionSummary {
                tick,
                time_seconds: loops_to_seconds(tick),
                player,
                kind: "composition".to_string(),
                detail: format!("heavy air production (air share {:.0}%)", air_share * 100.0),
            });
        } else if prev_air && air_share <= COMPOSITION_AIR_FLOOR {
            events.push(EventRecord::CompositionTransition(CompositionTransition {
                tick,
                time_seconds: loops_to_seconds(tick),
                player,
                category: "air".to_string(),
                share_before: 1.0,
                share_after: air_share,
            }));
            self.transitions.push(TransitionSummary {
                tick,
                time_seconds: loops_to_seconds(tick),
                player,
                kind: "composition".to_string(),
                detail: "air share dropped back below threshold".to_string(),
            });
        }
        self.air_heavy.insert(player, now_air);

        let prev_stealth = self.stealth_present.get(&player).copied().unwrap_or(false);
        let now_stealth = army.stealth_supply >= STEALTH_SUPPLY_THRESHOLD;
        if now_stealth && !prev_stealth {
            events.push(EventRecord::CompositionTransition(CompositionTransition {
                tick,
                time_seconds: loops_to_seconds(tick),
                player,
                category: "stealth".to_string(),
                share_before: 0.0,
                share_after: army.stealth_supply,
            }));
            self.transitions.push(TransitionSummary {
                tick,
                time_seconds: loops_to_seconds(tick),
                player,
                kind: "composition".to_string(),
                detail: "stealth combat units entered the army".to_string(),
            });
        } else if prev_stealth && army.stealth_supply < 2.0 {
            events.push(EventRecord::CompositionTransition(CompositionTransition {
                tick,
                time_seconds: loops_to_seconds(tick),
                player,
                category: "stealth".to_string(),
                share_before: 1.0,
                share_after: army.stealth_supply,
            }));
            self.transitions.push(TransitionSummary {
                tick,
                time_seconds: loops_to_seconds(tick),
                player,
                kind: "composition".to_string(),
                detail: "stealth combat units left the army".to_string(),
            });
        }
        self.stealth_present.insert(player, now_stealth);

        let prev_tier3 = self.tier3_present.get(&player).copied().unwrap_or(false);
        let now_tier3 = army.tier3_supply >= TIER3_SUPPLY_THRESHOLD;
        if now_tier3 && !prev_tier3 {
            events.push(EventRecord::CompositionTransition(CompositionTransition {
                tick,
                time_seconds: loops_to_seconds(tick),
                player,
                category: "tier3".to_string(),
                share_before: 0.0,
                share_after: army.tier3_supply,
            }));
            self.transitions.push(TransitionSummary {
                tick,
                time_seconds: loops_to_seconds(tick),
                player,
                kind: "composition".to_string(),
                detail: format!("tier3 units in the army ({:.0} supply)", army.tier3_supply),
            });
        } else if prev_tier3 && army.tier3_supply < 2.0 {
            events.push(EventRecord::CompositionTransition(CompositionTransition {
                tick,
                time_seconds: loops_to_seconds(tick),
                player,
                category: "tier3".to_string(),
                share_before: 1.0,
                share_after: army.tier3_supply,
            }));
            self.transitions.push(TransitionSummary {
                tick,
                time_seconds: loops_to_seconds(tick),
                player,
                kind: "composition".to_string(),
                detail: "tier3 units left the army".to_string(),
            });
        }
        self.tier3_present.insert(player, now_tier3);
    }

    fn detect_production_transitions(
        &mut self,
        player: u32,
        units: &[&FrameUnit],
        table: &UnitTable,
        tick: u32,
        events: &mut Vec<EventRecord>,
    ) {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for unit in units {
            if unit.is_hallucination {
                continue;
            }
            let name = table.name_of(unit.unit_type);
            if let Some(prod) = UnitClassifier::production_info(&name) {
                let entry = counts.entry(name.clone()).or_insert(0);
                *entry += 1;
                if let Some(addon) = unit.add_on_tag {
                    let _ = addon;
                }
                let _ = prod;
            }
        }
        for (name, count) in &counts {
            let prev = self
                .prev_structure_counts
                .get(&(player, name.clone()))
                .copied()
                .unwrap_or(0);
            if *count > prev {
                let tier = UnitClassifier::production_info(name)
                    .map(|p| p.tier)
                    .unwrap_or(0);
                events.push(EventRecord::ProductionTransition(
                    ProductionTransition {
                        tick,
                        time_seconds: loops_to_seconds(tick),
                        player,
                        structure: name.clone(),
                        tier,
                        count: *count,
                    },
                ));
                self.transitions.push(TransitionSummary {
                    tick,
                    time_seconds: loops_to_seconds(tick),
                    player,
                    kind: "production".to_string(),
                    detail: format!("{name} x{count} (tier {tier})"),
                });
            }
        }
        for (name, count) in &counts {
            self.prev_structure_counts
                .insert((player, name.clone()), *count);
        }
    }

    /// Track non-production structures (forges, tech buildings, defenses...)
    /// by comparing each type's *completed* count against the previous frame.
    /// Emits a gained/lost event whenever it changes, so build orders like a
    /// double-forge or an early Robotics Bay show up in events.jsonl and in
    /// the summary's transition timeline. Production structures are covered by
    /// `detect_production_transitions` and are skipped here; townhalls are
    /// covered by expansion events and also skipped.
    fn detect_structure_transitions(
        &mut self,
        player: u32,
        buildings: &BTreeMap<String, (usize, usize, f32)>,
        tick: u32,
        events: &mut Vec<EventRecord>,
    ) {
        let tracked =
            |name: &str| UnitClassifier::production_info(name).is_none() && !UnitClassifier::is_townhall(name);

        // gained / reduced counts on types present this frame
        for (name, &(_present, completed, _value)) in buildings {
            if !tracked(name) {
                continue;
            }
            let prev = self
                .prev_completed_buildings
                .get(&(player, name.clone()))
                .copied()
                .unwrap_or(0);
            if completed == prev {
                continue;
            }
            let change = if completed > prev { "gained" } else { "lost" };
            events.push(EventRecord::StructureTransition(StructureTransition {
                tick,
                time_seconds: loops_to_seconds(tick),
                player,
                structure: name.clone(),
                completed,
                change: change.to_string(),
            }));
            self.transitions.push(TransitionSummary {
                tick,
                time_seconds: loops_to_seconds(tick),
                player,
                kind: "structure".to_string(),
                detail: format!("{name} x{completed} {change}"),
            });
        }

        // types seen before but absent now were destroyed or cancelled
        let mut vanished: Vec<(String, usize)> = Vec::new();
        for (key, &count) in &self.prev_completed_buildings {
            if key.0 != player
                || count == 0
                || !tracked(&key.1)
                || buildings.contains_key(&key.1)
            {
                continue;
            }
            vanished.push((key.1.clone(), count));
        }
        for (name, count) in vanished {
            self.prev_completed_buildings
                .insert((player, name.clone()), 0);
            events.push(EventRecord::StructureTransition(StructureTransition {
                tick,
                time_seconds: loops_to_seconds(tick),
                player,
                structure: name.clone(),
                completed: 0,
                change: "lost".to_string(),
            }));
            self.transitions.push(TransitionSummary {
                tick,
                time_seconds: loops_to_seconds(tick),
                player,
                kind: "structure".to_string(),
                detail: format!("{name} x{count} lost"),
            });
        }

        for (name, &(_present, completed, _value)) in buildings {
            if tracked(name) {
                self.prev_completed_buildings
                    .insert((player, name.clone()), completed);
            }
        }
    }

    /// Track upgrades as research orders appear on structures and complete.
    /// Research is detected via the structure's orders: an upgrade's ability id
    /// (from the game data upgrade table) appears with progress 0->1 while
    /// researching and vanishes on completion, so an order that was seen and
    /// has since disappeared counts as researched once it got well underway.
    fn detect_upgrade_transitions(
        &mut self,
        player: u32,
        units: &[&FrameUnit],
        upgrades: &UpgradeTable,
        tick: u32,
        events: &mut Vec<EventRecord>,
    ) {
        let mut active: HashSet<u32> = HashSet::new();
        for unit in units {
            if unit.is_hallucination {
                continue;
            }
            for (ability_id, progress) in &unit.orders {
                if upgrades.name_by_ability(*ability_id).is_none() {
                    continue;
                }
                active.insert(*ability_id);
                let key = (player, *ability_id);
                let prev = self.upgrade_progress.get(&key).copied().unwrap_or(0.0);
                if *progress > prev {
                    self.upgrade_progress.insert(key, *progress);
                }
                let seen = self.upgrade_seen.entry(player).or_default();
                if seen.insert(*ability_id) {
                    let name = upgrades.name_by_ability(*ability_id).unwrap_or("Unknown");
                    events.push(EventRecord::UpgradeStarted(UpgradeTransition {
                        tick,
                        time_seconds: loops_to_seconds(tick),
                        player,
                        ability_id: *ability_id,
                        name: name.to_string(),
                    }));
                }
            }
        }

        let seen = self.upgrade_seen.entry(player).or_default();
        for ability_id in seen.iter().copied().collect::<Vec<_>>() {
            if active.contains(&ability_id) {
                continue;
            }
            seen.remove(&ability_id);
            let key = (player, ability_id);
            let progress = self.upgrade_progress.get(&key).copied().unwrap_or(0.0);
            self.upgrade_progress.remove(&key);
            if progress < UPGRADE_COMPLETED_PROGRESS {
                // cancelled (or a stray order); ignore
                continue;
            }
            let name = upgrades.name_by_ability(ability_id).unwrap_or("Unknown");
            events.push(EventRecord::UpgradeResearched(UpgradeTransition {
                tick,
                time_seconds: loops_to_seconds(tick),
                player,
                ability_id,
                name: name.to_string(),
            }));
            self.upgrades.push(UpgradeSummary {
                player,
                name: name.to_string(),
                tick,
                time_seconds: loops_to_seconds(tick),
            });
            self.transitions.push(TransitionSummary {
                tick,
                time_seconds: loops_to_seconds(tick),
                player,
                kind: "upgrade".to_string(),
                detail: format!("{name} researched"),
            });
        }
    }

    fn update_stats(&mut self, player: u32, metrics: &PlayerMetrics) {
        let stat = self.stats.entry(player).or_default();
        stat.peak_expansions = stat.peak_expansions.max(metrics.expansions);
        stat.peak_workers = stat.peak_workers.max(metrics.workers);
        stat.peak_army_supply = stat.peak_army_supply.max(metrics.army.combat_supply);
        stat.peak_army_value = stat.peak_army_value.max(metrics.army.combat_value);
    }
}

pub fn loops_to_seconds(loop_: u32) -> f64 {
    loop_ as f64 / GAME_LOOPS_PER_SECOND
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game_data::{UnitTable, UnitTypeInfo};

    fn table() -> UnitTable {
        let mut table = UnitTable::default();
        let specs: &[(&str, u32, u32, u32, f32, f32, f32)] = &[
            ("Probe", 10, 50, 0, 1.0, 0.0, 0.0),
            ("Nexus", 20, 400, 0, 0.0, 0.0, 0.0),
            ("Zealot", 30, 100, 0, 2.0, 10.0, 0.0),
            ("Stalker", 40, 125, 50, 2.0, 10.0, 10.0),
            ("Pylon", 50, 100, 0, 0.0, 0.0, 0.0),
            ("Gateway", 60, 150, 0, 0.0, 0.0, 0.0),
            ("Assimilator", 70, 75, 0, 0.0, 0.0, 0.0),
        ];
        for (name, uid, mineral, vespene, food, gdps, adps) in specs {
            table.units.insert(
                *uid,
                UnitTypeInfo {
                    name: name.to_string(),
                    mineral_cost: *mineral,
                    vespene_cost: *vespene,
                    food_required: *food,
                    food_provided: 8.0,
                    ground_dps: *gdps,
                    air_dps: *adps,
                },
            );
        }
        table
    }

    fn upgrade_table() -> UpgradeTable {
        let mut by_ability = HashMap::new();
        by_ability.insert(7, "Blink".to_string());
        by_ability.insert(9, "Charge".to_string());
        UpgradeTable { by_ability }
    }

    fn f_unit(tag: u64, owner: u32, unit_type: u32, x: f32, y: f32) -> FrameUnit {
        FrameUnit {
            tag,
            unit_type,
            owner,
            pos: Point2::new(x, y),
            is_active: false,
            is_hallucination: false,
            hits: 100.0,
            build_progress: 1.0,
            add_on_tag: None,
            orders: vec![],
            weapon_cooldown: 0.0,
        }
    }

    #[test]
    fn produced_units_detected_once() {
        let table = table();
        let upgrades = upgrade_table();
        let mut analyzer = Analyzer::new(1);
        let frame = Frame {
            tick: 100,
            units: vec![
                f_unit(1, 1, 20, 0.0, 0.0),
                f_unit(2, 1, 30, 10.0, 10.0),
            ],
            dead: vec![],
            player_common: None,
            score: None,
            effects: vec![]
        };
        analyzer.process_frame(&frame, &table, &upgrades, &ClusterConfig::default());
        // second frame: a new zealot appears
        let frame2 = Frame {
            tick: 120,
            units: vec![
                f_unit(1, 1, 20, 0.0, 0.0),
                f_unit(2, 1, 30, 10.0, 10.0),
                f_unit(3, 1, 30, 12.0, 12.0),
            ],
            dead: vec![],
            player_common: None,
            score: None,
            effects: vec![]
        };
        let (metrics, _) = analyzer.process_frame(&frame2, &table, &upgrades, &ClusterConfig::default());
        let p = metrics.players.get(&1).unwrap();
        assert!(p.production.produced_minerals_per_min > 0.0);
    }

    #[test]
    fn deaths_are_attributed_to_owner() {
        let table = table();
        let upgrades = upgrade_table();
        let mut analyzer = Analyzer::new(1);
        // frame 1: have a zealot
        let frame1 = Frame {
            tick: 100,
            units: vec![f_unit(1, 1, 30, 10.0, 10.0)],
            dead: vec![],
            player_common: None,
            score: None,
            effects: vec![]
        };
        analyzer.process_frame(&frame1, &table, &upgrades, &ClusterConfig::default());
        // frame 2: zealot dead
        let frame2 = Frame {
            tick: 120,
            units: vec![],
            dead: vec![1],
            player_common: None,
            score: None,
            effects: vec![]
        };
        let (metrics, _) = analyzer.process_frame(&frame2, &table, &upgrades, &ClusterConfig::default());
        let p1 = metrics.players.get(&1).unwrap();
        assert_eq!(p1.lost.units_value, 100.0);
        let p2 = metrics.players.get(&2).unwrap();
        assert_eq!(p2.killed.units_value, 100.0);
    }

    #[test]
    fn deaths_accumulate_cumulatively() {
        let table = table();
        let upgrades = upgrade_table();
        let mut analyzer = Analyzer::new(1);
        let frame1 = Frame {
            tick: 100,
            units: vec![
                f_unit(1, 1, 30, 10.0, 10.0),
                f_unit(2, 1, 30, 20.0, 20.0),
            ],
            dead: vec![],
            player_common: None,
            score: None,
            effects: vec![]
        };
        analyzer.process_frame(&frame1, &table, &upgrades, &ClusterConfig::default());

        let frame2 = Frame {
            tick: 120,
            units: vec![f_unit(2, 1, 30, 20.0, 20.0)],
            dead: vec![1],
            player_common: None,
            score: None,
            effects: vec![]
        };
        let (metrics, _) = analyzer.process_frame(&frame2, &table, &upgrades, &ClusterConfig::default());
        assert_eq!(metrics.players.get(&1).unwrap().lost.units_value, 100.0);

        let frame3 = Frame {
            tick: 140,
            units: vec![],
            dead: vec![2],
            player_common: None,
            score: None,
            effects: vec![]
        };
        let (metrics, _) = analyzer.process_frame(&frame3, &table, &upgrades, &ClusterConfig::default());
        assert_eq!(metrics.players.get(&1).unwrap().lost.units_value, 200.0);
    }

    #[test]
    fn buildings_tracked_with_construction_progress() {
        let table = table();
        let upgrades = upgrade_table();
        let mut analyzer = Analyzer::new(1);
        // frame 1: one finished gateway and a forge still going up
        let mut forge = f_unit(4, 1, 70, 30.0, 30.0);
        forge.build_progress = 0.5;
        let frame1 = Frame {
            tick: 100,
            units: vec![f_unit(1, 1, 60, 10.0, 10.0), forge],
            dead: vec![],
            player_common: None,
            score: None,
            effects: vec![]
        };
        let (metrics, _) =
            analyzer.process_frame(&frame1, &table, &upgrades, &ClusterConfig::default());
        let p1 = metrics.players.get(&1).unwrap();
        let gateway = p1
            .buildings_by_type
            .iter()
            .find(|b| b.unit_type == "Gateway")
            .unwrap();
        assert_eq!(gateway.completed, 1);
        assert_eq!(gateway.under_construction, 0);
        let forge_row = p1
            .buildings_by_type
            .iter()
            .find(|b| b.unit_type == "Assimilator")
            .unwrap();
        assert_eq!(forge_row.count, 1);
        assert_eq!(forge_row.completed, 0);
        assert_eq!(forge_row.under_construction, 1);

        // frame 2: the forge finished; a structure_transition event fires
        let frame2 = Frame {
            tick: 200,
            units: vec![
                f_unit(1, 1, 60, 10.0, 10.0),
                f_unit(4, 1, 70, 30.0, 30.0),
            ],
            dead: vec![],
            player_common: None,
            score: None,
            effects: vec![]
        };
        let (_, events) =
            analyzer.process_frame(&frame2, &table, &upgrades, &ClusterConfig::default());
        assert!(events.iter().any(|e| matches!(
            e,
            EventRecord::StructureTransition(t)
                if t.structure == "Assimilator" && t.change == "gained" && t.completed == 1
        )));

        // frame 3: the forge is destroyed; a lost event fires
        let frame3 = Frame {
            tick: 300,
            units: vec![f_unit(1, 1, 60, 10.0, 10.0)],
            dead: vec![4],
            player_common: None,
            score: None,
            effects: vec![]
        };
        let (_, events) =
            analyzer.process_frame(&frame3, &table, &upgrades, &ClusterConfig::default());
        assert!(events.iter().any(|e| matches!(
            e,
            EventRecord::StructureTransition(t)
                if t.structure == "Assimilator" && t.change == "lost"
        )));
    }

    #[test]
    fn production_structures_do_not_emit_structure_transitions() {
        let table = table();
        let upgrades = upgrade_table();
        let mut analyzer = Analyzer::new(1);
        let frame1 = Frame {
            tick: 100,
            units: vec![],
            dead: vec![],
            player_common: None,
            score: None,
            effects: vec![]
        };
        analyzer.process_frame(&frame1, &table, &upgrades, &ClusterConfig::default());
        let frame2 = Frame {
            tick: 200,
            units: vec![f_unit(1, 1, 60, 10.0, 10.0)],
            dead: vec![],
            player_common: None,
            score: None,
            effects: vec![]
        };
        let (_, events) =
            analyzer.process_frame(&frame2, &table, &upgrades, &ClusterConfig::default());
        assert!(!events
            .iter()
            .any(|e| matches!(e, EventRecord::StructureTransition(_))));
    }

    #[test]
    fn expansion_gained_event() {
        let table = table();
        let upgrades = upgrade_table();
        let mut analyzer = Analyzer::new(1);
        let frame1 = Frame {
            tick: 100,
            units: vec![f_unit(1, 1, 20, 0.0, 0.0)],
            dead: vec![],
            player_common: None,
            score: None,
            effects: vec![]
        };
        analyzer.process_frame(&frame1, &table, &upgrades, &ClusterConfig::default());
        let frame2 = Frame {
            tick: 200,
            units: vec![
                f_unit(1, 1, 20, 0.0, 0.0),
                f_unit(2, 1, 20, 80.0, 80.0),
            ],
            dead: vec![],
            player_common: None,
            score: None,
            effects: vec![]
        };
        let (_, events) = analyzer.process_frame(&frame2, &table, &upgrades, &ClusterConfig::default());
        assert!(events
            .iter()
            .any(|e| matches!(e, EventRecord::ExpansionGained(_))));
    }

    #[test]
    fn worker_vanishing_into_gas_is_not_a_death_or_a_birth() {
        let table = table();
        let upgrades = upgrade_table();
        let mut analyzer = Analyzer::new(1);
        // frame 1: a probe is gathering
        let frame1 = Frame {
            tick: 100,
            units: vec![f_unit(1, 1, 10, 10.0, 10.0)],
            dead: vec![],
            player_common: None,
            score: None,
            effects: vec![]
        };
        analyzer.process_frame(&frame1, &table, &upgrades, &ClusterConfig::default());
        // frame 2: the probe is inside the assimilator (vanishes, no death event)
        let frame2 = Frame {
            tick: 120,
            units: vec![],
            dead: vec![],
            player_common: None,
            score: None,
            effects: vec![]
        };
        let (metrics, _) = analyzer.process_frame(&frame2, &table, &upgrades, &ClusterConfig::default());
        assert_eq!(metrics.players.get(&1).unwrap().lost.workers_count, 0);
        // frame 3: the probe re-emerges (reappears, no death/birth event)
        let frame3 = Frame {
            tick: 140,
            units: vec![f_unit(1, 1, 10, 10.0, 10.0)],
            dead: vec![],
            player_common: None,
            score: None,
            effects: vec![]
        };
        let (metrics, _) = analyzer.process_frame(&frame3, &table, &upgrades, &ClusterConfig::default());
        assert_eq!(metrics.players.get(&1).unwrap().lost.workers_count, 0);
        assert!(metrics.players.get(&1).unwrap().production.produced_minerals_per_min <= 0.0);
        // a genuinely dead worker still attributes correctly
        let frame4 = Frame {
            tick: 160,
            units: vec![],
            dead: vec![1],
            player_common: None,
            score: None,
            effects: vec![]
        };
        let (metrics, _) = analyzer.process_frame(&frame4, &table, &upgrades, &ClusterConfig::default());
        assert_eq!(metrics.players.get(&1).unwrap().lost.workers_count, 1);
        assert_eq!(metrics.players.get(&1).unwrap().lost.workers_value, 50.0);
    }

    fn f_struct_with_order(tag: u64, owner: u32, ability_id: u32, progress: f32) -> FrameUnit {
        FrameUnit {
            tag,
            unit_type: 60, // Gateway
            owner,
            pos: Point2::new(0.0, 0.0),
            is_active: true,
            is_hallucination: false,
            hits: 100.0,
            build_progress: 1.0,
            add_on_tag: None,
            orders: vec![(ability_id, progress)],
            weapon_cooldown: 0.0,
        }
    }

    #[test]
    fn upgrade_research_start_and_complete() {
        let table = table();
        let upgrades = upgrade_table();
        let mut analyzer = Analyzer::new(1);

        // frame 1: research starts
        let frame1 = Frame {
            tick: 100,
            units: vec![f_struct_with_order(1, 1, 7, 0.05)],
            dead: vec![],
            player_common: None,
            score: None,
            effects: vec![]
        };
        let (_, events) = analyzer.process_frame(&frame1, &table, &upgrades, &ClusterConfig::default());
        assert!(events
            .iter()
            .any(|e| matches!(e, EventRecord::UpgradeStarted(t) if t.ability_id == 7 && t.name == "Blink")));

        // frame 2: still researching, progressed
        let frame2 = Frame {
            tick: 140,
            units: vec![f_struct_with_order(1, 1, 7, 0.6)],
            dead: vec![],
            player_common: None,
            score: None,
            effects: vec![]
        };
        let (_, events) = analyzer.process_frame(&frame2, &table, &upgrades, &ClusterConfig::default());
        assert!(!events.iter().any(|e| matches!(e, EventRecord::UpgradeResearched(_))));

        // frame 3: order gone => researched
        let frame3 = Frame {
            tick: 180,
            units: vec![f_struct_with_order(1, 1, 0, 0.0)],
            dead: vec![],
            player_common: None,
            score: None,
            effects: vec![]
        };
        let (_, events) = analyzer.process_frame(&frame3, &table, &upgrades, &ClusterConfig::default());
        assert!(events
            .iter()
            .any(|e| matches!(e, EventRecord::UpgradeResearched(t) if t.ability_id == 7 && t.name == "Blink")));
        assert!(analyzer
            .take_upgrade_summaries()
            .iter()
            .any(|u| u.player == 1 && u.name == "Blink"));
    }

    #[test]
    fn upgrade_cancelled_before_completion_is_ignored() {
        let table = table();
        let upgrades = upgrade_table();
        let mut analyzer = Analyzer::new(1);

        let frame1 = Frame {
            tick: 100,
            units: vec![f_struct_with_order(1, 1, 9, 0.1)],
            dead: vec![],
            player_common: None,
            score: None,
            effects: vec![]
        };
        let _ = analyzer.process_frame(&frame1, &table, &upgrades, &ClusterConfig::default());

        // order vanishes having only reached 10% => cancelled, no researched event
        let frame2 = Frame {
            tick: 140,
            units: vec![f_struct_with_order(1, 1, 0, 0.0)],
            dead: vec![],
            player_common: None,
            score: None,
            effects: vec![]
        };
        let (_, events) = analyzer.process_frame(&frame2, &table, &upgrades, &ClusterConfig::default());
        assert!(!events.iter().any(|e| matches!(e, EventRecord::UpgradeResearched(_))));
        assert!(analyzer.take_upgrade_summaries().is_empty());
    }
}
