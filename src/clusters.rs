use std::collections::{BTreeMap, HashMap, HashSet};

use crate::sc2::Point2;

use crate::{
    categories::UnitClassifier,
    game_data::UnitTable,
    metrics::FrameUnit,
    model::{ClusterMetrics, ClusterUnit, ConfrontationMetrics, EngagementDeaths, EventRecord},
};

#[derive(Debug, Clone, Copy)]
pub struct ClusterConfig {
    pub cluster_radius: f32,
    pub confront_radius: f32,
    pub engagement_radius: f32,
    pub engagement_min_supply: f32,
    pub base_assault_radius: f32,
    pub grace_ticks: usize,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            cluster_radius: 12.0,
            confront_radius: 20.0,
            engagement_radius: 14.0,
            engagement_min_supply: 6.0,
            base_assault_radius: 11.0,
            grace_ticks: 6,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Cluster {
    pub player: u32,
    pub centroid: Point2,
    pub supply: f32,
    pub value: f32,
    pub hits: f32,
    pub dps: f32,
    pub count: usize,
    pub air_supply: f32,
    /// unit tag -> unit name
    pub tags: Vec<(u64, String)>,
    /// unit name -> (count, supply)
    pub units: BTreeMap<String, (usize, f32)>,
}

impl Cluster {
    pub fn score(&self) -> f32 {
        self.hits + self.dps * 15.0
    }
}

struct UnionFind {
    parent: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
        }
    }

    fn find(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            self.parent[x] = self.parent[self.parent[x]];
            x = self.parent[x];
        }
        x
    }

    fn union(&mut self, a: usize, b: usize) {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra != rb {
            self.parent[ra] = rb;
        }
    }
}

/// Cluster combat units by proximity using grid-based union-find.
pub fn cluster_combat_units(units: &[&FrameUnit], table: &UnitTable, radius: f32) -> Vec<Cluster> {
    if units.is_empty() {
        return Vec::new();
    }
    let player = units[0].owner;

    let mut cell_to_index: HashMap<(i32, i32), usize> = HashMap::new();
    let mut uf = UnionFind::new(units.len());

    for (i, unit) in units.iter().enumerate() {
        let cell = (
            (unit.pos.x / radius).floor() as i32,
            (unit.pos.y / radius).floor() as i32,
        );
        if let Some(&existing) = cell_to_index.get(&cell) {
            uf.union(i, existing);
        } else {
            cell_to_index.insert(cell, i);
        }
        for dx in -1..=1 {
            for dy in -1..=1 {
                if dx == 0 && dy == 0 {
                    continue;
                }
                if let Some(&other) = cell_to_index.get(&(cell.0 + dx, cell.1 + dy)) {
                    uf.union(i, other);
                }
            }
        }
    }

    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..units.len() {
        groups.entry(uf.find(i)).or_default().push(i);
    }

    groups
        .into_values()
        .map(|indices| {
            let mut centroid = Point2::new(0.0, 0.0);
            let mut supply = 0.0f32;
            let mut value = 0.0f32;
            let mut hits = 0.0f32;
            let mut dps = 0.0f32;
            let mut count = 0usize;
            let mut air_supply = 0.0f32;
            let mut units_map: BTreeMap<String, (usize, f32)> = BTreeMap::new();
            let mut tags: Vec<(u64, String)> = Vec::new();

            for &i in &indices {
                let unit = units[i];
                let Some(info) = table.get(unit.unit_type) else {
                    continue;
                };
                if unit.is_hallucination {
                    continue;
                }
                count += 1;
                centroid.x += unit.pos.x;
                centroid.y += unit.pos.y;
                supply += info.food_required;
                value += info.value();
                hits += unit.hits;
                dps += info.ground_dps + info.air_dps;
                tags.push((unit.tag, info.name.clone()));
                let flags = UnitClassifier::combat_flags(&info.name);
                if flags.air {
                    air_supply += info.food_required;
                }
                units_map
                    .entry(info.name.clone())
                    .and_modify(|(c, s)| {
                        *c += 1;
                        *s += info.food_required;
                    })
                    .or_insert((1, info.food_required));
            }

            if count > 0 {
                centroid.x /= count as f32;
                centroid.y /= count as f32;
            }

            Cluster {
                player,
                centroid,
                supply,
                value,
                hits,
                dps,
                count,
                air_supply,
                tags,
                units: units_map,
            }
        })
        .collect()
}

