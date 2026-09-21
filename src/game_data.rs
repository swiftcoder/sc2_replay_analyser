use std::collections::HashMap;

use sc2_proto::data::Weapon_TargetType;
use sc2_proto::sc2api::Response;

#[derive(Debug, Clone)]
pub struct UnitTypeInfo {
    pub name: String,
    pub mineral_cost: u32,
    pub vespene_cost: u32,
    pub food_required: f32,
    pub food_provided: f32,
    pub ground_dps: f32,
    pub air_dps: f32,
}

impl UnitTypeInfo {
    pub fn value(&self) -> f32 {
        (self.mineral_cost + self.vespene_cost) as f32
    }
}

#[derive(Debug, Default)]
pub struct UnitTable {
    pub units: HashMap<u32, UnitTypeInfo>,
}

impl UnitTable {
    pub fn get(&self, id: u32) -> Option<&UnitTypeInfo> {
        self.units.get(&id)
    }

    pub fn name_of(&self, id: u32) -> String {
        self.units
            .get(&id)
            .map(|info| info.name.clone())
            .unwrap_or_else(|| format!("Unknown({id})"))
    }
}

/// Researchable upgrades keyed by *ability id*, because a structure's order
/// (which is how we see research in progress) carries the research ability,
/// not the upgrade id.
#[derive(Debug, Default)]
pub struct UpgradeTable {
    pub by_ability: HashMap<u32, String>,
}

impl UpgradeTable {
    pub fn name_by_ability(&self, ability_id: u32) -> Option<&str> {
        self.by_ability.get(&ability_id).map(|s| s.as_str())
    }
}

fn weapon_dps(weapon: &sc2_proto::data::Weapon) -> f32 {
    if weapon.get_speed() <= 0.0 {
        return 0.0;
    }
    weapon.get_damage() * weapon.get_attacks() as f32 / weapon.get_speed()
}

/// Fetch the unit type table from the SC2 game data and fold it into a compact
/// structure keyed by unit id.
pub fn fetch_unit_table(res: &Response) -> UnitTable {
    let data = res.get_data();
    let mut units = HashMap::new();

    for unit in data.get_units() {
        let mut ground_dps = 0.0f32;
        let mut air_dps = 0.0f32;
        for weapon in unit.get_weapons() {
            let dps = weapon_dps(weapon);
            match weapon.get_field_type() {
                Weapon_TargetType::Ground => ground_dps = ground_dps.max(dps),
                Weapon_TargetType::Air => air_dps = air_dps.max(dps),
                Weapon_TargetType::Any => {
                    ground_dps = ground_dps.max(dps);
                    air_dps = air_dps.max(dps);
                }
            }
        }

        units.insert(
            unit.get_unit_id(),
            UnitTypeInfo {
                name: unit.get_name().to_string(),
                mineral_cost: unit.get_mineral_cost(),
                vespene_cost: unit.get_vespene_cost(),
                food_required: unit.get_food_required(),
                food_provided: unit.get_food_provided(),
                ground_dps,
                air_dps,
            },
        );
    }

    UnitTable { units }
}

/// Fetch the upgrade table from the game data, keyed by research ability id.
pub fn fetch_upgrade_table(res: &Response) -> UpgradeTable {
    let data = res.get_data();
    let mut by_ability = HashMap::new();
    for upgrade in data.get_upgrades() {
        let ability_id = upgrade.get_ability_id();
        if ability_id > 0 {
            by_ability.insert(ability_id, upgrade.get_name().to_string());
        }
    }
    UpgradeTable { by_ability }
}
