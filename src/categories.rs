//! Unit classification for replay analysis.
//!
//! Classification is done by stable unit *name* (from the game data response)
//! rather than by the `UnitTypeId` enum, so the analyzer is robust to ladder
//! game versions that may be newer than the one `sc2-proto` was generated for.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitClass {
    /// Units that fight: army value, supply and cluster strength come from these.
    Combat,
    /// Non-combat units that support the army (overlord, observer, medivac...).
    Support,
    /// Worker units (SCV, Drone, Probe).
    Worker,
    /// Structures.
    Structure,
    /// Anything else (larva, eggs, mules, creep tumours, minerals...).
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructureKind {
    /// Produces units / is a townhall.
    Townhall,
    Production,
    Economy,
    Defense,
    Research,
    Supply,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProductionInfo {
    /// 1, 2 or 3.
    pub tier: u8,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct CombatFlags {
    pub air: bool,
    pub stealth: bool,
    pub tier3: bool,
}

const WORKERS: &[&str] = &["SCV", "Drone", "Probe"];

const TOWNHALLS: &[&str] = &[
    "CommandCenter",
    "CommandCenterFlying",
    "OrbitalCommand",
    "OrbitalCommandFlying",
    "PlanetaryFortress",
    "Hatchery",
    "Lair",
    "Hive",
    "Nexus",
];

/// Production structures mapped to the highest tier of unit they can build.
/// Zerg "production" is hatchery-larvae based, so the extra structures are
/// treated as tier unlocks rather than throughput.
const PRODUCTION_STRUCTURES: &[(&str, u8)] = &[
    ("Barracks", 2),
    ("BarracksFlying", 2),
    ("Factory", 3),
    ("FactoryFlying", 3),
    ("Starport", 3),
    ("StarportFlying", 3),
    ("Gateway", 2),
    ("WarpGate", 2),
    ("RoboticsFacility", 3),
    ("Stargate", 3),
    ("SpawningPool", 1),
    ("RoachWarren", 2),
    ("BanelingNest", 2),
    ("HydraliskDen", 2),
    ("LurkerDenMP", 2),
    ("InfestationPit", 2),
    ("Spire", 2),
    ("GreaterSpire", 3),
    ("UltraliskCavern", 3),
];

const REACTOR_ADDONS: &[&str] = &[
    "Reactor",
    "BarracksReactor",
    "FactoryReactor",
    "StarportReactor",
];

/// Buildings and units that provide supply (Pylon/SupplyDepot/Overlord).
const SUPPLY_PROVIDERS: &[&str] = &[
    "SupplyDepot",
    "SupplyDepotLowered",
    "SupplyDepotDrop",
    "Pylon",
    "Overlord",
    "OverlordCocoon",
    "OverlordTransport",
];

const SUPPLY_STRUCTURES: &[&str] = &["SupplyDepot", "SupplyDepotLowered", "SupplyDepotDrop", "Pylon"];

const GAS_STRUCTURES: &[&str] = &[
    "Refinery",
    "Assimilator",
    "Extractor",
    "RefineryRich",
    "AssimilatorRich",
    "ExtractorRich",
];

const DEFENSE_STRUCTURES: &[&str] = &[
    "Bunker",
    "MissileTurret",
    "SensorTower",
    "PhotonCannon",
    "SpineCrawler",
    "SporeCrawler",
    "ShieldBattery",
];

const RESEARCH_STRUCTURES: &[&str] = &[
    "EngineeringBay",
    "Armory",
    "FusionCore",
    "GhostAcademy",
    "Forge",
    "CyberneticsCore",
    "TwilightCouncil",
    "RoboticsBay",
    "FleetBeacon",
    "TemplarArchive",
    "DarkShrine",
    "EvolutionChamber",
    "NydusNetwork",
    "NydusWorm",
];

/// Support units: provide vision, transport, detection or healing rather than
/// raw damage.
const SUPPORT_UNITS: &[&str] = &[
    "Overlord",
    "OverlordTransport",
    "Overseer",
    "Observer",
    "ObserverSiegeMode",
    "WarpPrism",
    "WarpPrismPhasing",
    "Medivac",
    "Raven",
    "Queen",
    "Viper",
    "Mothership",
];

/// Stealth-capable combat units.
const STEALTH_UNITS: &[&str] = &[
    "DarkTemplar",
    "Oracle",
    "Banshee",
    "Ghost",
    "WidowMine",
    "WidowMineBurrowed",
    "LurkerMP",
    "LurkerMPBurrowed",
    "Infestor",
    "InfestorBurrowed",
];

/// Tier 3 combat units (heavy, expensive, lategame).
const TIER3_UNITS: &[&str] = &[
    "Battlecruiser",
    "Thor",
    "ThorAP",
    "Carrier",
    "Tempest",
    "Mothership",
    "Colossus",
    "Archon",
    "Ultralisk",
    "UltraliskBurrowed",
    "BroodLord",
    "Viper",
];

const AIR_UNITS: &[&str] = &[
    "VikingFighter",
    "Liberator",
    "LiberatorAG",
    "Banshee",
    "Battlecruiser",
    "Raven",
    "Medivac",
    "Phoenix",
    "VoidRay",
    "Oracle",
    "Carrier",
    "Tempest",
    "Mothership",
    "Mutalisk",
    "Corruptor",
    "BroodLord",
    "Viper",
    "Overlord",
    "OverlordTransport",
    "Overseer",
    "LocustMPFlying",
    "Interceptor",
];

pub struct UnitClassifier;

impl UnitClassifier {
    pub fn is_worker(name: &str) -> bool {
        WORKERS.contains(&name)
    }

    pub fn is_townhall(name: &str) -> bool {
        TOWNHALLS.contains(&name)
    }

    pub fn is_structure_name(name: &str) -> bool {
        PRODUCTION_STRUCTURES.iter().any(|(n, _)| *n == name)
            || SUPPLY_STRUCTURES.contains(&name)
            || GAS_STRUCTURES.contains(&name)
            || DEFENSE_STRUCTURES.contains(&name)
            || RESEARCH_STRUCTURES.contains(&name)
            || TOWNHALLS.contains(&name)
            || REACTOR_ADDONS.contains(&name)
            || matches!(
                name,
                "TechLab"
                    | "BarracksTechLab"
                    | "FactoryTechLab"
                    | "StarportTechLab"
                    | "AutoTurret"
                    | "CreepTumor"
                    | "CreepTumorBurrowed"
                    | "CreepTumorQueen"
            )
    }

    pub fn is_supply_provider(name: &str) -> bool {
        SUPPLY_PROVIDERS.contains(&name)
    }

    pub fn production_info(name: &str) -> Option<ProductionInfo> {
        let (_, tier) = PRODUCTION_STRUCTURES.iter().find(|(n, _)| *n == name)?;
        Some(ProductionInfo { tier: *tier })
    }

    pub fn structure_kind(name: &str) -> StructureKind {
        if Self::is_townhall(name) {
            StructureKind::Townhall
        } else if Self::production_info(name).is_some() {
            StructureKind::Production
        } else if SUPPLY_STRUCTURES.contains(&name) {
            StructureKind::Supply
        } else if GAS_STRUCTURES.contains(&name) {
            StructureKind::Economy
        } else if DEFENSE_STRUCTURES.contains(&name) {
            StructureKind::Defense
        } else if RESEARCH_STRUCTURES.contains(&name) {
            StructureKind::Research
        } else {
            StructureKind::Other
        }
    }

    pub fn class(name: &str) -> UnitClass {
        if Self::is_worker(name) {
            UnitClass::Worker
        } else if Self::is_structure_name(name) {
            UnitClass::Structure
        } else if SUPPORT_UNITS.contains(&name) {
            UnitClass::Support
        } else if is_combat_unit_name(name) {
            UnitClass::Combat
        } else {
            UnitClass::Other
        }
    }

    pub fn combat_flags(name: &str) -> CombatFlags {
        CombatFlags {
            air: AIR_UNITS.contains(&name),
            stealth: STEALTH_UNITS.contains(&name),
            tier3: TIER3_UNITS.contains(&name),
        }
    }
}

fn is_combat_unit_name(name: &str) -> bool {
    use self::ids_lite::COMBAT_UNITS;
    COMBAT_UNITS.contains(&name) || AIR_UNITS.contains(&name) || TIER3_UNITS.contains(&name)
}

/// Names of units that deal damage (ground combat primarily).
pub mod ids_lite {
    pub const COMBAT_UNITS: &[&str] = &[
        // Terran
        "Marine",
        "Marauder",
        "Reaper",
        "Ghost",
        "Hellion",
        "HellionTank",
        "SiegeTank",
        "SiegeTankSieged",
        "Cyclone",
        "WidowMine",
        "WidowMineBurrowed",
        "Thor",
        "ThorAP",
        "VikingFighter",
        "VikingAssault",
        "Liberator",
        "LiberatorAG",
        "Raven",
        "Banshee",
        "Battlecruiser",
        // Protoss
        "Zealot",
        "Stalker",
        "Sentry",
        "Adept",
        "AdeptPhaseShift",
        "HighTemplar",
        "DarkTemplar",
        "Immortal",
        "Colossus",
        "Disruptor",
        "Archon",
        "Phoenix",
        "VoidRay",
        "Oracle",
        "Carrier",
        "Tempest",
        "Mothership",
        // Zerg
        "Zergling",
        "ZerglingBurrowed",
        "Baneling",
        "BanelingBurrowed",
        "Roach",
        "RoachBurrowed",
        "Ravager",
        "RavagerBurrowed",
        "Hydralisk",
        "HydraliskBurrowed",
        "LurkerMP",
        "LurkerMPBurrowed",
        "Infestor",
        "InfestorBurrowed",
        "SwarmHostMP",
        "SwarmHostBurrowedMP",
        "Ultralisk",
        "UltraliskBurrowed",
        "BroodLord",
        "Viper",
        "Mutalisk",
        "Corruptor",
        "LocustMP",
        "LocustMPFlying",
        "Broodling",
        "InfestorTerran",
        "InfestorTerranBurrowed",
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_and_townhall_classification() {
        assert_eq!(UnitClassifier::class("SCV"), UnitClass::Worker);
        assert_eq!(UnitClassifier::class("Probe"), UnitClass::Worker);
        assert_eq!(UnitClassifier::class("Drone"), UnitClass::Worker);
        assert!(UnitClassifier::is_townhall("Nexus"));
        assert!(UnitClassifier::is_townhall("Hatchery"));
        assert!(UnitClassifier::is_townhall("OrbitalCommand"));
    }

    #[test]
    fn production_tiers() {
        assert_eq!(UnitClassifier::production_info("Gateway").unwrap().tier, 2);
        assert_eq!(
            UnitClassifier::production_info("Barracks").unwrap().tier,
            2
        );
        assert_eq!(
            UnitClassifier::production_info("Stargate").unwrap().tier,
            3
        );
        assert_eq!(
            UnitClassifier::production_info("SpawningPool").unwrap().tier,
            1
        );
        assert_eq!(
            UnitClassifier::production_info("UltraliskCavern").unwrap().tier,
            3
        );
        assert!(UnitClassifier::production_info("StarportReactor").is_none());
    }

    #[test]
    fn combat_flags() {
        assert!(UnitClassifier::combat_flags("Carrier").air);
        assert!(UnitClassifier::combat_flags("Carrier").tier3);
        assert!(UnitClassifier::combat_flags("DarkTemplar").stealth);
        assert!(UnitClassifier::combat_flags("Banshee").stealth);
        assert!(UnitClassifier::combat_flags("Ultralisk").tier3);
        assert!(!UnitClassifier::combat_flags("Zealot").air);
    }

    #[test]
    fn support_and_combat() {
        assert_eq!(UnitClassifier::class("Overlord"), UnitClass::Support);
        assert_eq!(UnitClassifier::class("Observer"), UnitClass::Support);
        assert_eq!(UnitClassifier::class("Medivac"), UnitClass::Support);
        assert_eq!(UnitClassifier::class("Zealot"), UnitClass::Combat);
        assert_eq!(UnitClassifier::class("Marine"), UnitClass::Combat);
        assert_eq!(UnitClassifier::class("Larva"), UnitClass::Other);
    }
}