pub fn cluster_to_metrics(cluster: &Cluster) -> ClusterMetrics {
    ClusterMetrics {
        player: cluster.player,
        centroid: cluster.centroid.into(),
        supply: cluster.supply,
        value: cluster.value,
        hits: cluster.hits,
        dps: cluster.dps,
        count: cluster.count,
        air_supply: cluster.air_supply,
        units: cluster
            .units
            .iter()
            .map(|(name, (count, supply))| ClusterUnit {
                unit_type: name.clone(),
                count: *count,
                supply: *supply,
            })
            .collect(),
    }
}

/// Match each "our" cluster to the nearest enemy cluster within `confront_radius`.
pub fn confrontations(
    our: &[Cluster],
    enemy: &[Cluster],
    confront_radius: f32,
) -> Vec<ConfrontationMetrics> {
    let mut out = Vec::new();
    for a in our {
        let mut best: Option<(&Cluster, f32)> = None;
        for b in enemy {
            let dist = a.centroid.distance_squared(b.centroid).sqrt();
            if dist <= confront_radius
                && best.is_none_or(|(_, best_dist)| dist < best_dist) {
                    best = Some((b, dist));
                }
        }
        if let Some((b, dist)) = best {
            out.push(ConfrontationMetrics {
                our_player: a.player,
                enemy_player: b.player,
                our_supply: a.supply,
                enemy_supply: b.supply,
                our_value: a.value,
                enemy_value: b.value,
                our_score: a.score(),
                enemy_score: b.score(),
                distance: dist,
            });
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct TownhallRef {
    pub player: u32,
    pub name: String,
    pub pos: Point2,
}

// ---------------------------------------------------------------------------
// Engagements
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct ActiveEngagement {
    id: u64,
    start_tick: u32,
    last_tick: u32,
    kind: &'static str,
    location: Point2,
    base: Option<String>,
    base_pos: Option<Point2>,
    /// Unit tags currently part of this engagement (both sides). Engagement
    /// identity follows these tags so a battle is tracked as one engagement
    /// even as the clusters that form it shift between ticks.
    tags: HashSet<u64>,
    supply: BTreeMap<u32, f32>,
    value: BTreeMap<u32, f32>,
    count: BTreeMap<u32, usize>,
    units: BTreeMap<u32, BTreeMap<String, (usize, f32)>>,
    peak_supply: BTreeMap<u32, f32>,
    peak_value: BTreeMap<u32, f32>,
    deaths: BTreeMap<u32, EngagementDeaths>,
    stale_ticks: usize,
}

#[derive(Debug, Default)]
struct EngagementComponent {
    kind: &'static str,
    base: Option<String>,
    base_pos: Option<Point2>,
    clusters: Vec<usize>,
}

impl EngagementComponent {
    fn centroid(&self, clusters: &[Cluster]) -> Point2 {
        let mut centroid = Point2::new(0.0, 0.0);
        let mut total_weight = 0.0f32;
        for &i in &self.clusters {
            let cluster = &clusters[i];
            let weight = cluster.supply.max(1.0);
            centroid.x += cluster.centroid.x * weight;
            centroid.y += cluster.centroid.y * weight;
            total_weight += weight;
        }
        if total_weight > 0.0 {
            centroid.x /= total_weight;
            centroid.y /= total_weight;
        }
        centroid
    }

    fn tags(&self, clusters: &[Cluster]) -> HashSet<u64> {
        let mut tags = HashSet::new();
        for &i in &self.clusters {
            tags.extend(clusters[i].tags.iter().map(|(tag, _)| *tag));
        }
        tags
    }
}

pub struct EngagementTracker {
    next_id: u64,
    active: Vec<ActiveEngagement>,
    pub completed: Vec<crate::model::EngagementSummary>,
}

impl EngagementTracker {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            active: Vec::new(),
            completed: Vec::new(),
        }
    }

    /// deaths is a list of (owner, tag, value) for units that died this tick.
    pub fn on_tick(
        &mut self,
        tick: u32,
        clusters: &[Cluster],
        townhalls: &[TownhallRef],
        deaths: &[(u32, u64, f32)],
        config: &ClusterConfig,
        events: &mut Vec<EventRecord>,
    ) {
        let components = self.build_components(clusters, townhalls, config);

        // Attribute this tick's deaths to the engagements whose units died,
        // using the tags from the previous tick (before any re-clustering).
        for active in self.active.iter_mut() {
            for (owner, tag, value) in deaths {
                if !active.tags.contains(tag) {
                    continue;
                }
                let other = if *owner == 1 { 2 } else { 1 };
                let killed = active.deaths.entry(other).or_insert(EngagementDeaths {
                    player: other,
                    killed_value: 0.0,
                    lost_value: 0.0,
                    lost_units: 0,
                });
                killed.killed_value += value;
                let lost = active.deaths.entry(*owner).or_insert(EngagementDeaths {
                    player: *owner,
                    killed_value: 0.0,
                    lost_value: 0.0,
                    lost_units: 0,
                });
                lost.lost_value += value;
                lost.lost_units += 1;
            }
            active.tags.retain(|tag| !deaths.iter().any(|(_, d, _)| d == tag));
        }

        // Match each component to an active engagement by unit-tag overlap.
        //
        // `existing_active` is captured before the loop: an unmatched component
        // pushes a *new* engagement below, which makes `self.active` longer than
        // `matched`, and a later component can then match one of those new
        // entries (`matched[i]` with `i >= matched.len()` indexed out of bounds
        // on 2026-11-02, len 2 index 2).
        let existing_active = self.active.len();
        let mut matched = vec![false; existing_active];

        for comp in &components {
            let comp_tags = comp.tags(clusters);
            let centroid = comp.centroid(clusters);

            let mut best: Option<(usize, usize)> = None;
            for (i, active) in self.active.iter().enumerate() {
                if active.stale_ticks > 0 {
                    continue;
                }
                let overlap = comp_tags.intersection(&active.tags).count();
                if overlap > 0 && best.is_none_or(|(_, best_overlap)| overlap > best_overlap) {
                    best = Some((i, overlap));
                }
            }

            // Base assault continuation: if a fresh wave of units hits the same
            // base, keep tracking it as the same engagement even though the
            // unit tags have completely turned over.
            if best.is_none()
                && let Some(comp_base) = comp.base_pos
            {
                for (i, active) in self.active.iter().enumerate() {
                    if active.stale_ticks > 0 || active.kind != "base_assault" {
                        continue;
                    }
                    if let Some(active_base) = active.base_pos
                        && comp_base.distance_squared(active_base)
                            <= (config.base_assault_radius * 2.5).powi(2)
                    {
                        best = Some((i, 1));
                        break;
                    }
                }
            }

            if let Some((i, _)) = best {
                // `i` may point at an engagement created earlier in this same
                // tick, which has no `matched` slot; it is matched for free.
                if let Some(m) = matched.get_mut(i) {
                    *m = true;
                }
                let active = &mut self.active[i];
                active.last_tick = tick;
                active.stale_ticks = 0;
                active.location = centroid;
                active.tags = comp_tags;
                if comp.kind == "base_assault" && active.kind == "clash" {
                    active.kind = "base_assault";
                    if active.base.is_none() {
                        active.base = comp.base.clone();
                        active.base_pos = comp.base_pos;
                    }
                }
                active.supply.clear();
                active.value.clear();
                active.count.clear();
                active.units.clear();
                for &ci in &comp.clusters {
                    let cluster = &clusters[ci];
                    let player = cluster.player;
                    *active.supply.entry(player).or_default() += cluster.supply;
                    *active.value.entry(player).or_default() += cluster.value;
                    *active.count.entry(player).or_default() += cluster.count;
                    let slots = active.units.entry(player).or_default();
                    for (name, (count, supply)) in &cluster.units {
                        slots
                            .entry(name.clone())
                            .and_modify(|(c, s)| {
                                *c += count;
                                *s += supply;
                            })
                            .or_insert((*count, *supply));
                    }
                    let peak = active.peak_supply.entry(player).or_default();
                    *peak = peak.max(cluster.supply);
                    let peakv = active.peak_value.entry(player).or_default();
                    *peakv = peakv.max(cluster.value);
                }
            } else {
                let mut supply = BTreeMap::new();
                let mut value = BTreeMap::new();
                let mut count = BTreeMap::new();
                let mut units: BTreeMap<u32, BTreeMap<String, (usize, f32)>> = BTreeMap::new();
                for &ci in &comp.clusters {
                    let cluster = &clusters[ci];
                    let player = cluster.player;
                    *supply.entry(player).or_default() += cluster.supply;
                    *value.entry(player).or_default() += cluster.value;
                    *count.entry(player).or_default() += cluster.count;
                    let slots = units.entry(player).or_default();
                    for (name, (c, s)) in &cluster.units {
                        slots.entry(name.clone()).or_insert((*c, *s));
                    }
                }

                let id = self.next_id;
                self.next_id += 1;
                let location = centroid;
                let sides = comp
                    .clusters
                    .iter()
                    .map(|&ci| {
                        let cluster = &clusters[ci];
                        crate::model::EngagementSide {
                            player: cluster.player,
                            supply: cluster.supply,
                            value: cluster.value,
                            count: cluster.count,
                            units: cluster
                                .units
                                .iter()
                                .map(|(name, (c, s))| ClusterUnit {
                                    unit_type: name.clone(),
                                    count: *c,
                                    supply: *s,
                                })
                                .collect(),
                        }
                    })
                    .collect();

                events.push(EventRecord::EngagementStart(crate::model::EngagementStart {
                    id,
                    tick,
                    time_seconds: loops_to_seconds(tick),
                    kind: comp.kind.to_string(),
                    location: location.into(),
                    base: comp.base.clone(),
                    sides,
                }));

                self.active.push(ActiveEngagement {
                    id,
                    start_tick: tick,
                    last_tick: tick,
                    kind: comp.kind,
                    location,
                    base: comp.base.clone(),
                    base_pos: comp.base_pos,
                    tags: comp_tags,
                    peak_supply: supply.clone(),
                    peak_value: value.clone(),
                    supply,
                    value,
                    count,
                    units,
                    deaths: BTreeMap::new(),
                    stale_ticks: 0,
                });
            }
        }

        // End stale engagements. Base assaults get hysteresis: they stay alive
        // while their army is still near the base, even if the cluster briefly
        // drifts out of the detection radius.
        //
        // Only the engagements that existed before this tick can be stale; the
        // ones appended above started on this tick.
        let mut i = 0;
        while i < self.active.len() {
            let is_stale = {
                let active = &mut self.active[i];
                if i < existing_active && !matched[i] {
                    active.stale_ticks += 1;
                }
                if active.kind == "base_assault"
                    && !matched.get(i).copied().unwrap_or(false)
                    && let Some(base_pos) = active.base_pos
                    && active.location.distance_squared(base_pos)
                        <= (config.base_assault_radius * 2.5).powi(2)
                {
                    active.stale_ticks = 0;
                }
                active.stale_ticks > config.grace_ticks
            };            if is_stale {
                let active = self.active.remove(i);
                if let Some(end) = self.finish_engagement(active, tick, events) {
                    self.completed.push(end);
                }
            } else {
                i += 1;
            }
        }
    }

    pub fn finish(&mut self, tick: u32, events: &mut Vec<EventRecord>) {
        while let Some(active) = self.active.pop() {
            if let Some(end) = self.finish_engagement(active, tick, events) {
                self.completed.push(end);
            }
        }
    }

    fn finish_engagement(
        &self,
        active: ActiveEngagement,
        end_tick: u32,
        events: &mut Vec<EventRecord>,
    ) -> Option<crate::model::EngagementSummary> {
        let deaths: Vec<EngagementDeaths> = active.deaths.values().cloned().collect();
        let peak_supply: Vec<crate::model::EngagementPeak> = active
            .peak_supply
            .iter()
            .map(|(player, supply)| crate::model::EngagementPeak {
                player: *player,
                supply: *supply,
                value: active.peak_value.get(player).copied().unwrap_or(0.0),
            })
            .collect();

        let outcome = compute_outcome(&deaths);
        let summary = crate::model::EngagementSummary {
            id: active.id,
            start_tick: active.start_tick,
            end_tick,
            start_seconds: loops_to_seconds(active.start_tick),
            end_seconds: loops_to_seconds(end_tick),
            kind: active.kind.to_string(),
            location: active.location.into(),
            base: active.base.clone(),
            sides: active
                .supply
                .iter()
                .map(|(player, supply)| crate::model::EngagementSide {
                    player: *player,
                    supply: *supply,
                    value: active.value.get(player).copied().unwrap_or(0.0),
                    count: active.count.get(player).copied().unwrap_or(0),
                    units: active
                        .units
                        .get(player)
                        .map(|slots| {
                            slots
                                .iter()
                                .map(|(name, (c, s))| ClusterUnit {
                                    unit_type: name.clone(),
                                    count: *c,
                                    supply: *s,
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                })
                .collect(),
            deaths,
            outcome,
        };

        events.push(EventRecord::EngagementEnd(crate::model::EngagementEnd {
            id: summary.id,
            tick: end_tick,
            time_seconds: loops_to_seconds(end_tick),
            duration_seconds: loops_to_seconds(end_tick) - summary.start_seconds,
            location: summary.location,
            peak_supply,
            deaths: summary.deaths.clone(),
            outcome: summary.outcome.clone(),
        }));

        Some(summary)
    }

    fn build_components(
        &self,
        clusters: &[Cluster],
        townhalls: &[TownhallRef],
        config: &ClusterConfig,
    ) -> Vec<EngagementComponent> {
        let mut adjacent: Vec<Vec<usize>> = vec![Vec::new(); clusters.len()];
        let mut kinds: Vec<&'static str> = vec!["clash"; clusters.len()];
        let mut bases: Vec<Option<(String, Point2)>> = vec![None; clusters.len()];
        let mut engaged: Vec<bool> = vec![false; clusters.len()];

        for (i, a) in clusters.iter().enumerate() {
            // Base assault: this cluster is threatening a townhall of the other player.
            for th in townhalls {
                if th.player != a.player
                    && a.centroid.distance_squared(th.pos).sqrt() <= config.base_assault_radius
                    && a.supply >= config.engagement_min_supply
                {
                    kinds[i] = "base_assault";
                    bases[i] = Some((th.name.clone(), th.pos));
                    engaged[i] = true;
                }
            }

            for (j, b) in clusters.iter().enumerate().skip(i + 1) {
                if a.player == b.player {
                    continue;
                }
                let dist = a.centroid.distance_squared(b.centroid).sqrt();
                if dist <= config.engagement_radius
                    && a.supply >= config.engagement_min_supply
                    && b.supply >= config.engagement_min_supply
                {
                    adjacent[i].push(j);
                    adjacent[j].push(i);
                    engaged[i] = true;
                    engaged[j] = true;
                }
            }
        }

        // Only clusters that are actually fighting (a clash edge or a base
        // assault) can form engagements; isolated clusters are ignored.
        let mut visited = vec![false; clusters.len()];
        let mut components = Vec::new();
        for i in 0..clusters.len() {
            if visited[i] || !engaged[i] {
                continue;
            }
            let mut comp = EngagementComponent {
                kind: "clash",
                base: None,
                base_pos: None,
                clusters: Vec::new(),
            };
            let mut stack = vec![i];
            visited[i] = true;
            while let Some(c) = stack.pop() {
                comp.clusters.push(c);
                if kinds[c] == "base_assault" {
                    comp.kind = "base_assault";
                    if comp.base.is_none()
                        && let Some((name, pos)) = &bases[c]
                    {
                        comp.base = Some(format!("{}@{},{}", name, pos.x, pos.y));
                        comp.base_pos = Some(*pos);
                    }
                }
                for &n in &adjacent[c] {
                    if !visited[n] {
                        visited[n] = true;
                        stack.push(n);
                    }
                }
            }
            comp.clusters.sort_unstable();
            components.push(comp);
        }

        components
    }
}

fn compute_outcome(deaths: &[EngagementDeaths]) -> Option<String> {
    if deaths.is_empty() {
        return None;
    }
    let mut best: Option<(u32, f32)> = None;
    for d in deaths {
        let net = d.killed_value - d.lost_value;
        if best.is_none_or(|(_, best_net)| net > best_net) {
            best = Some((d.player, net));
        }
    }
    let (winner, net) = best.unwrap();
    if net <= 0.0 {
        Some("mutual".to_string())
    } else {
        Some(format!("player_{winner}"))
    }
}

pub fn loops_to_seconds(loop_: u32) -> f64 {
    loop_ as f64 / 22.4
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game_data::{UnitTable, UnitTypeInfo};

    fn table() -> UnitTable {
        let mut table = UnitTable::default();
        for (id, name, food, mineral, vespene) in [
            (1u32, "Zealot", 2.0, 100, 0),
            (2, "Stalker", 2.0, 125, 50),
            (3, "Carrier", 6.0, 350, 250),
        ] {
            table.units.insert(
                id,
                UnitTypeInfo {
                    name: name.to_string(),
                    mineral_cost: mineral,
                    vespene_cost: vespene,
                    food_required: food,
                    food_provided: 8.0,
                    ground_dps: 10.0,
                    air_dps: 10.0,
                },
            );
        }
        table
    }

    fn frame_unit(tag: u64, owner: u32, unit_type: u32, x: f32, y: f32) -> FrameUnit {
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
    fn clusters_group_nearby_units() {
        let table = table();
        let u1 = frame_unit(1, 1, 1, 10.0, 10.0);
        let u2 = frame_unit(2, 1, 1, 12.0, 10.0);
        let u3 = frame_unit(3, 1, 1, 40.0, 40.0);
        let units = vec![&u1, &u2, &u3];
        let clusters = cluster_combat_units(&units, &table, 12.0);
        assert_eq!(clusters.len(), 2);
        let two = clusters.iter().find(|c| c.count == 2).unwrap();
        let one = clusters.iter().find(|c| c.count == 1).unwrap();
        assert!((two.supply - 4.0).abs() < 0.01);
        assert!((one.supply - 2.0).abs() < 0.01);
    }

    #[test]
    fn confrontations_find_nearby_enemies() {
        let table = table();
        let u1 = frame_unit(1, 1, 1, 10.0, 10.0);
        let u2 = frame_unit(2, 1, 1, 12.0, 10.0);
        let u3 = frame_unit(3, 2, 2, 22.0, 10.0);
        let ours = vec![&u1, &u2];
        let theirs = vec![&u3];
        let our_clusters = cluster_combat_units(&ours, &table, 12.0);
        let enemy_clusters = cluster_combat_units(&theirs, &table, 12.0);
        let confs = confrontations(&our_clusters, &enemy_clusters, 20.0);
        assert_eq!(confs.len(), 1);
        assert!((confs[0].distance - 11.0).abs() < 0.1);
    }

    #[test]
    fn component_centroid_is_weighted_by_supply_not_scaled() {
        let table = table();
        let u1 = frame_unit(1, 1, 1, 10.0, 10.0);
        let u2 = frame_unit(2, 1, 1, 12.0, 10.0);
        let units = vec![&u1, &u2];
        let clusters = cluster_combat_units(&units, &table, 12.0);
        assert_eq!(clusters.len(), 1);
        let comp = EngagementComponent {
            clusters: vec![0],
            ..Default::default()
        };
        let centroid = comp.centroid(&clusters);
        assert!((centroid.x - 11.0).abs() < 0.1, "x={}", centroid.x);
        assert!((centroid.y - 10.0).abs() < 0.1, "y={}", centroid.y);
    }

    #[test]
    fn engagement_tracks_and_ends() {
        let table = table();
        let u1 = frame_unit(1, 1, 1, 10.0, 10.0);
        let u2 = frame_unit(2, 1, 1, 12.0, 10.0);
        let u3 = frame_unit(3, 1, 1, 11.0, 11.0);
        let u4 = frame_unit(4, 2, 2, 16.0, 10.0);
        let u5 = frame_unit(5, 2, 2, 18.0, 10.0);
        let u6 = frame_unit(6, 2, 2, 17.0, 11.0);
        let ours = vec![&u1, &u2, &u3];
        let theirs = vec![&u4, &u5, &u6];
        let config = ClusterConfig {
            engagement_min_supply: 4.0,
            grace_ticks: 2,
            ..Default::default()
        };

        let mut tracker = EngagementTracker::new();
        let mut events = Vec::new();

        let oc = cluster_combat_units(&ours, &table, 12.0);
        let ec = cluster_combat_units(&theirs, &table, 12.0);
        let mut all = oc;
        all.extend(ec);

        let deaths: [(u32, u64, f32); 0] = [];
        tracker.on_tick(100, &all, &[], &deaths, &config, &mut events);
        assert!(events
            .iter()
            .any(|e| matches!(e, EventRecord::EngagementStart(_))));

        tracker.on_tick(104, &all, &[], &deaths, &config, &mut events);
        tracker.on_tick(108, &[], &[], &deaths, &config, &mut events);
        tracker.on_tick(112, &[], &[], &deaths, &config, &mut events);
        tracker.on_tick(116, &[], &[], &deaths, &config, &mut events);
        assert!(events
            .iter()
            .any(|e| matches!(e, EventRecord::EngagementEnd(_))));
        assert_eq!(tracker.completed.len(), 1);
    }
}
